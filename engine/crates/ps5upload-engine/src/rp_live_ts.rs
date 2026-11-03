//! MPEG-TS for the live feed (rp_live.rs, #635): the console's own H.264/HEVC access units and
//! Opus packets in a transport stream that ffmpeg (and so OBS's Media Source) opens. Nothing is
//! re-encoded: this only wraps what the console sent.
//!
//! A small muxer of our own rather than a crate: it writes one program with two streams and a
//! fixed layout, and the parts that matter here (Opus's descriptors and control headers, PCR on
//! a video PID whose frames may be held back) are not what the general-purpose crates cover.
//!
//! * PAT (PID 0) → PMT (PID 0x1000) → video on 0x100 (stream_type 0x1B H.264, 0x24 HEVC; it
//!   carries the PCR) and Opus on 0x101: stream_type 0x06 with a registration descriptor
//!   "Opus" and the DVB extension descriptor 0x7F / 0x80 with the channel config, as ffmpeg's
//!   mpegtsenc writes them and its mpegts demuxer reads them.
//! * PAT/PMT go out before every keyframe and at least every 100 ms.
//! * Video PES: stream_id 0xE0, unbounded length, PTS only (no B-frames: DTS = PTS). Every
//!   access unit starts with an access unit delimiter (added when the console's has none), and
//!   a keyframe carries the cached parameter sets (SPS/PPS, plus VPS for HEVC) right after it
//!   unless it already has its own. The first TS packet of a keyframe is marked random access.
//! * Opus PES: stream_id 0xBD (private stream 1), one packet each, behind the Opus control
//!   header (0x7FE0 + the packet size in 255-steps) as ffmpeg's demuxer and parser expect.
//! * PCR (on the first packet of each video PES) runs PCR_DELAY behind the PTS. While video
//!   is held back (after a loss, until the next keyframe), audio keeps the PCR going with
//!   adaptation-only packets on the video PID, so a player's clock never stalls.
//!
//! Timestamps (90 kHz). The console sends no timestamps: video has a 16-bit frame index (one
//! per frame at the session's fps) and audio a 16-bit frame index (one per 10 ms Opus packet).
//! Both clocks start at the feed's first keyframe (its arrival is time 0, shown at PTS_BASE),
//! so audio and video share one origin. Each stream's PTS is its index paced at its nominal
//! rate (frame_index × 1/fps, audio index × 10 ms), pulled gently toward the time the engine
//! had the frame: `pts = predicted + (arrival − predicted) × alpha`, alpha 0.1 for video
//! (network jitter of ±10 ms moves a frame by ±1 ms) and 0.01 for audio (sound stays evenly
//! spaced). The pull keeps both streams on the engine's clock over hours, so the console's and
//! this computer's clocks drifting apart cannot drift audio and video apart; a console that
//! sends fewer frames than its nominal rate settles at a constant offset with the right
//! spacing. A jump of more than RESYNC (a stall, a pause) restarts from the arrival time.
//! The result is put on the stream's own grid (whole frames: 1500 ticks at 60 fps, 900 for a
//! 10 ms Opus packet), each on the next free slot after the last, so PTS always moves forward,
//! spacing is exact (a player that assumes a constant rate sees one; a held or lost frame is a
//! gap of whole frames) and jitter never makes two frames share a time. 16-bit indexes are
//! unwrapped into a running count. Audio is reordered in a window of two frames (a lost
//! packet's Opus frames arrive late, in the next packet's FEC); anything older is dropped.

use std::collections::BTreeMap;

pub const TS_PACKET: usize = 188;
const PID_PAT: u16 = 0;
const PID_PMT: u16 = 0x1000;
const PID_VIDEO: u16 = 0x100;
const PID_AUDIO: u16 = 0x101;
const STREAM_TYPE_H264: u8 = 0x1b;
const STREAM_TYPE_HEVC: u8 = 0x24;
const STREAM_TYPE_PRIVATE: u8 = 0x06;
/// The first frame's PTS: one second in, leaving room for the PCR below it.
const PTS_BASE: u64 = 90_000;
/// The PCR runs this far behind the PTS (a player's decode buffer).
const PCR_DELAY: u64 = 9_000;
/// PAT/PMT at least this often.
const PSI_EVERY: u64 = 9_000;
/// A PCR at least this often while video is held back.
const PCR_EVERY: u64 = 3_600;
/// A timestamp further than this from the arrival time starts the clock over (0.5 s).
const RESYNC: f64 = 45_000.0;
/// Audio frames held to put a late one back in place.
const AUDIO_REORDER: i64 = 2;
const PTS_MASK: u64 = (1 << 33) - 1;

/// A 16-bit wrapping index unwrapped into a running count.
#[derive(Default, Debug)]
struct Unwrap {
    last: Option<(u16, i64)>,
}

impl Unwrap {
    fn next(&mut self, idx: u16) -> i64 {
        let ext = match self.last {
            None => idx as i64,
            Some((raw, ext)) => ext + idx.wrapping_sub(raw) as i16 as i64,
        };
        if self.last.is_none_or(|(_, e)| ext > e) {
            self.last = Some((idx, ext));
        }
        ext
    }
}

/// One stream's presentation clock (see the module comment).
#[derive(Debug)]
struct PtsClock {
    /// One index step in 90 kHz ticks.
    step: f64,
    alpha: f64,
    /// The last frame and its smoothed time.
    last: Option<(i64, f64)>,
    /// The last frame's slot on the step grid (its PTS is slot × step).
    slot: Option<u64>,
}

impl PtsClock {
    fn new(step: u64, alpha: f64) -> Self {
        PtsClock {
            step: step.max(1) as f64,
            alpha,
            last: None,
            slot: None,
        }
    }

    /// The PTS (ticks since the origin) of frame `ext` that arrived at `arrival` ticks: its
    /// smoothed time, on the next free slot of the step grid.
    fn pts(&mut self, ext: i64, arrival: f64) -> u64 {
        let t = self.smoothed(ext, arrival);
        let want = (t / self.step).round().max(0.0) as u64;
        let slot = match self.slot {
            Some(s) => want.max(s + 1),
            None => want,
        };
        self.slot = Some(slot);
        slot * self.step as u64
    }

    fn smoothed(&mut self, ext: i64, arrival: f64) -> f64 {
        let pts = match self.last {
            None => arrival,
            Some((li, lp)) => {
                let predicted = lp + (ext - li) as f64 * self.step;
                let err = arrival - predicted;
                let p = if ext <= li || err.abs() > RESYNC {
                    arrival
                } else {
                    predicted + err * self.alpha
                };
                p.max(lp + 1.0)
            }
        };
        self.last = Some((ext, pts));
        pts
    }
}

/// What a video access unit is: whether it starts with an access unit delimiter, and whether
/// it carries its own parameter sets ahead of its first slice.
fn nal_starts(au: &[u8]) -> Vec<(usize, usize)> {
    // (start of the start code, start of the NAL header)
    let mut v = Vec::new();
    let mut i = 0;
    while i + 3 <= au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let sc = if i > 0 && au[i - 1] == 0 { i - 1 } else { i };
            v.push((sc, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

fn nal_type(au: &[u8], at: usize, hevc: bool) -> Option<u8> {
    let b = *au.get(at)?;
    Some(if hevc { (b >> 1) & 0x3f } else { b & 0x1f })
}

const AUD_H264: &[u8] = &[0, 0, 0, 1, 0x09, 0xf0];
const AUD_HEVC: &[u8] = &[0, 0, 0, 1, 0x46, 0x01, 0x50];

/// The access unit as it goes out: delimiter first, then (a keyframe without its own) the
/// parameter sets, then the rest.
pub fn video_payload(au: &[u8], key: bool, header: Option<&[u8]>, hevc: bool) -> Vec<u8> {
    let starts = nal_starts(au);
    let is_aud = |t: u8| if hevc { t == 35 } else { t == 9 };
    let is_param = |t: u8| {
        if hevc {
            (32..=34).contains(&t)
        } else {
            t == 7 || t == 8
        }
    };
    let is_slice = |t: u8| if hevc { t < 32 } else { (1..=5).contains(&t) };
    let mut out = Vec::with_capacity(au.len() + header.map_or(0, <[u8]>::len) + 8);
    // The console's own delimiter, or ours.
    let rest = match starts.first() {
        Some(&(sc, h)) if nal_type(au, h, hevc).is_some_and(is_aud) => {
            let end = starts.get(1).map_or(au.len(), |&(s, _)| s);
            out.extend_from_slice(&au[sc..end]);
            &au[end..]
        }
        _ => {
            out.extend_from_slice(if hevc { AUD_HEVC } else { AUD_H264 });
            au
        }
    };
    if key {
        let own = starts
            .iter()
            .filter_map(|&(_, h)| nal_type(au, h, hevc))
            .take_while(|&t| !is_slice(t))
            .any(is_param);
        if !own {
            if let Some(h) = header {
                out.extend_from_slice(h);
            }
        }
    }
    out.extend_from_slice(rest);
    out
}

/// CRC-32/MPEG-2 for PSI sections.
fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn pts_bytes(pts: u64) -> [u8; 5] {
    let p = pts & PTS_MASK;
    [
        0x20 | ((p >> 29) as u8 & 0x0e) | 1,
        (p >> 22) as u8,
        ((p >> 14) as u8 & 0xfe) | 1,
        (p >> 7) as u8,
        ((p << 1) as u8 & 0xfe) | 1,
    ]
}

fn pcr_bytes(pcr: u64) -> [u8; 6] {
    let b = pcr & PTS_MASK;
    [
        (b >> 25) as u8,
        (b >> 17) as u8,
        (b >> 9) as u8,
        (b >> 1) as u8,
        ((b & 1) as u8) << 7 | 0x7e,
        0,
    ]
}

/// A PES packet: header with PTS, then `payload`.
fn pes(stream_id: u8, pts: u64, payload: &[u8], bounded: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(14 + payload.len());
    v.extend_from_slice(&[0, 0, 1, stream_id]);
    let len = 3 + 5 + payload.len();
    let len = if bounded && len <= 0xffff {
        len as u16
    } else {
        0
    };
    v.extend_from_slice(&len.to_be_bytes());
    v.extend_from_slice(&[0x84, 0x80, 5]); // data aligned; PTS only
    v.extend_from_slice(&pts_bytes(pts));
    v.extend_from_slice(payload);
    v
}

/// The Opus control header ffmpeg's TS demuxer expects ahead of each packet.
fn opus_control(packet: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(packet.len() + 3 + packet.len() / 255);
    v.extend_from_slice(&[0x7f, 0xe0]);
    let mut n = packet.len() as i64;
    loop {
        v.push(n.min(255) as u8);
        n -= 255;
        if n < 0 {
            break;
        }
    }
    v.extend_from_slice(packet);
    v
}

pub struct TsMuxer {
    hevc: bool,
    channels: u8,
    /// Continuity counters: PAT, PMT, video, audio.
    cc: [u8; 4],
    origin_us: Option<u64>,
    last_psi: Option<u64>,
    last_pcr: Option<u64>,
    video_idx: Unwrap,
    audio_idx: Unwrap,
    video: PtsClock,
    audio: PtsClock,
    audio_wait: BTreeMap<i64, (Vec<u8>, u64)>,
    audio_last: Option<i64>,
}

impl TsMuxer {
    /// `fps`: the session's frame rate; `channels`: Opus channels (the console sends stereo).
    pub fn new(hevc: bool, fps: u32, channels: u8) -> Self {
        TsMuxer {
            hevc,
            channels: if (1..=2).contains(&channels) {
                channels
            } else {
                2
            },
            cc: [0; 4],
            origin_us: None,
            last_psi: None,
            last_pcr: None,
            video_idx: Unwrap::default(),
            audio_idx: Unwrap::default(),
            video: PtsClock::new(90_000 / fps.clamp(1, 240) as u64, 0.1),
            audio: PtsClock::new(900, 0.01),
            audio_wait: BTreeMap::new(),
            audio_last: None,
        }
    }

    fn ticks(&self, at_us: u64) -> f64 {
        at_us.saturating_sub(self.origin_us.unwrap_or(at_us)) as f64 * 0.09
    }

    /// One video access unit that arrived at `at_us` (µs on any monotonic clock, the same for
    /// audio). The feed starts at a keyframe: anything before the first one gives nothing.
    pub fn video(
        &mut self,
        au: &[u8],
        index: u16,
        at_us: u64,
        key: bool,
        header: Option<&[u8]>,
    ) -> Vec<u8> {
        if self.origin_us.is_none() {
            if !key {
                return Vec::new();
            }
            self.origin_us = Some(at_us);
        }
        let ext = self.video_idx.next(index);
        let t = self.ticks(at_us);
        let pts = PTS_BASE + self.video.pts(ext, t);
        let pcr = pts
            .saturating_sub(PCR_DELAY)
            .max(self.last_pcr.unwrap_or(0));
        let mut out = Vec::with_capacity(au.len() + au.len() / 160 * 4 + 1024);
        self.psi(&mut out, pcr, key);
        self.last_pcr = Some(pcr);
        let payload = video_payload(au, key, header, self.hevc);
        let p = pes(0xe0, pts, &payload, false);
        self.packetize(&mut out, PID_VIDEO, 2, &p, Some(pcr), key);
        out
    }

    /// One Opus packet (`index`: the console's audio frame index) that arrived at `at_us`.
    pub fn audio(&mut self, opus: &[u8], index: u16, at_us: u64) -> Vec<u8> {
        let mut out = Vec::new();
        if self.origin_us.is_none() || opus.is_empty() {
            return out;
        }
        let ext = self.audio_idx.next(index);
        if self.audio_last.is_some_and(|l| ext <= l) || self.audio_wait.contains_key(&ext) {
            return out; // too late, or a repeat
        }
        self.audio_wait.insert(ext, (opus.to_vec(), at_us));
        let newest = *self.audio_wait.keys().next_back().unwrap_or(&ext);
        while let Some(first) = self.audio_wait.keys().next().copied() {
            let in_order = self.audio_last.is_none_or(|l| first == l + 1);
            if !in_order && first > newest - AUDIO_REORDER {
                break;
            }
            let (pkt, at) = self.audio_wait.remove(&first).unwrap_or_default();
            self.audio_last = Some(first);
            let t = self.ticks(at);
            let pts = PTS_BASE + self.audio.pts(first, t);
            let now = pts.saturating_sub(PCR_DELAY);
            self.psi(&mut out, now, false);
            // Video held back: keep the clock going.
            if self
                .last_pcr
                .is_some_and(|l| now > l && now - l >= PCR_EVERY)
            {
                self.pcr_only(&mut out, now);
                self.last_pcr = Some(now);
            }
            let p = pes(0xbd, pts, &opus_control(&pkt), true);
            self.packetize(&mut out, PID_AUDIO, 3, &p, None, false);
        }
        out
    }

    fn psi(&mut self, out: &mut Vec<u8>, now: u64, force: bool) {
        if !force
            && self
                .last_psi
                .is_some_and(|l| now.saturating_sub(l) < PSI_EVERY)
        {
            return;
        }
        self.last_psi = Some(now);
        let pat = self.pat();
        self.section(out, PID_PAT, 0, &pat);
        let pmt = self.pmt();
        self.section(out, PID_PMT, 1, &pmt);
    }

    fn pat(&self) -> Vec<u8> {
        let mut s = vec![0x00, 0xb0, 0, 0x00, 0x01, 0xc1, 0x00, 0x00];
        s.extend_from_slice(&[0x00, 0x01, 0xe0 | (PID_PMT >> 8) as u8, PID_PMT as u8]);
        finish_section(s)
    }

    fn pmt(&self) -> Vec<u8> {
        let mut s = vec![0x02, 0xb0, 0, 0x00, 0x01, 0xc1, 0x00, 0x00];
        s.extend_from_slice(&[0xe0 | (PID_VIDEO >> 8) as u8, PID_VIDEO as u8, 0xf0, 0x00]);
        let vt = if self.hevc {
            STREAM_TYPE_HEVC
        } else {
            STREAM_TYPE_H264
        };
        s.extend_from_slice(&[vt, 0xe0 | (PID_VIDEO >> 8) as u8, PID_VIDEO as u8, 0xf0, 0]);
        // Opus: registration "Opus", then the DVB extension descriptor (0x7F) with the Opus
        // audio descriptor (tag extension 0x80) and its channel config (1 mono, 2 stereo).
        let es = [
            0x05,
            4,
            b'O',
            b'p',
            b'u',
            b's',
            0x7f,
            2,
            0x80,
            self.channels,
        ];
        s.extend_from_slice(&[
            STREAM_TYPE_PRIVATE,
            0xe0 | (PID_AUDIO >> 8) as u8,
            PID_AUDIO as u8,
            0xf0,
            es.len() as u8,
        ]);
        s.extend_from_slice(&es);
        finish_section(s)
    }

    /// One PSI section in one TS packet (ours are always short).
    fn section(&mut self, out: &mut Vec<u8>, pid: u16, cc: usize, sec: &[u8]) {
        let mut p = [0xffu8; TS_PACKET];
        p[0] = 0x47;
        p[1] = 0x40 | (pid >> 8) as u8;
        p[2] = pid as u8;
        p[3] = 0x10 | self.next_cc(cc);
        p[4] = 0; // pointer field
        p[5..5 + sec.len()].copy_from_slice(sec);
        out.extend_from_slice(&p);
    }

    fn next_cc(&mut self, i: usize) -> u8 {
        let c = self.cc[i];
        self.cc[i] = (c + 1) & 0x0f;
        c
    }

    /// An adaptation-only packet on the video PID carrying just a PCR (no payload: the
    /// continuity counter stays).
    fn pcr_only(&mut self, out: &mut Vec<u8>, pcr: u64) {
        let mut p = [0xffu8; TS_PACKET];
        p[0] = 0x47;
        p[1] = (PID_VIDEO >> 8) as u8;
        p[2] = PID_VIDEO as u8;
        p[3] = 0x20 | (self.cc[2].wrapping_sub(1) & 0x0f);
        p[4] = 183;
        p[5] = 0x10;
        p[6..12].copy_from_slice(&pcr_bytes(pcr));
        out.extend_from_slice(&p);
    }

    /// A PES in TS packets, with a PCR and the random-access mark on the first when asked;
    /// the last is padded with adaptation-field stuffing.
    fn packetize(
        &mut self,
        out: &mut Vec<u8>,
        pid: u16,
        cc: usize,
        pes: &[u8],
        pcr: Option<u64>,
        random_access: bool,
    ) {
        let mut pos = 0;
        let mut first = true;
        while pos < pes.len() {
            let mut p = [0xffu8; TS_PACKET];
            p[0] = 0x47;
            p[1] = if first { 0x40 } else { 0 } | (pid >> 8) as u8;
            p[2] = pid as u8;
            // The adaptation field's contents after its length byte.
            let mut af: Vec<u8> = Vec::new();
            if first && (pcr.is_some() || random_access) {
                af.push(
                    if random_access { 0x40 } else { 0 } | if pcr.is_some() { 0x10 } else { 0 },
                );
                if let Some(pcr) = pcr {
                    af.extend_from_slice(&pcr_bytes(pcr));
                }
            }
            let left = pes.len() - pos;
            let af_size = if af.is_empty() { 0 } else { 1 + af.len() };
            let room = 184 - af_size;
            let af_size = if left < room {
                // Pad the last packet: the adaptation field grows to fill it.
                let need = 184 - left;
                if af.is_empty() && need >= 2 {
                    af.push(0);
                }
                while 1 + af.len() < need {
                    af.push(0xff);
                }
                need
            } else {
                af_size
            };
            let has_af = af_size > 0;
            p[3] = if has_af { 0x30 } else { 0x10 } | self.next_cc(cc);
            let mut at = 4;
            if has_af {
                p[4] = (af_size - 1) as u8;
                p[5..5 + af.len()].copy_from_slice(&af);
                at += af_size;
            }
            let n = (TS_PACKET - at).min(left);
            p[at..at + n].copy_from_slice(&pes[pos..pos + n]);
            pos += n;
            first = false;
            out.extend_from_slice(&p);
        }
    }
}

/// Fill in a section's length and append its CRC.
fn finish_section(mut s: Vec<u8>) -> Vec<u8> {
    let len = s.len() - 3 + 4;
    s[1] = 0xb0 | ((len >> 8) as u8 & 0x0f);
    s[2] = len as u8;
    let crc = crc32_mpeg2(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 1, 2, 3];
    const P: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 1];

    fn packets(b: &[u8]) -> Vec<&[u8]> {
        assert_eq!(b.len() % TS_PACKET, 0, "whole TS packets");
        b.chunks(TS_PACKET).collect()
    }

    fn pid(p: &[u8]) -> u16 {
        ((p[1] as u16 & 0x1f) << 8) | p[2] as u16
    }

    fn pes_pts(p: &[u8]) -> u64 {
        // The PES starts after the header and any adaptation field.
        let at = if p[3] & 0x20 != 0 {
            5 + p[4] as usize
        } else {
            4
        };
        let h = &p[at..];
        assert_eq!(&h[..3], &[0, 0, 1]);
        let b = &h[9..14];
        ((b[0] as u64 >> 1) & 7) << 30
            | (b[1] as u64) << 22
            | ((b[2] as u64) >> 1) << 15
            | (b[3] as u64) << 7
            | (b[4] as u64) >> 1
    }

    #[test]
    fn crc_matches_the_mpeg2_check_value() {
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376_e6e7);
    }

    #[test]
    fn a_keyframe_gets_a_delimiter_and_the_parameter_sets() {
        let out = video_payload(IDR, true, Some(SPS_PPS), false);
        let mut want = AUD_H264.to_vec();
        want.extend_from_slice(SPS_PPS);
        want.extend_from_slice(IDR);
        assert_eq!(out, want);
        // Not a keyframe: just the delimiter.
        let out = video_payload(P, false, Some(SPS_PPS), false);
        assert_eq!(&out[..AUD_H264.len()], AUD_H264);
        assert_eq!(&out[AUD_H264.len()..], P);
        // The console's own delimiter is kept (not doubled) and the sets go after it.
        let mut au = vec![0, 0, 0, 1, 0x09, 0x10];
        au.extend_from_slice(IDR);
        let out = video_payload(&au, true, Some(SPS_PPS), false);
        let mut want = vec![0, 0, 0, 1, 0x09, 0x10];
        want.extend_from_slice(SPS_PPS);
        want.extend_from_slice(IDR);
        assert_eq!(out, want);
        // A keyframe with its own parameter sets is left as it is.
        let mut au = SPS_PPS.to_vec();
        au.extend_from_slice(IDR);
        let out = video_payload(&au, true, Some(b"XX"), false);
        assert_eq!(&out[AUD_H264.len()..], &au[..]);
    }

    #[test]
    fn hevc_uses_its_own_delimiter_and_parameter_set_types() {
        let vps_sps_pps: &[u8] = &[0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1];
        let idr: &[u8] = &[0, 0, 1, 0x26, 1, 0xaf]; // IDR_W_RADL (19)
        let out = video_payload(idr, true, Some(vps_sps_pps), true);
        let mut want = AUD_HEVC.to_vec();
        want.extend_from_slice(vps_sps_pps);
        want.extend_from_slice(idr);
        assert_eq!(out, want);
    }

    #[test]
    fn psi_then_a_keyframe_with_pcr_and_random_access() {
        let mut m = TsMuxer::new(false, 60, 2);
        // Nothing before the first keyframe, audio included.
        assert!(m.video(P, 1, 0, false, Some(SPS_PPS)).is_empty());
        assert!(m.audio(&[1, 2, 3], 1, 0).is_empty());
        let out = m.video(IDR, 2, 1_000, true, Some(SPS_PPS));
        let ps = packets(&out);
        assert_eq!(pid(ps[0]), PID_PAT);
        assert_eq!(pid(ps[1]), PID_PMT);
        assert_eq!(pid(ps[2]), PID_VIDEO);
        // PUSI, adaptation field with random access + PCR.
        assert_eq!(ps[2][1] & 0x40, 0x40);
        assert_eq!(ps[2][3] & 0x30, 0x30);
        assert_eq!(ps[2][5] & 0x50, 0x50);
        assert_eq!(pes_pts(ps[2]), PTS_BASE);
        // Sections carry a valid CRC (the CRC of a section with its CRC is 0).
        for p in &ps[..2] {
            let len = (((p[6] & 0x0f) as usize) << 8) | p[7] as usize;
            assert_eq!(crc32_mpeg2(&p[5..8 + len]), 0);
        }
        // The PMT names H.264 and Opus with its descriptors.
        let pmt = ps[1];
        assert!(pmt.windows(5).any(|w| w == [0x1b, 0xe1, 0x00, 0xf0, 0]));
        assert!(pmt
            .windows(10)
            .any(|w| w == [0x05, 4, b'O', b'p', b'u', b's', 0x7f, 2, 0x80, 2]));
    }

    #[test]
    fn a_big_frame_spans_packets_with_counting_continuity() {
        let mut m = TsMuxer::new(true, 60, 2);
        let mut au = vec![0, 0, 1, 0x26, 1];
        au.extend(std::iter::repeat_n(0xab, 5000));
        let out = m.video(&au, 0, 0, true, None);
        let vids: Vec<&[u8]> = packets(&out)
            .into_iter()
            .filter(|p| pid(p) == PID_VIDEO)
            .collect();
        assert!(vids.len() > 25);
        for (i, p) in vids.iter().enumerate() {
            assert_eq!((p[3] & 0x0f) as usize, i % 16);
            assert_eq!(p[1] & 0x40 != 0, i == 0);
        }
        // The payload comes back whole.
        let mut pes_bytes = Vec::new();
        for p in &vids {
            let at = if p[3] & 0x20 != 0 {
                5 + p[4] as usize
            } else {
                4
            };
            pes_bytes.extend_from_slice(&p[at..]);
        }
        let payload = &pes_bytes[14..];
        assert_eq!(payload, video_payload(&au, true, None, true));
    }

    #[test]
    fn every_payload_length_pads_to_a_whole_packet() {
        // Payload sizes around the packet boundary, including the 1-byte stuffing case.
        for n in 150..400 {
            let mut m = TsMuxer::new(false, 60, 2);
            let mut au = IDR.to_vec();
            au.extend(std::iter::repeat_n(0x11, n));
            let out = m.video(&au, 0, 0, true, None);
            let vids: Vec<&[u8]> = packets(&out)
                .into_iter()
                .filter(|p| pid(p) == PID_VIDEO)
                .collect();
            let mut got = Vec::new();
            for p in &vids {
                let at = if p[3] & 0x20 != 0 {
                    5 + p[4] as usize
                } else {
                    4
                };
                got.extend_from_slice(&p[at..]);
            }
            assert_eq!(
                &got[14..],
                &video_payload(&au, true, None, false)[..],
                "n {n}"
            );
        }
    }

    #[test]
    fn opus_control_header_sizes() {
        assert_eq!(&opus_control(&[9; 100])[..3], &[0x7f, 0xe0, 100]);
        assert_eq!(&opus_control(&[9; 255])[..4], &[0x7f, 0xe0, 255, 0]);
        assert_eq!(&opus_control(&[9; 300])[..4], &[0x7f, 0xe0, 255, 45]);
    }

    #[test]
    fn timestamps_follow_the_index_through_jitter_and_wrap() {
        let mut m = TsMuxer::new(false, 60, 2);
        let mut last = 0;
        // Frames 65530.. wrap past 0; arrivals jitter by up to 8 ms either way.
        for i in 0..200u32 {
            let idx = 65530u16.wrapping_add(i as u16);
            let jitter = [0i64, 8000, -6000, 3000, -8000][i as usize % 5];
            let at = (i as i64 * 16_667 + 20_000 + jitter) as u64;
            let out = m.video(if i == 0 { IDR } else { P }, idx, at, i == 0, Some(SPS_PPS));
            let first_video = packets(&out)
                .into_iter()
                .find(|p| pid(p) == PID_VIDEO)
                .unwrap();
            let pts = pes_pts(first_video);
            assert!(pts > last, "pts went back at {i}");
            if i > 0 {
                // On the frame grid; once settled, one frame apart.
                assert_eq!((pts - last) % 1500, 0, "step {} at {i}", pts - last);
                if i > 20 {
                    assert_eq!(pts - last, 1500, "at {i}");
                }
            }
            last = pts;
        }
    }

    #[test]
    fn audio_starts_with_video_reorders_and_never_goes_back() {
        let mut m = TsMuxer::new(false, 60, 2);
        m.video(IDR, 0, 1_000_000, true, None);
        let mut pts = Vec::new();
        let mut feed = |m: &mut TsMuxer, idx: u16, at: u64| {
            for p in packets(&m.audio(&[0xfc, 1, 2], idx, at)) {
                if pid(p) == PID_AUDIO && p[1] & 0x40 != 0 {
                    pts.push(pes_pts(p));
                }
            }
        };
        feed(&mut m, 65534, 1_000_000);
        feed(&mut m, 0, 1_020_000); // 65535 is late
        feed(&mut m, 65535, 1_021_000); // it comes, and goes in its place
        feed(&mut m, 1, 1_030_000);
        feed(&mut m, 65533, 1_031_000); // older than what went out: dropped
        feed(&mut m, 2, 1_040_000);
        assert_eq!(pts.len(), 5, "{pts:?}");
        assert_eq!(pts[0], PTS_BASE);
        for w in pts.windows(2) {
            assert!(w[1] > w[0]);
            assert!(w[1] - w[0] < 1200, "{pts:?}");
        }
    }

    #[test]
    fn held_video_keeps_the_pcr_going_on_audio() {
        let mut m = TsMuxer::new(false, 60, 2);
        m.video(IDR, 0, 0, true, None);
        let mut pcr_only = 0;
        for i in 0..50u16 {
            for p in packets(&m.audio(&[0xfc, 1], i, i as u64 * 10_000)) {
                if pid(p) == PID_VIDEO && p[3] & 0x30 == 0x20 {
                    pcr_only += 1;
                }
            }
        }
        // 0.5 s of audio and no video: a PCR every 40 ms or so.
        assert!((10..=14).contains(&pcr_only), "{pcr_only}");
    }
}
