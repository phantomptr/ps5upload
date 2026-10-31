// SPDX-License-Identifier: AGPL-3.0-only
// Ported from chiaki-ng: Copyright (C) the Chiaki and chiaki-ng authors. Rust port for
// ps5upload. Licensed under the GNU AGPL v3.0 (LICENSES/AGPL-3.0.txt).
//! Rebuilds whole video frames from Takion video units.
//!
//! Ported from chiaki-ng (AGPL-3.0, `lib/src/videoreceiver.c`,
//! `lib/src/frameprocessor.c`). A frame is `k` source units then `m` FEC
//! units, all padded to one unit size; a source unit's first 2 bytes say how
//! much of that size is padding, the rest is the next slice of the Annex-B
//! bitstream. A frame is flushed as soon as `k` of its units are in: missing
//! source units are rebuilt with Reed-Solomon ([`super::fec`]).

use super::fec;
use super::takion::AvPacket;

fn seq16_gt(a: u16, b: u16) -> bool {
    a != b && (a.wrapping_sub(b) as i16) > 0
}

#[derive(Default, Debug, Clone, Copy)]
pub struct VideoStats {
    pub frames: u64,
    pub frames_recovered: u64,
    pub frames_dropped: u64,
    pub bytes: u64,
}

/// What the assembler has to say after a unit.
#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    /// A complete Annex-B access unit.
    Frame(Vec<u8>),
    /// Frames `from..=to` will never be complete: tell the console (it
    /// re-references or sends a keyframe).
    Lost { from: u16, to: u16 },
}

pub struct FrameAssembler {
    frame_index: Option<u16>,
    /// The last frame handed on or given up on.
    last_done: Option<u16>,
    flushed: bool,
    k: usize,
    m: usize,
    unit_size: usize,
    units: Vec<Option<Vec<u8>>>,
    /// Which units of the current frame came at all, before or after a flush.
    arrived: Vec<bool>,
    pub stats: VideoStats,
    /// Units received / missing in frames finished since the last
    /// [`FrameAssembler::take_generation`] (chiaki's "generation" stats).
    gen_received: u64,
    gen_lost: u64,
}

impl Default for FrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self {
            frame_index: None,
            last_done: None,
            flushed: true,
            k: 0,
            m: 0,
            unit_size: 0,
            units: Vec::new(),
            arrived: Vec::new(),
            stats: VideoStats::default(),
            gen_received: 0,
            gen_lost: 0,
        }
    }

    /// Units received and lost since the last call, for the congestion report.
    pub fn take_generation(&mut self) -> (u64, u64) {
        (
            std::mem::take(&mut self.gen_received),
            std::mem::take(&mut self.gen_lost),
        )
    }

    fn start(&mut self, p: &AvPacket, out: &mut Vec<Output>) {
        // The frame we are leaving: how many of its units ever came.
        // Counted from `arrived`, not the stored units: FEC units that come
        // after the frame was already rebuilt still arrived (chiaki counts
        // them), and calling them lost would make the console cut its bitrate.
        if self.frame_index.is_some() {
            let got = self.arrived.iter().filter(|&&a| a).count() as u64;
            self.gen_received += got;
            self.gen_lost += self.arrived.len() as u64 - got;
        }
        // Whole frames skipped since the last one finished.
        if let Some(done) = self.last_done {
            let expected = done.wrapping_add(1);
            if seq16_gt(p.frame_index, expected) {
                let to = p.frame_index.wrapping_sub(1);
                self.stats.frames_dropped += to.wrapping_sub(expected) as u64 + 1;
                out.push(Output::Lost { from: expected, to });
            }
        }
        self.frame_index = Some(p.frame_index);
        self.flushed = false;
        let total = p.units_in_frame_total as usize;
        self.m = (p.units_in_frame_fec as usize).min(total);
        self.k = total - self.m;
        // The full unit size: a source unit's data plus the padding it declares.
        self.unit_size = p.data.len();
        if (p.unit_index as usize) < self.k && p.data.len() >= 2 {
            self.unit_size += u16::from_be_bytes([p.data[0], p.data[1]]) as usize;
        }
        self.units = vec![None; total.max(1)];
        self.arrived = vec![false; total.max(1)];
    }

    /// Feed one decrypted video unit.
    pub fn push(&mut self, p: &AvPacket) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(cur) = self.frame_index {
            if p.frame_index != cur && !seq16_gt(p.frame_index, cur) {
                return out; // a unit of a frame we have moved past
            }
        }
        if self.frame_index != Some(p.frame_index) {
            if !self.flushed {
                self.flush(&mut out); // the previous frame, with what it has
            }
            self.start(p, &mut out);
        }
        let idx = p.unit_index as usize;
        if let Some(a) = self.arrived.get_mut(idx) {
            *a = true;
        }
        if self.flushed {
            return out;
        }
        if idx < self.units.len()
            && self.units[idx].is_none()
            && !p.data.is_empty()
            && p.data.len() <= self.unit_size
        {
            self.units[idx] = Some(p.data.clone());
        }
        let have = self.units.iter().filter(|u| u.is_some()).count();
        if have >= self.k || idx + 1 == self.units.len() {
            self.flush(&mut out);
        }
        out
    }

    fn flush(&mut self, out: &mut Vec<Output>) {
        self.flushed = true;
        let Some(index) = self.frame_index else {
            return;
        };
        self.last_done = Some(index);
        let k = self.k;
        let missing: Vec<usize> = (0..self.units.len())
            .filter(|&i| self.units[i].is_none())
            .collect();
        let source_missing = missing.iter().any(|&i| i < k);
        let mut sizes: Vec<usize> = self
            .units
            .iter()
            .map(|u| u.as_ref().map_or(0, Vec::len))
            .collect();
        if source_missing {
            let ok = self.m > 0 && missing.len() <= self.m && {
                let mut padded: Vec<Vec<u8>> = self
                    .units
                    .iter()
                    .map(|u| {
                        let mut v = u.clone().unwrap_or_default();
                        v.resize(self.unit_size, 0);
                        v
                    })
                    .collect();
                if fec::decode(&mut padded, k, self.m, &missing).is_ok() {
                    for &i in missing.iter().filter(|&&i| i < k) {
                        let pad = u16::from_be_bytes([padded[i][0], padded[i][1]]) as usize;
                        if pad >= self.unit_size {
                            return self.give_up(index, out);
                        }
                        sizes[i] = self.unit_size - pad;
                        self.units[i] = Some(padded[i].clone());
                    }
                    true
                } else {
                    false
                }
            };
            if !ok {
                return self.give_up(index, out);
            }
            self.stats.frames_recovered += 1;
        }
        let mut frame = Vec::new();
        for (i, u) in self.units[..k].iter().enumerate() {
            let u = u.as_ref().expect("all source units present");
            if sizes[i] >= 2 {
                frame.extend_from_slice(&u[2..sizes[i].min(u.len())]);
            }
        }
        self.stats.frames += 1;
        self.stats.bytes += frame.len() as u64;
        out.push(Output::Frame(frame));
    }

    fn give_up(&mut self, index: u16, out: &mut Vec<Output>) {
        self.stats.frames_dropped += 1;
        out.push(Output::Lost {
            from: index,
            to: index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(frame: u16, idx: u16, total: u16, fec: u16, data: &[u8]) -> AvPacket {
        AvPacket {
            is_video: true,
            packet_index: 0,
            frame_index: frame,
            unit_index: idx,
            units_in_frame_total: total,
            units_in_frame_fec: fec,
            codec: 0,
            adaptive_stream_index: 0,
            is_haptics: false,
            key_pos: 0,
            data: data.to_vec(),
        }
    }

    fn frames(o: Vec<Output>) -> Vec<Vec<u8>> {
        o.into_iter()
            .filter_map(|x| match x {
                Output::Frame(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn joins_source_units_and_skips_fec() {
        let mut a = FrameAssembler::new();
        assert!(a.push(&unit(1, 0, 3, 1, &[0, 0, 1, 2])).is_empty());
        let f = frames(a.push(&unit(1, 1, 3, 1, &[0, 1, 3]))).pop().unwrap();
        assert_eq!(f, vec![1, 2, 3]);
        assert!(a.push(&unit(1, 2, 3, 1, &[9, 9, 9, 9])).is_empty());
        assert_eq!(a.stats.frames, 1);
    }

    #[test]
    fn rebuilds_a_lost_source_unit_from_fec() {
        // k = 2 units of 4 bytes, m = 1 coding unit made with the real code.
        let src = [[0u8, 0, 0xaa, 0xbb], [0u8, 1, 0xcc, 0]];
        let mut all = [src[0], src[1], [0u8; 4]];
        // Coding row 0: C[0][j] = 1 / (0 ^ (1 + j)) → 1/1 = 1, 1/2.
        let g_half = {
            // 1/2 in GF(2^8)/0x11d is 0x8e.
            0x8eu8
        };
        for b in 0..4 {
            let mul = |a: u8, k: u8| -> u8 {
                let (mut a, mut k, mut r) = (a, k, 0u8);
                while k != 0 {
                    if k & 1 != 0 {
                        r ^= a;
                    }
                    let hi = a & 0x80;
                    a <<= 1;
                    if hi != 0 {
                        a ^= 0x1d;
                    }
                    k >>= 1;
                }
                r
            };
            all[2][b] = src[0][b] ^ mul(src[1][b], g_half);
        }
        let mut a = FrameAssembler::new();
        a.push(&unit(1, 0, 3, 1, &all[0]));
        // Unit 1 lost; the coding unit arrives.
        let f = frames(a.push(&unit(1, 2, 3, 1, &all[2]))).pop().unwrap();
        assert_eq!(f, vec![0xaa, 0xbb, 0xcc]);
        assert_eq!(a.stats.frames_recovered, 1);
    }

    #[test]
    fn fec_units_after_the_flush_count_as_received() {
        let mut a = FrameAssembler::new();
        a.push(&unit(1, 0, 3, 2, &[0, 0, 1])); // k = 1: flushes at once
        a.push(&unit(1, 1, 3, 2, &[9, 9, 9]));
        a.push(&unit(1, 2, 3, 2, &[9, 9, 9]));
        a.push(&unit(2, 0, 1, 0, &[0, 0, 2])); // closes frame 1's books
        assert_eq!(a.take_generation(), (3, 0));
    }

    #[test]
    fn reports_a_skipped_frame() {
        let mut a = FrameAssembler::new();
        a.push(&unit(1, 0, 1, 0, &[0, 0, 1]));
        let out = a.push(&unit(3, 0, 1, 0, &[0, 0, 3]));
        assert_eq!(out[0], Output::Lost { from: 2, to: 2 });
        assert!(matches!(out[1], Output::Frame(_)));
    }
}
