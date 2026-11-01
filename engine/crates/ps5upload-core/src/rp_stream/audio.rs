// SPDX-License-Identifier: AGPL-3.0-only
// Ported from chiaki-ng: Copyright (C) the Chiaki and chiaki-ng authors. Rust port for
// ps5upload. Licensed under the GNU AGPL v3.0 (LICENSES/AGPL-3.0.txt).
//! Audio frames out of Takion audio packets.
//!
//! Ported from chiaki-ng (AGPL-3.0, `lib/src/audioreceiver.c`, `audio.c`).
//! Each packet carries `source` consecutive Opus frames starting at its frame
//! index, then `fec` copies of the frames just before it (redundancy, so a
//! lost packet's audio usually arrives in the next one). Every unit is the
//! same size. We pass each frame on once, tagged with its index; ordering and
//! the jitter buffer are the player's.

use super::takion::AvPacket;

/// The 14-byte audio header from STREAMINFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    pub channels: u8,
    pub bits: u8,
    pub rate: u32,
    pub frame_size: u32,
}

impl AudioHeader {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 14 {
            return None;
        }
        Some(Self {
            channels: b[0],
            bits: b[1],
            rate: u32::from_be_bytes(b[2..6].try_into().ok()?),
            frame_size: u32::from_be_bytes(b[6..10].try_into().ok()?),
        })
    }
}

fn seq16_gt(a: u16, b: u16) -> bool {
    a != b && (a.wrapping_sub(b) as i16) > 0
}

/// How far back a frame may arrive and still be played.
const WINDOW: u16 = 64;

/// Which of the last [`WINDOW`] indexes were seen.
#[derive(Default)]
struct SeenWindow {
    newest: Option<u16>,
    /// Bit i set = index `newest - i` already seen.
    seen: u64,
}

impl SeenWindow {
    /// True the first time `idx` is seen (and not too old to tell).
    fn mark(&mut self, idx: u16) -> bool {
        match self.newest {
            None => {
                self.newest = Some(idx);
                self.seen = 1;
                true
            }
            Some(n) if seq16_gt(idx, n) => {
                let shift = idx.wrapping_sub(n) as u32;
                self.seen = if shift >= 64 { 0 } else { self.seen << shift };
                self.seen |= 1;
                self.newest = Some(idx);
                true
            }
            Some(n) => {
                let back = n.wrapping_sub(idx);
                if back >= WINDOW {
                    return false;
                }
                let bit = 1u64 << back;
                if self.seen & bit != 0 {
                    return false;
                }
                self.seen |= bit;
                true
            }
        }
    }
}

#[derive(Default)]
pub struct AudioAssembler {
    /// Frames already delivered (a packet's own, or an FEC copy).
    delivered: SeenWindow,
    /// Packets already counted, by their frame index: a repeated packet is not a second one.
    packets: SeenWindow,
    pub frames: u64,
    /// Packet sequence stats since the last [`AudioAssembler::take_seq`]
    /// (chiaki's "seq" stats over audio packet frame indexes).
    seq_min: Option<u16>,
    seq_max: u16,
    seq_received: u64,
}

impl AudioAssembler {
    /// Packets received and lost since the last call, as chiaki counts them:
    /// the spread of frame indexes seen against how many packets came.
    ///
    /// chiaki-ng (`packetstats.c`) reports the whole spread as lost when more packets came
    /// than it spans: the first window (its first packet is counted and spans nothing), or a
    /// late packet from the window before. That told the console a burst of loss that never
    /// happened, and it cuts its bitrate on that. More packets than the spread means none of it
    /// was lost. A repeated packet is counted once, so it cannot hide a real loss.
    pub fn take_seq(&mut self) -> (u64, u64) {
        let Some(min) = self.seq_min else {
            return (0, 0);
        };
        let diff = self.seq_max.wrapping_sub(min) as u64;
        let lost = diff.saturating_sub(self.seq_received);
        let r = (self.seq_received, lost);
        self.seq_min = Some(self.seq_max);
        self.seq_received = 0;
        r
    }

    /// Opus frames in this packet not delivered before, with their index.
    pub fn push(&mut self, p: &AvPacket) -> Vec<(u16, Vec<u8>)> {
        let mut out = Vec::new();
        // Counted once: a repeated packet must not cancel out a real loss.
        if self.packets.mark(p.frame_index) {
            self.seq_received += 1;
            if self.seq_min.is_none() {
                self.seq_min = Some(p.frame_index);
                self.seq_max = p.frame_index;
            } else if seq16_gt(p.frame_index, self.seq_max) {
                self.seq_max = p.frame_index;
            }
        }
        if p.codec != 5 {
            return out;
        }
        let unit = (p.units_in_frame_fec >> 8) as usize;
        let source = (p.units_in_frame_fec & 0xf) as usize;
        let fec = ((p.units_in_frame_fec >> 4) & 0xf) as usize;
        if unit == 0
            || source + fec != p.units_in_frame_total as usize
            || p.data.len() != unit * (source + fec)
        {
            return out;
        }
        for i in 0..source + fec {
            let idx = if i < source {
                p.frame_index.wrapping_add(i as u16)
            } else {
                // Before the stream's first frames, FEC slots point at nothing.
                let fec_index = i - source;
                if p.frame_index < 0x8000 && (p.frame_index as usize + fec_index) < fec + 1 {
                    continue;
                }
                p.frame_index
                    .wrapping_sub(fec as u16)
                    .wrapping_add((i - source) as u16)
            };
            if self.delivered.mark(idx) {
                self.frames += 1;
                out.push((idx, p.data[unit * i..unit * (i + 1)].to_vec()));
            }
        }
        out
    }
}

/// The DualSense's haptics: audio packets flagged 0x02 (v12), framed and protected like the
/// sound, but each unit is raw PCM (3 kHz, stereo s16le, 10 ms = 120 bytes) for the pad's
/// actuators. Only a frame newer than the last one played is passed on: a late copy (FEC) of an
/// older frame would buzz out of turn (chiaki `chiaki_audio_receiver_frame`, `is_haptics`).
/// Not counted in the congestion reports (chiaki's haptics receiver has no packet stats).
#[derive(Default)]
pub struct HapticsAssembler {
    inner: AudioAssembler,
    last: Option<u16>,
}

impl HapticsAssembler {
    /// The PCM frames in this packet to play now, oldest first.
    pub fn push(&mut self, p: &AvPacket) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (idx, frame) in self.inner.push(p) {
            if self.last.is_none_or(|l| seq16_gt(idx, l)) {
                self.last = Some(idx);
                out.push(frame);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(frame: u16, source: u16, fec: u16, unit: u16) -> AvPacket {
        let n = (source + fec) as usize;
        AvPacket {
            is_video: false,
            packet_index: 0,
            frame_index: frame,
            unit_index: 0,
            units_in_frame_total: source + fec,
            units_in_frame_fec: (unit << 8) | (fec << 4) | source,
            codec: 5,
            adaptive_stream_index: 0,
            is_haptics: false,
            key_pos: 0,
            data: (0..n * unit as usize)
                .map(|i| (i / unit as usize) as u8)
                .collect(),
        }
    }

    #[test]
    fn fec_copies_fill_a_gap_and_duplicates_are_dropped() {
        let mut a = AudioAssembler::default();
        let got: Vec<u16> = a.push(&pkt(10, 1, 2, 4)).into_iter().map(|f| f.0).collect();
        assert_eq!(got, vec![10, 8, 9]);
        // Packet for 11 lost; 12 carries copies of 10 and 11.
        let got: Vec<u16> = a.push(&pkt(12, 1, 2, 4)).into_iter().map(|f| f.0).collect();
        assert_eq!(got, vec![12, 11]);
    }

    #[test]
    fn reads_the_header() {
        let mut h = [0u8; 14];
        h[0] = 2;
        h[1] = 16;
        h[2..6].copy_from_slice(&48000u32.to_be_bytes());
        h[6..10].copy_from_slice(&480u32.to_be_bytes());
        let a = AudioHeader::parse(&h).unwrap();
        assert_eq!((a.channels, a.rate, a.frame_size), (2, 48000, 480));
    }

    #[test]
    fn audio_loss_counts_only_missing_packets() {
        let mut a = AudioAssembler::default();
        // The first window: 10..=14, nothing missing. chiaki-ng called this 4 lost.
        for f in 10..=14 {
            a.push(&pkt(f, 1, 0, 4));
        }
        assert_eq!(a.take_seq(), (5, 0));
        // 16 and 17 missing; a repeat of 13 (already counted) does not hide one of them.
        for f in [15, 13, 18, 19] {
            a.push(&pkt(f, 1, 0, 4));
        }
        assert_eq!(a.take_seq(), (3, 2));
        // 20..=23 with one repeated: counted once, none lost (chiaki-ng: 4).
        for f in [20, 21, 21, 22, 23] {
            a.push(&pkt(f, 1, 0, 4));
        }
        assert_eq!(a.take_seq(), (4, 0));
        // A late packet from the window before that never came then: it came, more packets than
        // the spread, none lost.
        let mut b = AudioAssembler::default();
        for f in [30, 33] {
            b.push(&pkt(f, 1, 0, 4));
        }
        assert_eq!(b.take_seq(), (2, 1));
        for f in [34, 31, 35] {
            b.push(&pkt(f, 1, 0, 4));
        }
        assert_eq!(b.take_seq(), (3, 0));
        // A packet whose frame came first as an FEC copy is still a packet that arrived.
        let mut c = AudioAssembler::default();
        c.push(&pkt(40, 1, 0, 4));
        c.take_seq();
        c.push(&pkt(42, 1, 2, 4)); // carries 41 (FEC)
        c.push(&pkt(41, 1, 0, 4)); // 41's own packet, late
        assert_eq!(c.take_seq(), (2, 0));
        // Two of 24..=27 missing.
        for f in [24, 27] {
            a.push(&pkt(f, 1, 0, 4));
        }
        assert_eq!(a.take_seq(), (2, 2));
        // Nothing came: nothing to say.
        assert_eq!(a.take_seq(), (0, 0));
    }

    #[test]
    fn haptics_pass_on_only_frames_newer_than_the_last() {
        let mut h = HapticsAssembler::default();
        // Frame 10 (source) and FEC copies of 8, 9: only 10 is new enough.
        assert_eq!(h.push(&pkt(10, 1, 2, 4)).len(), 1);
        // 12 arrives (11 lost): 12 plays, the late copy of 11 does not.
        let got = h.push(&pkt(12, 1, 2, 4));
        assert_eq!(got, vec![vec![0u8; 4]]);
        // Across the wrap.
        let mut h = HapticsAssembler::default();
        assert_eq!(h.push(&pkt(0xffff, 1, 0, 4)).len(), 1);
        assert_eq!(h.push(&pkt(0, 1, 0, 4)).len(), 1);
        assert!(h.push(&pkt(0xfffe, 1, 0, 4)).is_empty());
    }
}
