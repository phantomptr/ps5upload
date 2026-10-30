//! Instant replay: the last seconds of a Remote Play session, kept as the console sent them.
//!
//! Encoded video access units (Annex-B H.264/HEVC) and Opus audio frames, with the time they
//! arrived, in order, trimmed from the front so the oldest kept frame is the newest keyframe
//! that still leaves the whole window (a clip can only start at a keyframe). Nothing is
//! re-encoded here; the page turns a snapshot into an MP4 when the player saves it.
//!
//! The snapshot's wire form (`GET /api/ps5/rp/replay`), all integers big-endian:
//! `"PSRP1"`, then records `[kind u8][t_us u64][len u32][bytes]`: kind 0 = video codec header,
//! 3 = audio header (both at t 0, first), 1 = video access unit, 4 = Opus frame; then for kind 1
//! one more byte before the bytes: 1 for a keyframe, 0 otherwise.

use std::collections::VecDeque;
use std::time::Duration;

/// The windows a player can choose.
pub const WINDOWS_S: [u64; 3] = [15, 30, 60];
/// Whatever the window, never more than this is held (a 60 s window at a high bitrate fits).
pub const MAX_BYTES: usize = 256 * 1024 * 1024;
/// While replay is on, a keyframe is asked for at least this often, so a clip starts at most
/// this long before the window.
pub const KEYFRAME_EVERY: Duration = Duration::from_secs(5);

pub const KIND_VIDEO_HEADER: u8 = 0;
pub const KIND_VIDEO: u8 = 1;
pub const KIND_AUDIO_HEADER: u8 = 3;
pub const KIND_AUDIO: u8 = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub kind: u8,
    pub t_us: u64,
    pub key: bool,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct Replay {
    window_us: u64,
    frames: VecDeque<Frame>,
    bytes: usize,
    /// When the last keyframe arrived (for asking the console for the next one).
    last_key_us: Option<u64>,
}

/// The window a request asked for, snapped to one a player can choose; 0 / none = off.
pub fn window_s(asked: Option<u64>) -> Option<u64> {
    let s = asked.filter(|s| *s > 0)?;
    Some(
        *WINDOWS_S
            .iter()
            .min_by_key(|w| w.abs_diff(s))
            .unwrap_or(&30),
    )
}

impl Replay {
    pub fn new(window_s: u64) -> Self {
        Replay {
            window_us: window_s * 1_000_000,
            frames: VecDeque::new(),
            bytes: 0,
            last_key_us: None,
        }
    }

    pub fn window_s(&self) -> u64 {
        self.window_us / 1_000_000
    }

    /// Whether a keyframe should be asked for now (`now_us` on the frames' clock).
    pub fn wants_keyframe(&self, now_us: u64) -> bool {
        self.last_key_us
            .is_none_or(|t| now_us.saturating_sub(t) >= KEYFRAME_EVERY.as_micros() as u64)
    }

    pub fn push(&mut self, kind: u8, t_us: u64, key: bool, data: &[u8]) {
        // Until the first keyframe nothing can be played back: keep nothing before it.
        if self.frames.is_empty() && !(kind == KIND_VIDEO && key) {
            return;
        }
        if kind == KIND_VIDEO && key {
            self.last_key_us = Some(t_us);
        }
        self.bytes += data.len();
        self.frames.push_back(Frame {
            kind,
            t_us,
            key,
            data: data.to_vec(),
        });
        self.trim(t_us);
    }

    /// Drop what the window no longer needs: everything before the newest keyframe that is at
    /// or before the window's start (and anything over the byte cap, keyframe by keyframe).
    fn trim(&mut self, now_us: u64) {
        let start = now_us.saturating_sub(self.window_us);
        let cut = self
            .frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.kind == KIND_VIDEO && f.key && f.t_us <= start)
            .map(|(i, _)| i)
            .next_back()
            .unwrap_or(0);
        self.drop_front(cut);
        while self.bytes > MAX_BYTES {
            // The next keyframe after the first frame: dropping up to it keeps a clean start.
            let Some(next) = self
                .frames
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, f)| f.kind == KIND_VIDEO && f.key)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.drop_front(next);
        }
    }

    fn drop_front(&mut self, n: usize) {
        for f in self.frames.drain(..n) {
            self.bytes -= f.data.len();
        }
    }

    pub fn frames(&self) -> impl Iterator<Item = &Frame> {
        self.frames.iter()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The snapshot as sent to the page (see the module doc).
pub fn encode(video_header: &[u8], audio_header: Option<&[u8]>, r: &Replay) -> Vec<u8> {
    let mut out = Vec::with_capacity(r.bytes() + r.frames.len() * 14 + 64);
    out.extend_from_slice(b"PSRP1");
    let mut rec = |kind: u8, t: u64, key: Option<bool>, data: &[u8]| {
        out.push(kind);
        out.extend_from_slice(&t.to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        if let Some(k) = key {
            out.push(k as u8);
        }
        out.extend_from_slice(data);
    };
    rec(KIND_VIDEO_HEADER, 0, None, video_header);
    if let Some(a) = audio_header {
        rec(KIND_AUDIO_HEADER, 0, None, a);
    }
    for f in r.frames() {
        rec(
            f.kind,
            f.t_us,
            (f.kind == KIND_VIDEO).then_some(f.key),
            &f.data,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(r: &Replay) -> Vec<(u8, u64, bool)> {
        r.frames()
            .map(|f| (f.kind, f.t_us / 1_000_000, f.key))
            .collect()
    }

    #[test]
    fn the_window_snaps_to_a_choosable_one() {
        assert_eq!(window_s(None), None);
        assert_eq!(window_s(Some(0)), None);
        assert_eq!(window_s(Some(30)), Some(30));
        assert_eq!(window_s(Some(20)), Some(15));
        assert_eq!(window_s(Some(999)), Some(60));
    }

    #[test]
    fn a_replay_starts_at_a_keyframe_and_covers_the_window() {
        let mut r = Replay::new(15);
        // Audio and a delta frame before any keyframe are useless: not kept.
        r.push(KIND_AUDIO, 0, false, b"a");
        r.push(KIND_VIDEO, 0, false, b"d");
        assert_eq!(r.frames().count(), 0);
        // Keyframes at 0, 10, 20, 30 s, a delta frame and audio each second.
        for s in 0..=32u64 {
            let t = s * 1_000_000;
            r.push(KIND_VIDEO, t, s % 10 == 0, b"vvvv");
            r.push(KIND_AUDIO, t, false, b"aa");
        }
        // At 32 s the window starts at 17 s; the newest keyframe at or before it is 10 s.
        let k = kinds(&r);
        assert_eq!(k.first(), Some(&(KIND_VIDEO, 10, true)));
        assert_eq!(k.last(), Some(&(KIND_AUDIO, 32, false)));
        assert_eq!(r.bytes(), 23 * 6);
    }

    #[test]
    fn a_keyframe_is_asked_for_every_few_seconds() {
        let mut r = Replay::new(30);
        assert!(r.wants_keyframe(0));
        r.push(KIND_VIDEO, 1_000_000, true, b"k");
        assert!(!r.wants_keyframe(5_000_000));
        assert!(r.wants_keyframe(6_000_000));
    }

    #[test]
    fn the_snapshot_wire_form() {
        let mut r = Replay::new(30);
        r.push(KIND_VIDEO, 7, true, b"K");
        r.push(KIND_AUDIO, 9, false, b"A");
        let b = encode(b"VH", Some(b"AH"), &r);
        let mut want = b"PSRP1".to_vec();
        want.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        want.extend(b"VH");
        want.extend([3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        want.extend(b"AH");
        want.extend([1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 1]);
        want.extend(b"K");
        want.extend([4, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 1]);
        want.extend(b"A");
        assert_eq!(b, want);
    }
}
