//! Limits sized for people, tightened only for robots.
//!
//! Every per-client limit the rooms features add is a token bucket: a generous burst (well above
//! what a person does: mashing refresh on the room list, opening ten rooms' details, retrying a
//! password a few times) that refills smoothly. A request past it is slowed down, not punished:
//! HTTP 429 with `Retry-After`, which the apps honour silently. Only a *sustained* machine-like
//! pattern escalates: each refusal adds to a client's abuse score, the score drains by one a
//! second, and only a score that stays high for [`Escalation::sustain`] (a steady flood, not one
//! burst) turns into a short temporary block, doubling on repeats up to a cap and forgotten after
//! a clean hour. One burst, however fast, never blocks anyone.
//!
//! Keys are [`IpKey`]s (an IPv4 address or an IPv6 /64), in memory only, never logged.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::limits::IpKey;

/// A bucket's shape: `burst` tokens at most, refilled at `per_sec`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub burst: f64,
    pub per_sec: f64,
}

impl Rule {
    pub const fn new(burst: f64, per_sec: f64) -> Self {
        Self { burst, per_sec }
    }

    /// `n` tokens a minute.
    pub const fn per_min(burst: f64, n: f64) -> Self {
        Self {
            burst,
            per_sec: n / 60.0,
        }
    }
}

/// A token bucket. Starts full.
#[derive(Clone, Debug)]
pub struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    pub fn full(rule: Rule, now: Instant) -> Self {
        Self {
            tokens: rule.burst,
            last: now,
        }
    }

    fn refill(&mut self, rule: Rule, now: Instant) {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + dt * rule.per_sec).min(rule.burst);
        self.last = now;
    }

    /// Takes one token, or says how long until one is there.
    pub fn take(&mut self, rule: Rule, now: Instant) -> Result<(), Duration> {
        self.refill(rule, now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let need = (1.0 - self.tokens) / rule.per_sec.max(1e-9);
        Err(Duration::from_secs_f64(need.min(86_400.0)))
    }

    /// Full again (a client this bucket can forget).
    pub fn is_full(&self, rule: Rule, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens + dt * rule.per_sec >= rule.burst
    }
}

/// The classes of request each client has a bucket for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `GET /rooms`.
    List,
    /// `GET /rooms/status`.
    Status,
    /// `GET /rooms/<id>/preview`.
    Preview,
    /// Knocks on lobby rooms, every room together.
    Knock,
    /// WebSocket upgrades.
    Connect,
}

impl Class {
    pub const ALL: [Class; 5] = [
        Class::List,
        Class::Status,
        Class::Preview,
        Class::Knock,
        Class::Connect,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Class::List => "list",
            Class::Status => "status",
            Class::Preview => "preview",
            Class::Knock => "knock",
            Class::Connect => "connect",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// The bucket for each class (indexed by [`Class::index`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules(pub [Rule; 5]);

impl Default for Rules {
    fn default() -> Self {
        Rules([
            // List: 30 quick refreshes, then one a second. Our apps fetch on open and on a pull,
            // and the page every 30 s while visible; nobody reads a list faster than this.
            Rule::new(30.0, 1.0),
            // Friends lookup: the Play screen asks once a minute; 20 at once, one per 3 s.
            Rule::new(20.0, 1.0 / 3.0),
            // Details: opening ten rooms' details in a row, then one each 6 s.
            Rule::new(10.0, 1.0 / 6.0),
            // Knocks, all rooms together: a few rooms tried and a password retried, then one a
            // minute.
            Rule::per_min(10.0, 1.0),
            // WebSocket upgrades: an engine and a few guests reconnecting at once, then two a
            // second.
            Rule::new(120.0, 2.0),
        ])
    }
}

/// When refusals turn into a temporary block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Escalation {
    /// Abuse score that may block (each refusal adds one; it drains by one a second).
    pub score: f64,
    /// ... and only when refusals have kept coming this long without the score draining to zero:
    /// a steady flood, never one burst.
    pub sustain: Duration,
    /// The first block; each repeat within [`Escalation::forget`] doubles it up to `max_block`.
    pub block: Duration,
    pub max_block: Duration,
    /// A client clean this long after a block starts again from `block`.
    pub forget: Duration,
}

impl Default for Escalation {
    fn default() -> Self {
        Self {
            score: 120.0,
            sustain: Duration::from_secs(30),
            block: Duration::from_secs(120),
            max_block: Duration::from_secs(30 * 60),
            forget: Duration::from_secs(3600),
        }
    }
}

#[derive(Clone, Debug)]
struct Client {
    buckets: [Option<Bucket>; 5],
    score: f64,
    score_at: Instant,
    streak_since: Option<Instant>,
    blocked_until: Option<Instant>,
    blocks: u32,
}

/// What a check answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Slow down: try again after this long.
    Wait(Duration),
    /// Temporarily blocked for this long more.
    Blocked(Duration),
}

impl Verdict {
    pub fn is_ok(self) -> bool {
        self == Verdict::Ok
    }

    /// Seconds for `Retry-After` (at least 1).
    pub fn retry_secs(self) -> u64 {
        match self {
            Verdict::Ok => 0,
            Verdict::Wait(d) | Verdict::Blocked(d) => d.as_secs_f64().ceil().max(1.0) as u64,
        }
    }
}

/// Per-client buckets and abuse scores, bounded: when `max_keys` clients are tracked a new one is
/// let through unmetered rather than refused (a flood of spoofed-looking keys must not lock out
/// real people), and [`Throttle::sweep`] forgets clients whose buckets are full again.
pub struct Throttle {
    map: HashMap<IpKey, Client>,
    pub rules: Rules,
    pub escalation: Escalation,
    max_keys: usize,
    /// Temporary blocks given, in all.
    pub blocks_total: u64,
}

impl Throttle {
    pub fn new(rules: Rules, escalation: Escalation, max_keys: usize) -> Self {
        Self {
            map: HashMap::new(),
            rules,
            escalation,
            max_keys,
            blocks_total: 0,
        }
    }

    fn client(&mut self, key: IpKey, now: Instant) -> Option<&mut Client> {
        if !self.map.contains_key(&key) && self.map.len() >= self.max_keys {
            return None;
        }
        Some(self.map.entry(key).or_insert_with(|| Client {
            buckets: Default::default(),
            score: 0.0,
            score_at: now,
            streak_since: None,
            blocked_until: None,
            blocks: 0,
        }))
    }

    /// The client's temporary block, if one is running.
    pub fn blocked(&mut self, key: IpKey, now: Instant) -> Option<Duration> {
        let c = self.map.get(&key)?;
        c.blocked_until
            .filter(|t| *t > now)
            .map(|t| t.duration_since(now))
    }

    /// One request of `class` from `key`.
    pub fn check(&mut self, key: IpKey, class: Class, now: Instant) -> Verdict {
        let rule = self.rules.0[class.index()];
        let esc = self.escalation;
        let Some(c) = self.client(key, now) else {
            return Verdict::Ok;
        };
        if let Some(t) = c.blocked_until {
            if t > now {
                return Verdict::Blocked(t - now);
            }
        }
        let b = c.buckets[class.index()].get_or_insert_with(|| Bucket::full(rule, now));
        match b.take(rule, now) {
            Ok(()) => Verdict::Ok,
            Err(wait) => {
                if self.strike(key, now) {
                    let left = self
                        .map
                        .get(&key)
                        .and_then(|c| c.blocked_until)
                        .map(|t| t - now)
                        .unwrap_or(esc.block);
                    return Verdict::Blocked(left);
                }
                Verdict::Wait(wait)
            }
        }
    }

    /// Counts one refusal (or one piece of bad behaviour) against `key`; true when it started a
    /// temporary block.
    pub fn strike(&mut self, key: IpKey, now: Instant) -> bool {
        let esc = self.escalation;
        let Some(c) = self.client(key, now) else {
            return false;
        };
        let drained = now.saturating_duration_since(c.score_at).as_secs_f64();
        c.score = (c.score - drained).max(0.0);
        c.score_at = now;
        if c.score <= 0.0 {
            c.streak_since = None;
        }
        c.score += 1.0;
        let since = *c.streak_since.get_or_insert(now);
        if c.score >= esc.score && now.duration_since(since) >= esc.sustain {
            // Forgiven after a clean hour.
            if let Some(t) = c.blocked_until {
                if now.saturating_duration_since(t) >= esc.forget {
                    c.blocks = 0;
                }
            }
            let factor = 1u32 << c.blocks.min(8);
            let d = (esc.block * factor).min(esc.max_block);
            c.blocked_until = Some(now + d);
            c.blocks += 1;
            c.score = 0.0;
            c.streak_since = None;
            self.blocks_total += 1;
            return true;
        }
        false
    }

    /// Clients blocked right now.
    pub fn blocked_count(&self, now: Instant) -> usize {
        self.map
            .values()
            .filter(|c| c.blocked_until.is_some_and(|t| t > now))
            .count()
    }

    /// Forgets clients with every bucket full, no score and no block in the last `forget`.
    pub fn sweep(&mut self, now: Instant) {
        let rules = self.rules;
        let esc = self.escalation;
        self.map.retain(|_, c| {
            let drained = now.saturating_duration_since(c.score_at).as_secs_f64();
            let busy = c
                .buckets
                .iter()
                .enumerate()
                .any(|(i, b)| b.as_ref().is_some_and(|b| !b.is_full(rules.0[i], now)));
            let scored = c.score - drained > 0.0;
            let remembered = c
                .blocked_until
                .is_some_and(|t| now.saturating_duration_since(t) < esc.forget);
            busy || scored || remembered
        });
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> IpKey {
        IpKey::V4(n)
    }

    #[test]
    fn bucket_bursts_then_refills_smoothly() {
        let t0 = Instant::now();
        let r = Rule::new(3.0, 1.0);
        let mut b = Bucket::full(r, t0);
        for _ in 0..3 {
            assert!(b.take(r, t0).is_ok());
        }
        let wait = b.take(r, t0).unwrap_err();
        assert!(wait <= Duration::from_secs(1) && wait > Duration::from_millis(900));
        assert!(b.take(r, t0 + Duration::from_millis(1000)).is_ok());
        assert!(b.is_full(r, t0 + Duration::from_secs(5)));
    }

    /// A person mashing refresh (5 a second for 20 seconds: far more than anyone does) is slowed
    /// down, never blocked.
    #[test]
    fn a_burst_is_slowed_never_blocked() {
        let t0 = Instant::now();
        let mut t = Throttle::new(Rules::default(), Escalation::default(), 100);
        let mut waits = 0;
        for i in 0..100u64 {
            match t.check(key(1), Class::List, t0 + Duration::from_millis(i * 200)) {
                Verdict::Ok => {}
                Verdict::Wait(_) => waits += 1,
                Verdict::Blocked(_) => panic!("blocked a person at {i}"),
            }
        }
        assert!(waits > 0);
        // Even a single burst of 1,000 in one instant (a script, once) is not a block.
        let mut t = Throttle::new(Rules::default(), Escalation::default(), 100);
        for _ in 0..1000 {
            assert!(!matches!(
                t.check(key(2), Class::List, t0),
                Verdict::Blocked(_)
            ));
        }
    }

    /// A steady flood (20 a second for a minute) is blocked once it has gone on past `sustain`,
    /// briefly, and a repeat doubles it.
    #[test]
    fn a_sustained_flood_is_blocked_briefly_then_longer() {
        let t0 = Instant::now();
        let esc = Escalation::default();
        let mut t = Throttle::new(Rules::default(), esc, 100);
        let mut blocked_at = None;
        for i in 0..1200u64 {
            let now = t0 + Duration::from_millis(i * 50);
            if let Verdict::Blocked(d) = t.check(key(3), Class::List, now) {
                assert_eq!(d, esc.block);
                blocked_at = Some(now);
                break;
            }
        }
        let at = blocked_at.expect("a flood is blocked");
        assert!(at.duration_since(t0) >= esc.sustain);
        assert_eq!(t.blocked_count(at), 1);
        // Other clients are untouched.
        assert!(t.check(key(4), Class::List, at).is_ok());
        // After the block: the flood again gets a block twice as long.
        let mut now = at + esc.block;
        let second = loop {
            now += Duration::from_millis(50);
            if let Verdict::Blocked(d) = t.check(key(3), Class::List, now) {
                break d;
            }
        };
        assert_eq!(second, esc.block * 2);
        assert_eq!(t.blocks_total, 2);
    }

    #[test]
    fn the_table_is_bounded_and_swept() {
        let t0 = Instant::now();
        let mut t = Throttle::new(Rules::default(), Escalation::default(), 2);
        assert!(t.check(key(1), Class::List, t0).is_ok());
        assert!(t.check(key(2), Class::List, t0).is_ok());
        // Full table: a new client goes through unmetered rather than refused.
        assert!(t.check(key(3), Class::List, t0).is_ok());
        assert_eq!(t.len(), 2);
        t.sweep(t0 + Duration::from_secs(60));
        assert!(t.is_empty());
        assert_eq!(Verdict::Wait(Duration::from_millis(1)).retry_secs(), 1);
    }
}
