//! Play Rooms voice: the frames on the wire, a listener's outgoing frames and bundles, and how
//! each person's voice follows their connection (join_comms.rs decides who hears whom).
//!
//! **Forward, never mix.** The engine passes each speaker's Opus frames to each listener as they
//! came (Opus, `signal: voice`, mono, wideband, 20 ms, DTX + in-band FEC); listeners decode and mix.
//! The host's CPU only copies bytes.
//!
//! **Wire (binary, both on `/api/join/ws`, the internet data channel and the host page's
//! `/api/ps5/rp/ws`):**
//!
//! - up, a speaker's frame: `[0x0A][seq u16 BE][level u8][Opus, 1–255 bytes]`. `level` is the
//!   frame's loudness, 0 (silence) to 255 (loudest); it picks the quietest speaker when the room's
//!   voice budget is full. Clients gate on their own voice-activity detection and send nothing
//!   while silent.
//! - down, one bundle per listener per tick: `[0x0B][n u8]` then `n` times
//!   `[speaker id u8][seq u16 BE][len u8][Opus, len bytes]`. A tick is 20 ms, or 40 ms when the
//!   listener's link is congested (their route is Playable or their picture queue is not empty), and
//!   then a speaker's two frames ride in one bundle. A bundle goes at once when every speaker the
//!   listener hears has a frame in it (the first frame never waits a tick behind nothing).
//!
//! (The design's draft numbered these 6 and 7; 6 is the haptics frame and 7 the timed picture on
//! these sockets already, so voice takes the next free kinds. docs/ROOMS_DEVIATIONS.md.)
//!
//! **Never ahead of the game.** A listener's frames wait at most [`MAX_AGE`] (60 ms, three
//! 20 ms bundles): older ones are dropped, never queued behind pictures. The guest's socket
//! loop sends voice only when no picture is ready, and chat only when no voice is.
//!
//! **Adaptive by route tier** ([`Adapt`]): Excellent/Good 24 kb/s at 20 ms, Playable 12 kb/s at
//! 40 ms, Poor paused both ways. A drop applies at once; going back up needs 10 s at the better
//! tier. Voice that seems to push the route down (the tier fell within 10 s of voice starting or
//! rising) steps down one more level for 30 s; a second time for 5 minutes.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::join_route::Tier;

/// A speaker's frame, up.
pub(crate) const KIND_VOICE_UP: u8 = 0x0A;
/// A listener's bundle, down.
pub(crate) const KIND_VOICE_DOWN: u8 = 0x0B;
/// The largest Opus frame (its length is one byte in a bundle).
pub(crate) const OPUS_MAX: usize = 255;
/// Speakers one listener hears at once; a fourth waits until one stops.
pub(crate) const MAX_SPEAKERS: usize = 3;
/// A speaker keeps their place (and counts as speaking) this long after their last frame; also
/// push-to-talk's tail after the key comes up.
pub(crate) const HANGOVER: Duration = Duration::from_millis(300);
/// A listener's frame older than this is dropped (three 20 ms bundles).
pub(crate) const MAX_AGE: Duration = Duration::from_millis(60);
/// Frames waiting for one listener at most (3 speakers × 2 frames, and a little).
pub(crate) const PENDING_MAX: usize = 9;
/// The room's voice going out, at most (bits per second): past it the quietest speaker is dropped.
pub(crate) const ROOM_CAP_BPS: u64 = 800_000;
/// A speaker's frames: a burst (a client catching up after a hiccup) and the steady rate (50 a
/// second is 20 ms frames; a little over for jitter).
pub(crate) const FRAMES_BURST: f64 = 25.0;
pub(crate) const FRAMES_PER_S: f64 = 60.0;
/// The bytes a frame costs in a bundle beside its Opus (speaker, seq, len).
pub(crate) const ENTRY_OVERHEAD: usize = 4;
/// Going back up a level needs this long at the better tier.
pub(crate) const RISE_AFTER: Duration = Duration::from_secs(10);
/// A tier drop this soon after voice started or rose is voice's fault.
pub(crate) const BLAME_WITHIN: Duration = Duration::from_secs(10);
/// How long the step-down holds, the first time and after that.
pub(crate) const STEP_DOWN_FOR: Duration = Duration::from_secs(30);
pub(crate) const STEP_DOWN_AGAIN_FOR: Duration = Duration::from_secs(300);

/// A speaker's frame as it came up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Up<'a> {
    pub(crate) seq: u16,
    pub(crate) level: u8,
    pub(crate) opus: &'a [u8],
}

/// `[0x0A][seq u16 BE][level u8][Opus 1–255]`, or None.
pub(crate) fn parse_up(b: &[u8]) -> Option<Up<'_>> {
    if b.first() != Some(&KIND_VOICE_UP) || b.len() < 5 || b.len() > 4 + OPUS_MAX {
        return None;
    }
    Some(Up {
        seq: u16::from_be_bytes([b[1], b[2]]),
        level: b[3],
        opus: &b[4..],
    })
}

/// A test client's frame.
#[cfg(test)]
pub(crate) fn up_frame(seq: u16, level: u8, opus: &[u8]) -> Vec<u8> {
    let mut v = vec![KIND_VOICE_UP];
    v.extend_from_slice(&seq.to_be_bytes());
    v.push(level);
    v.extend_from_slice(opus);
    v
}

/// One frame on its way to a listener.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub(crate) sid: u8,
    pub(crate) seq: u16,
    pub(crate) opus: axum::body::Bytes,
    pub(crate) at: Instant,
}

/// `[0x0B][n][(sid, seq, len, Opus)…]`.
pub(crate) fn bundle<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> Vec<u8> {
    let mut v = vec![KIND_VOICE_DOWN, 0];
    let mut n = 0u8;
    for f in frames {
        if n == u8::MAX {
            break;
        }
        v.push(f.sid);
        v.extend_from_slice(&f.seq.to_be_bytes());
        v.push(f.opus.len() as u8);
        v.extend_from_slice(&f.opus);
        n += 1;
    }
    v[1] = n;
    v
}

/// A bundle's frames as `(sid, seq, Opus)` (clients read it so).
#[cfg(test)]
pub(crate) fn parse_bundle(b: &[u8]) -> Option<Vec<(u8, u16, &[u8])>> {
    if b.first() != Some(&KIND_VOICE_DOWN) {
        return None;
    }
    let n = *b.get(1)? as usize;
    let mut out = Vec::with_capacity(n);
    let mut i = 2;
    for _ in 0..n {
        let h = b.get(i..i + 4)?;
        let len = h[3] as usize;
        let opus = b.get(i + 4..i + 4 + len)?;
        out.push((h[0], u16::from_be_bytes([h[1], h[2]]), opus));
        i += 4 + len;
    }
    (i == b.len()).then_some(out)
}

/// A listener's frames waiting for their next bundle.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    frames: VecDeque<Frame>,
    /// Dropped for age or room (counted for the stats).
    pub(crate) dropped: u64,
}

/// What [`Pending::take`] decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Take {
    /// A bundle to send now.
    Bundle(Vec<u8>),
    /// Nothing yet: the next bundle is due then.
    Wait(Instant),
    Empty,
}

impl Pending {
    pub(crate) fn push(&mut self, f: Frame) {
        if self.frames.len() >= PENDING_MAX {
            self.frames.pop_front();
            self.dropped += 1;
        }
        self.frames.push_back(f);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }

    /// The waiting frames as `(speaker, seq)`.
    #[cfg(test)]
    pub(crate) fn sids(&self) -> Vec<(u8, u16)> {
        self.frames.iter().map(|f| (f.sid, f.seq)).collect()
    }

    /// The bundle due now, if any. `speakers`: the speakers this listener hears now (a bundle
    /// with a frame — two when `congested` — from each goes at once); `congested`: 40 ms ticks.
    pub(crate) fn take(&mut self, now: Instant, speakers: &[u8], congested: bool) -> Take {
        while self
            .frames
            .front()
            .is_some_and(|f| now.saturating_duration_since(f.at) > MAX_AGE)
        {
            self.frames.pop_front();
            self.dropped += 1;
        }
        let Some(first) = self.frames.front() else {
            return Take::Empty;
        };
        let (tick, per) = if congested {
            (Duration::from_millis(40), 2)
        } else {
            (Duration::from_millis(20), 1)
        };
        let due = first.at + tick;
        let full = !speakers.is_empty()
            && speakers
                .iter()
                .all(|s| self.frames.iter().filter(|f| f.sid == *s).count() >= per);
        if full || now >= due {
            let b = bundle(self.frames.iter());
            self.frames.clear();
            Take::Bundle(b)
        } else {
            Take::Wait(due)
        }
    }

    pub(crate) fn clear(&mut self) {
        self.frames.clear();
    }
}

/// How much voice a person sends and gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    /// Nothing either way (Poor): "Voice paused: your connection needs it for the game".
    Paused,
    /// 12 kb/s, 40 ms (Playable).
    Low,
    /// 24 kb/s, 20 ms (Excellent, Good, or not measured yet).
    Full,
}

impl Level {
    pub(crate) fn kbps(self) -> u32 {
        match self {
            Level::Paused => 0,
            Level::Low => 12,
            Level::Full => 24,
        }
    }

    pub(crate) fn frame_ms(self) -> u32 {
        match self {
            Level::Paused => 0,
            Level::Low => 40,
            Level::Full => 20,
        }
    }

    pub(crate) fn down(self) -> Level {
        match self {
            Level::Full => Level::Low,
            _ => Level::Paused,
        }
    }

    /// What a route tier allows (None: not measured yet).
    pub(crate) fn of(tier: Option<Tier>) -> Level {
        match tier {
            None | Some(Tier::Excellent | Tier::Good) => Level::Full,
            Some(Tier::Playable) => Level::Low,
            Some(Tier::Poor | Tier::Down) => Level::Paused,
        }
    }
}

/// A person's voice level as their route tier moves.
#[derive(Clone, Debug)]
pub(crate) struct Adapt {
    level: Level,
    /// A better level wanted, since then.
    better: Option<(Level, Instant)>,
    /// When voice last started (or the level rose): a tier drop soon after is voice's fault.
    started: Option<Instant>,
    /// Stepped down for voice's sake: at most this level until then.
    cap: Option<(Level, Instant)>,
    blamed: u8,
}

impl Default for Adapt {
    fn default() -> Self {
        Adapt {
            level: Level::Full,
            better: None,
            started: None,
            cap: None,
            blamed: 0,
        }
    }
}

impl Adapt {
    pub(crate) fn level(&self) -> Level {
        self.level
    }

    /// Stepped down for voice's sake right now.
    pub(crate) fn stepped(&self, now: Instant) -> bool {
        self.cap.is_some_and(|(_, until)| now < until)
    }

    /// The person's voice started (their first frame after a silence).
    pub(crate) fn voice_started(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// Their tier now (None: not known). True when the level changed.
    pub(crate) fn on_tier(&mut self, tier: Option<Tier>, now: Instant) -> bool {
        let mut want = Level::of(tier);
        if let Some((cap, until)) = self.cap {
            if now < until {
                want = want.min(cap);
            } else {
                self.cap = None;
            }
        }
        let was = self.level;
        if want < self.level {
            self.better = None;
            let blame = self
                .started
                .is_some_and(|t| now.saturating_duration_since(t) <= BLAME_WITHIN)
                && Level::of(tier) < was;
            if blame {
                self.blamed = self.blamed.saturating_add(1);
                let hold = if self.blamed > 1 {
                    STEP_DOWN_AGAIN_FOR
                } else {
                    STEP_DOWN_FOR
                };
                let cap = was.down();
                self.cap = Some((cap, now + hold));
                want = want.min(cap);
                self.started = None;
            }
            self.level = want;
        } else if want > self.level {
            match self.better {
                Some((l, since)) if l == want => {
                    if now.saturating_duration_since(since) >= RISE_AFTER {
                        self.level = want;
                        self.better = None;
                        self.started = Some(now);
                    }
                }
                _ => self.better = Some((want, now)),
            }
        } else {
            self.better = None;
        }
        self.level != was
    }
}

/// Bytes in the last whole second and this one (for kb/s and the room's budget).
#[derive(Clone, Debug, Default)]
pub(crate) struct Rate {
    sec: u64,
    cur: u64,
    prev: u64,
    pub(crate) total: u64,
}

impl Rate {
    fn roll(&mut self, sec: u64) {
        if sec != self.sec {
            self.prev = if sec == self.sec + 1 { self.cur } else { 0 };
            self.cur = 0;
            self.sec = sec;
        }
    }

    /// Counts `n` bytes at `sec` (seconds since some fixed start).
    pub(crate) fn add(&mut self, sec: u64, n: usize) {
        self.roll(sec);
        self.cur += n as u64;
        self.total += n as u64;
    }

    /// kbit/s over the last whole second.
    pub(crate) fn kbps(&mut self, sec: u64) -> u64 {
        self.roll(sec);
        self.prev * 8 / 1000
    }

    /// Bits per second now: the larger of the last second and this one so far.
    pub(crate) fn bps(&mut self, sec: u64) -> u64 {
        self.roll(sec);
        self.prev.max(self.cur) * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sid: u8, seq: u16, at: Instant) -> Frame {
        Frame {
            sid,
            seq,
            opus: axum::body::Bytes::from(vec![0xAB; 60]),
            at,
        }
    }

    #[test]
    fn frames_up_and_bundles_down_parse_and_refuse_junk() {
        let f = up_frame(513, 90, &[1, 2, 3]);
        assert_eq!(
            parse_up(&f),
            Some(Up {
                seq: 513,
                level: 90,
                opus: &[1, 2, 3]
            })
        );
        assert_eq!(parse_up(&[KIND_VOICE_UP, 0, 1, 2]), None, "no Opus");
        assert_eq!(parse_up(&up_frame(1, 1, &[0; 256])), None, "too big");
        assert_eq!(parse_up(&[0x01, 0, 1, 2, 3]), None, "a pad frame");
        let t = Instant::now();
        let b = bundle([frame(1, 7, t), frame(3, 9, t)].iter());
        assert_eq!(b.len(), 2 + 2 * (4 + 60));
        let got = parse_bundle(&b).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].0, got[0].1, got[0].2.len()), (1, 7, 60));
        assert_eq!((got[1].0, got[1].1), (3, 9));
        assert!(parse_bundle(&b[..b.len() - 1]).is_none());
    }

    #[test]
    fn a_bundle_goes_at_once_when_every_speaker_is_in_it_else_on_the_tick() {
        let t = Instant::now();
        let mut p = Pending::default();
        assert_eq!(p.take(t, &[1], false), Take::Empty);
        // One speaker: their frame goes at once (no tick behind nothing).
        p.push(frame(1, 1, t));
        assert!(matches!(p.take(t, &[1], false), Take::Bundle(_)));
        // Two speakers: the first waits for the second, at most a tick.
        p.push(frame(1, 2, t));
        assert_eq!(
            p.take(t, &[1, 2], false),
            Take::Wait(t + Duration::from_millis(20))
        );
        p.push(frame(2, 1, t + Duration::from_millis(5)));
        match p.take(t + Duration::from_millis(5), &[1, 2], false) {
            Take::Bundle(b) => assert_eq!(parse_bundle(&b).unwrap().len(), 2),
            other => panic!("{other:?}"),
        }
        // The other speaker went quiet: the tick sends what there is.
        p.push(frame(1, 3, t));
        assert!(matches!(p.take(t, &[1, 2], false), Take::Wait(_)));
        assert!(matches!(
            p.take(t + Duration::from_millis(20), &[1, 2], false),
            Take::Bundle(_)
        ));
        // Congested: two frames a speaker, 40 ms.
        p.push(frame(1, 4, t));
        assert_eq!(
            p.take(t, &[1], true),
            Take::Wait(t + Duration::from_millis(40))
        );
        p.push(frame(1, 5, t + Duration::from_millis(20)));
        match p.take(t + Duration::from_millis(20), &[1], true) {
            Take::Bundle(b) => assert_eq!(parse_bundle(&b).unwrap().len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn old_frames_are_dropped_never_queued() {
        let t = Instant::now();
        let mut p = Pending::default();
        // A listener whose loop was busy for 100 ms: the frames older than 60 ms go.
        for i in 0..5u16 {
            p.push(frame(1, i, t + Duration::from_millis(20 * i as u64)));
        }
        match p.take(t + Duration::from_millis(100), &[1], false) {
            Take::Bundle(b) => {
                let got = parse_bundle(&b).unwrap();
                let seqs: Vec<u16> = got.iter().map(|f| f.1).collect();
                assert_eq!(seqs, vec![2, 3, 4]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.dropped, 2);
        // Never more than PENDING_MAX wait.
        for i in 0..30u16 {
            p.push(frame((i % 3) as u8, i, t));
        }
        assert_eq!(p.len(), PENDING_MAX);
        // All too old: nothing goes.
        assert_eq!(
            p.take(t + Duration::from_secs(1), &[0, 1, 2], false),
            Take::Empty
        );
    }

    #[test]
    fn voice_follows_the_route_tier_with_hysteresis_and_steps_down_when_it_hurts() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut a = Adapt::default();
        assert_eq!(a.level(), Level::Full);
        assert!(!a.on_tier(Some(Tier::Good), s(0)));
        // Playable: 12 kb/s at once; Poor: paused at once.
        assert!(a.on_tier(Some(Tier::Playable), s(1)));
        assert_eq!((a.level().kbps(), a.level().frame_ms()), (12, 40));
        assert!(a.on_tier(Some(Tier::Poor), s(2)));
        assert_eq!(a.level(), Level::Paused);
        // Better again: only after 10 s at the better tier.
        assert!(!a.on_tier(Some(Tier::Excellent), s(3)));
        assert!(!a.on_tier(Some(Tier::Excellent), s(12)));
        assert!(a.on_tier(Some(Tier::Excellent), s(13)));
        assert_eq!(a.level(), Level::Full);
        // A wobble resets the wait.
        let mut b = Adapt::default();
        b.on_tier(Some(Tier::Playable), s(0));
        b.on_tier(Some(Tier::Good), s(1));
        b.on_tier(Some(Tier::Playable), s(5));
        b.on_tier(Some(Tier::Good), s(6));
        assert!(!b.on_tier(Some(Tier::Good), s(12)));
        assert!(b.on_tier(Some(Tier::Good), s(16)));

        // Voice starts and the tier falls within 10 s: one level below where it was (24 → 12),
        // held for 30 s even though the route recovers.
        let mut c = Adapt::default();
        c.on_tier(Some(Tier::Excellent), s(0));
        c.voice_started(s(1));
        c.on_tier(Some(Tier::Good), s(2));
        assert_eq!(c.level(), Level::Full, "Good still allows full voice");
        c.voice_started(s(3));
        c.on_tier(Some(Tier::Playable), s(5));
        assert_eq!(c.level(), Level::Low);
        assert!(c.stepped(s(5)));
        for n in 6..=34 {
            c.on_tier(Some(Tier::Excellent), s(n));
            assert_eq!(c.level(), Level::Low, "held at {n}");
        }
        // Then it tries again (10 s at the better tier after the hold).
        for n in 35..=46 {
            c.on_tier(Some(Tier::Excellent), s(n));
        }
        assert_eq!(c.level(), Level::Full);
        // Hurts again right after rising: held for 5 minutes this time.
        c.on_tier(Some(Tier::Playable), s(47));
        assert_eq!(c.level(), Level::Low);
        c.on_tier(Some(Tier::Excellent), s(200));
        c.on_tier(Some(Tier::Excellent), s(250));
        assert_eq!(c.level(), Level::Low);
        assert!(c.stepped(s(300)));
    }

    #[test]
    fn rates_count_the_last_whole_second() {
        let mut r = Rate::default();
        r.add(10, 3000);
        assert_eq!(r.kbps(10), 0);
        assert_eq!(r.bps(10), 24_000);
        r.add(11, 1000);
        assert_eq!(r.kbps(11), 24);
        assert_eq!(r.kbps(13), 0, "a gap is silence");
        assert_eq!(r.total, 4000);
    }
}
