//! Instant replay: the last seconds of a Remote Play session, kept as the console sent them.
//!
//! Encoded video access units (Annex-B H.264/HEVC) and Opus audio frames, with the time they
//! arrived, in order, trimmed from the front so the oldest kept frame is the newest keyframe
//! that still leaves the whole window (a clip can only start at a keyframe). Nothing is
//! re-encoded here; the page turns a snapshot into an MP4 when the player saves it.
//!
//! The snapshot's wire form (`GET /api/ps5/rp/replay`), all integers big-endian:
//! `"PSRP1"`, then records `[kind u8][t_us u64][len u32][bytes]`: kind 0 = video codec header,
//! 3 = audio header (both at t 0, first), 1 = video access unit, 4 = audio frame as the session
//! carries it (`[u16 BE index][Opus packet]`; a recovered copy can arrive late); then for kind 1
//! one more byte before the bytes: 1 for a keyframe, 0 otherwise.

use std::collections::VecDeque;
use std::time::Duration;

use axum::body::Bytes;

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
    /// Shared with the frame sent to the page: keeping it costs no copy.
    pub data: Bytes,
}

#[derive(Debug)]
pub struct Replay {
    window_us: u64,
    frames: VecDeque<Frame>,
    /// When each kept keyframe arrived, oldest first. The first frame is always the first of them.
    keys: VecDeque<u64>,
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
            keys: VecDeque::new(),
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

    pub fn push(&mut self, kind: u8, t_us: u64, key: bool, data: Bytes) {
        // Until the first keyframe nothing can be played back: keep nothing before it.
        if self.frames.is_empty() && !(kind == KIND_VIDEO && key) {
            return;
        }
        if kind == KIND_VIDEO && key {
            self.last_key_us = Some(t_us);
            self.keys.push_back(t_us);
        }
        self.bytes += data.len();
        self.frames.push_back(Frame {
            kind,
            t_us,
            key,
            data,
        });
        self.trim(t_us);
    }

    /// Drop what the window no longer needs: everything before the newest keyframe that is at
    /// or before the window's start (and anything over the byte cap, keyframe by keyframe). Runs
    /// for every frame, so it looks only at the next keyframe, not through the whole window.
    fn trim(&mut self, now_us: u64) {
        let start = now_us.saturating_sub(self.window_us);
        while self.keys.get(1).is_some_and(|t| *t <= start) {
            self.drop_to_next_key();
        }
        while self.bytes > MAX_BYTES && self.keys.len() > 1 {
            self.drop_to_next_key();
        }
    }

    /// Drops the first keyframe and what follows it, up to the next keyframe.
    fn drop_to_next_key(&mut self) {
        self.keys.pop_front();
        if let Some(first) = self.frames.pop_front() {
            self.bytes -= first.data.len();
        }
        while self
            .frames
            .front()
            .is_some_and(|f| !(f.kind == KIND_VIDEO && f.key))
        {
            if let Some(f) = self.frames.pop_front() {
                self.bytes -= f.data.len();
            }
        }
    }

    pub fn frames(&self) -> impl Iterator<Item = &Frame> {
        self.frames.iter()
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The snapshot as sent to the page (see the module doc). Up to [`MAX_BYTES`] of copying: run it
/// away from the lock the stream thread needs for every frame, on `Replay::frames().cloned()`.
pub fn encode(video_header: &[u8], audio_header: Option<&[u8]>, frames: &[Frame]) -> Vec<u8> {
    let bytes: usize = frames.iter().map(|f| f.data.len()).sum();
    let mut out = Vec::with_capacity(bytes + frames.len() * 14 + 64);
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
    for f in frames {
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
        r.push(KIND_AUDIO, 0, false, Bytes::from_static(b"a"));
        r.push(KIND_VIDEO, 0, false, Bytes::from_static(b"d"));
        assert_eq!(r.frames().count(), 0);
        // Keyframes at 0, 10, 20, 30 s, a delta frame and audio each second.
        for s in 0..=32u64 {
            let t = s * 1_000_000;
            r.push(KIND_VIDEO, t, s % 10 == 0, Bytes::from_static(b"vvvv"));
            r.push(KIND_AUDIO, t, false, Bytes::from_static(b"aa"));
        }
        // At 32 s the window starts at 17 s; the newest keyframe at or before it is 10 s.
        let k = kinds(&r);
        assert_eq!(k.first(), Some(&(KIND_VIDEO, 10, true)));
        assert_eq!(k.last(), Some(&(KIND_AUDIO, 32, false)));
        assert_eq!(r.bytes(), 23 * 6);
    }

    #[test]
    fn over_the_byte_cap_whole_keyframe_runs_go_first() {
        let mut r = Replay::new(60);
        let big = Bytes::from(vec![0u8; MAX_BYTES / 2]);
        r.push(KIND_VIDEO, 0, true, big.clone());
        r.push(KIND_VIDEO, 2, true, big.clone());
        assert_eq!(r.frames().count(), 2, "right at the cap, nothing goes yet");
        // One byte over: the first keyframe goes with what depends on it.
        r.push(KIND_VIDEO, 3, false, Bytes::from_static(b"d"));
        assert_eq!(r.frames().next().map(|f| f.t_us), Some(2));
        assert_eq!(r.frames().count(), 2);
        assert_eq!(r.bytes(), MAX_BYTES / 2 + 1);
        // A single keyframe over the cap stays: there is nothing to cut back to.
        let mut one = Replay::new(60);
        one.push(KIND_VIDEO, 0, true, Bytes::from(vec![0u8; MAX_BYTES + 1]));
        assert_eq!(one.frames().count(), 1);
    }

    #[test]
    fn a_keyframe_is_asked_for_every_few_seconds() {
        let mut r = Replay::new(30);
        assert!(r.wants_keyframe(0));
        r.push(KIND_VIDEO, 1_000_000, true, Bytes::from_static(b"k"));
        assert!(!r.wants_keyframe(5_000_000));
        assert!(r.wants_keyframe(6_000_000));
    }

    #[test]
    fn the_snapshot_wire_form() {
        let mut r = Replay::new(30);
        r.push(KIND_VIDEO, 7, true, Bytes::from_static(b"K"));
        r.push(KIND_AUDIO, 9, false, Bytes::from_static(b"A"));
        let frames: Vec<Frame> = r.frames().cloned().collect();
        let b = encode(b"VH", Some(b"AH"), &frames);
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
