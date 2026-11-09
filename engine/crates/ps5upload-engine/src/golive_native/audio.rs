//! The sound for Go live without ffmpeg: the console's Opus decoded and encoded to AAC-LC (no
//! streaming site takes Opus over RTMP) by the device's own codecs, behind two small traits:
//! AudioToolbox on iPhone, iPad and Mac (apple.rs), MediaCodec on Android (android.rs). Tests
//! use a pretend pair (`fake`). No sound is ever played.
//!
//! `AudioTrack` does the rest, the same everywhere: the console's 10 ms packets put back in
//! order (a window of two, as the TS feed does), their times from the same smoothed clock as
//! the video (rp_live_ts.rs's PtsClock, so sound and picture share one origin and never drift
//! apart), up to 200 ms of lost packets filled with silence so the AAC timeline stays whole,
//! and each AAC frame's time from the samples before it. Where the samples and the clock part
//! by more than RESYNC (a long loss, the clocks drifting over hours) the timeline jumps to the
//! clock.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, OnceLock};

use crate::rp_live_ts::{PtsClock, Unwrap};

pub(crate) const RATE: u32 = 48_000;
/// Samples per channel in one of the console's Opus packets (10 ms).
pub(crate) const OPUS_FRAME: usize = 480;
/// Samples per channel in an AAC-LC frame.
pub(crate) const AAC_FRAME: usize = 1024;
/// 90 kHz ticks per sample.
const TICKS_PER_SAMPLE: f64 = 90_000.0 / RATE as f64;
/// FLV's time unit in 90 kHz ticks.
const TICKS_PER_MS: u64 = 90;
/// Lost packets up to this many in a row become silence; more and the timeline jumps.
const GAP_FILL_MAX: i64 = 20;
/// The samples' time this far from the clock (80 ms) moves the timeline to the clock.
const RESYNC_TICKS: f64 = 7_200.0;
/// Packets held to put a late one back in place.
const REORDER: i64 = 2;
/// The decoder failing this many packets in a row is broken, not one bad packet.
const MAX_BAD_PACKETS: u32 = 50;

/// Opus to PCM.
pub(crate) trait OpusDecoder: Send {
    /// One packet's samples (16-bit, interleaved), appended to `out`.
    fn decode(&mut self, packet: &[u8], out: &mut Vec<i16>) -> Result<(), String>;
}

/// PCM to AAC-LC.
pub(crate) trait AacEncoder: Send {
    /// Takes any number of interleaved samples; appends each whole raw AAC frame (no ADTS)
    /// that is ready.
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<Vec<u8>>) -> Result<(), String>;
    /// Priming samples the encoder puts ahead of the first input sample.
    fn delay(&self) -> u32 {
        0
    }
}

/// A device's pair of codecs.
pub(crate) trait AudioCodecs: Send + Sync {
    fn name(&self) -> &'static str;
    /// A decoder and an encoder for `channels` at 48 kHz, the encoder at `kbps`.
    #[allow(clippy::type_complexity)]
    fn open(
        &self,
        channels: u8,
        kbps: u32,
    ) -> Result<(Box<dyn OpusDecoder>, Box<dyn AacEncoder>), String>;
}

/// This device's codecs, when it has them.
pub(crate) fn platform() -> Option<Arc<dyn AudioCodecs>> {
    #[cfg(target_vendor = "apple")]
    {
        super::apple::codecs()
    }
    #[cfg(target_os = "android")]
    {
        super::android::codecs()
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
    {
        None
    }
}

/// The device's codecs open (checked once): Go live without ffmpeg can run here.
pub(crate) fn available() -> Option<Arc<dyn AudioCodecs>> {
    static OK: OnceLock<Option<Arc<dyn AudioCodecs>>> = OnceLock::new();
    OK.get_or_init(|| {
        let c = platform()?;
        match c.open(2, 160) {
            Ok(_) => Some(c),
            Err(e) => {
                crate::log_at!(
                    Warn,
                    "golive",
                    "golive: the device's sound converter ({}) won't open: {e}",
                    c.name()
                );
                None
            }
        }
    })
    .clone()
}

/// One AAC frame and its time (90 kHz ticks since the feed's first keyframe).
pub(crate) type AacOut = (u64, Vec<u8>);

pub(crate) struct AudioTrack {
    dec: Box<dyn OpusDecoder>,
    enc: Box<dyn AacEncoder>,
    channels: usize,
    idx: Unwrap,
    clock: PtsClock,
    wait: BTreeMap<i64, (Vec<u8>, f64)>,
    last: Option<i64>,
    /// Samples per channel handed to the encoder.
    fed: u64,
    /// (sample, ticks): where the sample count meets the clock.
    anchors: VecDeque<(u64, f64)>,
    frames_out: u64,
    last_out: Option<u64>,
    bad: u32,
    pcm: Vec<i16>,
    aac: Vec<Vec<u8>>,
    /// Packets lost (filled or skipped), for the log.
    pub lost: u64,
}

impl AudioTrack {
    pub(crate) fn new(dec: Box<dyn OpusDecoder>, enc: Box<dyn AacEncoder>, channels: u8) -> Self {
        AudioTrack {
            dec,
            enc,
            channels: channels.clamp(1, 2) as usize,
            idx: Unwrap::default(),
            clock: PtsClock::new(900, 0.01),
            wait: BTreeMap::new(),
            last: None,
            fed: 0,
            anchors: VecDeque::new(),
            frames_out: 0,
            last_out: None,
            bad: 0,
            pcm: Vec::new(),
            aac: Vec::new(),
            lost: 0,
        }
    }

    /// The sample timeline's time at sample `p`.
    fn ticks_at(&self, p: u64) -> f64 {
        let a = self
            .anchors
            .iter()
            .rev()
            .find(|a| a.0 <= p)
            .or(self.anchors.front())
            .copied()
            .unwrap_or((0, 0.0));
        a.1 + (p as f64 - a.0 as f64) * TICKS_PER_SAMPLE
    }

    /// One Opus packet (`index`: the console's audio frame index) that arrived `arrival`
    /// ticks after the origin. Gives the AAC frames that are ready.
    pub(crate) fn push(
        &mut self,
        opus: &[u8],
        index: u16,
        arrival: f64,
    ) -> Result<Vec<AacOut>, String> {
        let mut out = Vec::new();
        if opus.is_empty() {
            return Ok(out);
        }
        let ext = self.idx.next(index);
        if self.last.is_some_and(|l| ext <= l) || self.wait.contains_key(&ext) {
            return Ok(out); // too late, or a repeat
        }
        self.wait.insert(ext, (opus.to_vec(), arrival));
        let newest = *self.wait.keys().next_back().unwrap_or(&ext);
        while let Some(first) = self.wait.keys().next().copied() {
            let in_order = self.last.is_none_or(|l| first == l + 1);
            if !in_order && first > newest - REORDER {
                break;
            }
            let (pkt, at) = self.wait.remove(&first).unwrap_or_default();
            let gap = self.last.map_or(0, |l| first - l - 1);
            self.last = Some(first);
            let pts = self.clock.pts(first, at) as f64;
            self.pcm.clear();
            if gap > 0 {
                self.lost += gap as u64;
                if gap <= GAP_FILL_MAX {
                    self.pcm
                        .resize(gap as usize * OPUS_FRAME * self.channels, 0);
                }
            }
            // Where this packet's first sample falls on the timeline, after any silence.
            let fill = (self.pcm.len() / self.channels) as u64;
            let here = self.fed + fill;
            if self.anchors.is_empty() || (pts - self.ticks_at(here)).abs() > RESYNC_TICKS {
                self.anchors.push_back((here, pts));
            }
            let before = self.pcm.len();
            match self.dec.decode(&pkt, &mut self.pcm) {
                Ok(()) => self.bad = 0,
                Err(e) => {
                    self.bad += 1;
                    if self.bad >= MAX_BAD_PACKETS {
                        return Err(format!("the sound decoder failed: {e}"));
                    }
                    self.pcm.truncate(before);
                    self.pcm.resize(before + OPUS_FRAME * self.channels, 0);
                }
            }
            // Whole frames only (a decoder that gave an odd count is padded).
            let rem = self.pcm.len() % self.channels;
            if rem != 0 {
                self.pcm.resize(self.pcm.len() + self.channels - rem, 0);
            }
            self.fed += (self.pcm.len() / self.channels) as u64;
            self.aac.clear();
            self.enc
                .encode(&self.pcm, &mut self.aac)
                .map_err(|e| format!("the sound encoder failed: {e}"))?;
            let frames = std::mem::take(&mut self.aac);
            for f in frames {
                let p =
                    (self.frames_out * AAC_FRAME as u64).saturating_sub(self.enc.delay() as u64);
                self.frames_out += 1;
                let mut t = self.ticks_at(p).max(0.0).round() as u64;
                // FLV times are whole milliseconds: each frame at least one after the last.
                if let Some(l) = self.last_out {
                    t = t.max(l + TICKS_PER_MS);
                }
                self.last_out = Some(t);
                while self.anchors.len() > 1 && self.anchors[1].0 <= p {
                    self.anchors.pop_front();
                }
                out.push((t, f));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A pretend decoder and encoder: a packet decodes to 480 samples whose value is the
    //! packet's first byte; a frame is made of each 1024 samples and names its first sample.

    use super::*;

    pub(crate) struct FakeCodecs {
        pub fail_open: bool,
    }

    struct Dec {
        channels: usize,
    }

    impl OpusDecoder for Dec {
        fn decode(&mut self, packet: &[u8], out: &mut Vec<i16>) -> Result<(), String> {
            if packet[0] == 0xff {
                return Err("a bad packet".into());
            }
            out.extend(std::iter::repeat_n(
                packet[0] as i16,
                OPUS_FRAME * self.channels,
            ));
            Ok(())
        }
    }

    struct Enc {
        channels: usize,
        buf: Vec<i16>,
        delay: u32,
    }

    impl AacEncoder for Enc {
        fn encode(&mut self, pcm: &[i16], out: &mut Vec<Vec<u8>>) -> Result<(), String> {
            self.buf.extend_from_slice(pcm);
            let n = AAC_FRAME * self.channels;
            while self.buf.len() >= n {
                let f: Vec<i16> = self.buf.drain(..n).collect();
                out.push(vec![0x21, f[0] as u8]);
            }
            Ok(())
        }
        fn delay(&self) -> u32 {
            self.delay
        }
    }

    impl AudioCodecs for FakeCodecs {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn open(
            &self,
            channels: u8,
            _kbps: u32,
        ) -> Result<(Box<dyn OpusDecoder>, Box<dyn AacEncoder>), String> {
            if self.fail_open {
                return Err("no codec here".into());
            }
            let ch = channels as usize;
            Ok((
                Box::new(Dec { channels: ch }),
                Box::new(Enc {
                    channels: ch,
                    buf: Vec::new(),
                    delay: 0,
                }),
            ))
        }
    }

    pub(crate) fn track(channels: u8, delay: u32) -> AudioTrack {
        let ch = channels as usize;
        AudioTrack::new(
            Box::new(Dec { channels: ch }),
            Box::new(Enc {
                channels: ch,
                buf: Vec::new(),
                delay,
            }),
            channels,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packets 0.. at 10 ms each, arriving on time; returns every frame out.
    fn feed(t: &mut AudioTrack, order: &[u16], value: impl Fn(u16) -> u8) -> Vec<AacOut> {
        let mut out = Vec::new();
        for &i in order {
            out.extend(t.push(&[value(i)], i, i as f64 * 900.0).unwrap());
        }
        out
    }

    #[test]
    fn frames_come_every_1024_samples_on_the_packets_clock() {
        let mut t = fake::track(2, 0);
        let order: Vec<u16> = (0..100).collect();
        let out = feed(&mut t, &order, |_| 1);
        // 100 packets × 480 = 48000 samples: 46 whole frames.
        assert_eq!(out.len(), 46);
        for (k, (ts, f)) in out.iter().enumerate() {
            let want = (k as f64 * 1024.0 * TICKS_PER_SAMPLE).round() as u64;
            assert_eq!(*ts, want, "frame {k}");
            assert_eq!(f, &vec![0x21, 1]);
        }
    }

    #[test]
    fn a_late_packet_goes_back_in_place_and_repeats_are_dropped() {
        let mut t = fake::track(1, 0);
        // Packet value = its index: the first sample of each frame tells which packet it came from.
        let out = feed(&mut t, &[0, 1, 3, 2, 2, 4, 5, 6, 7, 8, 9, 10], |i| i as u8);
        // Frame 0 starts in packet 0, frame 1 at sample 1024, in packet 2 (960..1440).
        assert_eq!(out[0].1[1], 0);
        assert_eq!(out[1].1[1], 2);
        assert_eq!(t.lost, 0);
    }

    #[test]
    fn short_losses_become_silence_and_keep_the_timeline_whole() {
        let mut t = fake::track(1, 0);
        // Packets 5-9 lost (50 ms): silence holds their place.
        let order: Vec<u16> = (0..5).chain(10..40).collect();
        let out = feed(&mut t, &order, |_| 7);
        assert_eq!(t.lost, 5);
        // 40 packets' worth of samples, lost ones included: 18 frames.
        assert_eq!(out.len(), 18);
        for (k, (ts, _)) in out.iter().enumerate() {
            assert_eq!(*ts, (k as f64 * 1024.0 * TICKS_PER_SAMPLE).round() as u64);
        }
        // Frame 2 starts at sample 2048, inside packet 4 (1920..2400): the console's sound.
        assert_eq!(out[2].1[1], 7);
        // Frame 3 starts at 3072: packet 6, lost: silence.
        assert_eq!(out[3].1[1], 0);
    }

    #[test]
    fn a_long_loss_moves_the_timeline_to_the_clock() {
        let mut t = fake::track(1, 0);
        // A second lost (100 packets): no silence, the next sound is placed by its clock.
        let order: Vec<u16> = (0..10).chain(110..140).collect();
        let out = feed(&mut t, &order, |_| 3);
        let first_after = out
            .iter()
            .find(|(ts, _)| *ts > 10 * 900)
            .expect("frames after the loss");
        // The first frame that starts after the loss lands near packet 110's time (99000).
        assert!(
            (first_after.0 as i64 - 99_000).abs() < 2_000,
            "{}",
            first_after.0
        );
        // Times only go forward.
        assert!(out.windows(2).all(|w| w[1].0 > w[0].0));
    }

    #[test]
    fn the_encoders_priming_is_taken_off_the_times() {
        let mut t = fake::track(2, 2112);
        let order: Vec<u16> = (0..40).collect();
        let out = feed(&mut t, &order, |_| 1);
        // Frames 0-2 start inside the priming (before the first input sample): they sit at
        // the start, a millisecond apart. Frame 3 starts 960 samples into the sound.
        assert_eq!(out[0].0, 0);
        assert_eq!(out[1].0, 90);
        assert_eq!(out[2].0, 180);
        assert_eq!(
            out[3].0,
            ((3 * 1024 - 2112) as f64 * TICKS_PER_SAMPLE).round() as u64
        );
    }

    #[test]
    fn index_wraps_and_a_broken_decoder_ends_the_track() {
        let mut t = fake::track(1, 0);
        let order: Vec<u16> = (65_500..=65_535).chain(0..30).collect();
        let mut out = Vec::new();
        for (n, &i) in order.iter().enumerate() {
            out.extend(t.push(&[1], i, n as f64 * 900.0).unwrap());
        }
        assert_eq!(t.lost, 0);
        assert!(out.windows(2).all(|w| w[1].0 > w[0].0));

        let mut t = fake::track(1, 0);
        let mut err = None;
        for i in 0..100u16 {
            if let Err(e) = t.push(&[0xff], i, i as f64 * 900.0) {
                err = Some(e);
                break;
            }
        }
        assert!(err.is_some_and(|e| e.contains("decoder")));
    }
}
