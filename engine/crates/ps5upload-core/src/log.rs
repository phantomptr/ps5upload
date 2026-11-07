//! Pluggable log sink for ps5upload-core, and the log levels every part of PS5Upload shares.
//!
//! Core is a library crate, so it can't know anything about where logs
//! should land (stderr for the CLI, an in-memory ring for the engine
//! sidecar, a Tauri event for the desktop client). Each consumer
//! installs a sink via `set_sink` / `set_leveled_sink` at startup; anything core emits via
//! `core_log!` or `clog!` goes through it.
//!
//! Default sink is `eprintln!` so tests and standalone usage still
//! produce visible output without any setup.
//!
//! # Levels
//!
//! One model for the app, the engine and the PS5 helper: `error`, `warn`, `info`, `debug`,
//! `trace`, chosen per category ([`CATEGORIES`]) as a global level plus per-category overrides
//! ([`Levels`]). A line is written when its level is at most the effective level of its category.
//! A level more verbose than `info` is a *raise*: it lasts until `until_ms` (at most
//! [`MAX_RAISE_MS`] from when it was set) and then falls back to `info`, so nobody stays verbose
//! by accident. `info`, `warn` and `error` settings never expire. The engine owns the setting
//! (`/api/logs/level`), keeps it on disk and hands it to the app and to each helper.
//!
//! The check is a couple of atomic loads when a line is not wanted: `clog!` does not even format
//! its arguments then.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

type Sink = Box<dyn Fn(&str) + Send + Sync + 'static>;

fn sink_cell() -> &'static OnceLock<Sink> {
    static CELL: OnceLock<Sink> = OnceLock::new();
    &CELL
}

/// Install a log sink. First call wins; subsequent calls are ignored —
/// callers that need to replace the sink should use a forwarding sink
/// of their own.
pub fn set_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = sink_cell().set(Box::new(f));
}

/// Emit an `info` line in the `general` category through the installed sink, or `eprintln!` if
/// none has been set yet. Safe to call before/without `set_sink`. Dropped when the level of
/// `general` is below `info`.
pub fn log(msg: &str) {
    if !enabled(Lv::Info, GENERAL) {
        return;
    }
    match sink_cell().get() {
        Some(f) => f(msg),
        None => eprintln!("[core] {msg}"),
    }
}

/// `format!`-style shortcut. Use this in core modules that want to
/// surface diagnostic info to whatever frontend has attached.
#[macro_export]
macro_rules! core_log {
    ($($arg:tt)*) => { $crate::log::log(&format!($($arg)*)) };
}

// ---- levels ----

/// A log level, least verbose first. The numbers are the wire form the helper takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lv {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Lv {
    pub const ALL: [Lv; 5] = [Lv::Error, Lv::Warn, Lv::Info, Lv::Debug, Lv::Trace];

    pub fn name(self) -> &'static str {
        match self {
            Lv::Error => "error",
            Lv::Warn => "warn",
            Lv::Info => "info",
            Lv::Debug => "debug",
            Lv::Trace => "trace",
        }
    }

    pub fn parse(s: &str) -> Option<Lv> {
        Lv::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(s.trim()))
    }

    fn from_u8(n: u8) -> Lv {
        match n {
            0 | 1 => Lv::Error,
            2 => Lv::Warn,
            3 => Lv::Info,
            4 => Lv::Debug,
            _ => Lv::Trace,
        }
    }
}

/// The category of a line with none of its own.
pub const GENERAL: &str = "general";

/// Every category, with what it covers. The app, the engine and the helper use the same names.
pub const CATEGORIES: &[(&str, &str)] = &[
    (GENERAL, "Anything not in another category"),
    ("api", "The engine's HTTP API: requests and failures"),
    (
        "connection",
        "Finding consoles, status checks, the helper coming and going",
    ),
    (
        "helper",
        "The PS5 helper: its calls, answers and own log lines",
    ),
    (
        "rp",
        "Remote Play: session stages, the console's answers, decoders, teardown",
    ),
    ("pairing", "Remote Play pairing and registration"),
    ("wake", "Waking a console from rest mode"),
    ("transfer", "Uploads, downloads and copies"),
    ("install", "Package installs"),
    ("cleanup", "Console clean-up"),
    ("files", "File operations on the console"),
    ("convert", "Converting games, images, packages and archives"),
    (
        "together",
        "Play Together, phone controllers and the watch party",
    ),
    ("golive", "Go live streaming"),
    ("mesh", "Devices sharing consoles: the mesh and Drive"),
    ("ui", "The app's own screens"),
];

/// The category's canonical name, or None for one that does not exist.
pub fn category(name: &str) -> Option<&'static str> {
    let name = name.trim();
    CATEGORIES
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(name))
        .map(|(c, _)| *c)
}

/// How long a raise lasts when no end is given.
pub const DEFAULT_RAISE_MS: u64 = 60 * 60 * 1000;
/// The longest a raise lasts.
pub const MAX_RAISE_MS: u64 = 24 * 60 * 60 * 1000;

/// The level setting: a global level and per-category overrides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Levels {
    pub level: Lv,
    #[serde(default)]
    pub categories: BTreeMap<String, Lv>,
    /// When a raise (anything above `info`) ends, Unix ms. None when nothing is raised.
    #[serde(default)]
    pub until_ms: Option<u64>,
}

impl Default for Levels {
    fn default() -> Self {
        Levels {
            level: Lv::Info,
            categories: BTreeMap::new(),
            until_ms: None,
        }
    }
}

impl Levels {
    /// Something is set above `info`.
    pub fn raised(&self) -> bool {
        self.level > Lv::Info || self.categories.values().any(|l| *l > Lv::Info)
    }

    fn raise_live(&self, now_ms: u64) -> bool {
        self.until_ms.is_some_and(|u| u > now_ms)
    }

    /// The level in force for `cat` at `now_ms`: its override or the global level, and a raise
    /// that has run out counts as `info`.
    pub fn effective(&self, cat: &str, now_ms: u64) -> Lv {
        let l = self.categories.get(cat).copied().unwrap_or(self.level);
        if l > Lv::Info && !self.raise_live(now_ms) {
            Lv::Info
        } else {
            l
        }
    }

    /// Unknown categories out, a raise given an end (the default when none was asked, never more
    /// than the longest), and no end when nothing is raised. Overrides equal to the global level
    /// are dropped.
    pub fn normalize(&mut self, now_ms: u64) {
        let global = self.level;
        self.categories = std::mem::take(&mut self.categories)
            .into_iter()
            .filter_map(|(c, l)| category(&c).map(|c| (c.to_string(), l)))
            .filter(|(_, l)| *l != global)
            .collect();
        if self.raised() {
            let max = now_ms + MAX_RAISE_MS;
            let until = self.until_ms.unwrap_or(now_ms + DEFAULT_RAISE_MS).min(max);
            self.until_ms = Some(until);
        } else {
            self.until_ms = None;
        }
    }

    /// An end more than [`MAX_RAISE_MS`] away is pulled in to that: the clock was set back since
    /// the raise began, and without this the raise would outlast the cap by as much. True when
    /// the end moved.
    pub fn clamp_end(&mut self, now_ms: u64) -> bool {
        let max = now_ms.saturating_add(MAX_RAISE_MS);
        match self.until_ms {
            Some(u) if u > max => {
                self.until_ms = Some(max);
                true
            }
            _ => false,
        }
    }

    /// A raise that has run out goes: raised levels fall back to `info`. True when anything changed.
    pub fn expire(&mut self, now_ms: u64) -> bool {
        if !self.raised() || self.raise_live(now_ms) {
            return false;
        }
        if self.level > Lv::Info {
            self.level = Lv::Info;
        }
        self.categories.retain(|_, l| *l <= Lv::Info);
        let global = self.level;
        self.categories.retain(|_, l| *l != global);
        self.until_ms = None;
        true
    }

    /// The effective level of every category at `now_ms`, for display and for the helper.
    pub fn effective_all(&self, now_ms: u64) -> BTreeMap<&'static str, Lv> {
        CATEGORIES
            .iter()
            .map(|(c, _)| (*c, self.effective(c, now_ms)))
            .collect()
    }
}

/// The current setting.
fn state() -> &'static RwLock<Levels> {
    static S: OnceLock<RwLock<Levels>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Levels::default()))
}

/// Lines at or below this level are always written (the lowest effective level, never above
/// `info`, so a raise that runs out is noticed by the slow path).
static FLOOR: AtomicU8 = AtomicU8::new(Lv::Info as u8);
/// No line above this level is written (the highest effective level).
static CEIL: AtomicU8 = AtomicU8::new(Lv::Info as u8);
/// When the raise ends (Unix ms), 0 when nothing is raised.
static UNTIL: AtomicU64 = AtomicU64::new(0);
/// Bumped on every change, so a helper knows to fetch the setting again.
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Replace the setting (normalized first). Returns what is now in force.
pub fn set_levels(mut levels: Levels) -> Levels {
    let now = now_ms();
    levels.normalize(now);
    let all: Vec<Lv> = std::iter::once(levels.level)
        .chain(levels.categories.values().copied())
        .collect();
    let lowest = all.iter().copied().min().unwrap_or(Lv::Info).min(Lv::Info);
    let highest = all.iter().copied().max().unwrap_or(Lv::Info);
    let mut g = state().write().unwrap_or_else(|e| e.into_inner());
    *g = levels.clone();
    FLOOR.store(lowest as u8, Ordering::Relaxed);
    CEIL.store(highest as u8, Ordering::Relaxed);
    UNTIL.store(levels.until_ms.unwrap_or(0), Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    levels
}

/// The setting as it was set (a raise that has run out is still in it until [`expire_now`]).
pub fn levels() -> Levels {
    state().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// What [`expire_now`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    Unchanged,
    /// The end was more than [`MAX_RAISE_MS`] away (the clock went back) and was pulled in.
    EndPulledIn,
    /// The raise ran out and was dropped.
    RanOut,
}

/// Drop a raise that has run out, and pull in an end the clock left too far away. Anything but
/// [`Expiry::Unchanged`] changed the setting (the caller saves it and tells people).
pub fn expire_now() -> Expiry {
    let now = now_ms();
    let mut l = levels();
    let pulled = l.clamp_end(now);
    let ran_out = l.expire(now);
    if !pulled && !ran_out {
        return Expiry::Unchanged;
    }
    set_levels(l);
    if ran_out {
        Expiry::RanOut
    } else {
        Expiry::EndPulledIn
    }
}

/// Changes on every [`set_levels`].
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// A line at `lv` in `cat` is wanted now.
#[inline]
pub fn enabled(lv: Lv, cat: &str) -> bool {
    let n = lv as u8;
    if n <= FLOOR.load(Ordering::Relaxed) {
        return true;
    }
    if n > CEIL.load(Ordering::Relaxed) {
        return false;
    }
    let now = now_ms();
    if lv > Lv::Info {
        let until = UNTIL.load(Ordering::Relaxed);
        if until == 0 || until <= now {
            return false;
        }
    }
    state()
        .read()
        .map(|g| g.effective(cat, now) >= lv)
        .unwrap_or(false)
}

/// Something above `info` is wanted now, anywhere.
pub fn any_raised() -> bool {
    CEIL.load(Ordering::Relaxed) > Lv::Info as u8 && UNTIL.load(Ordering::Relaxed) > now_ms()
}

/// The highest level any category writes now.
pub fn ceiling() -> Lv {
    if any_raised() {
        Lv::from_u8(CEIL.load(Ordering::Relaxed))
    } else {
        Lv::from_u8(CEIL.load(Ordering::Relaxed)).min(Lv::Info)
    }
}

type LeveledSink = Box<dyn Fn(Lv, &'static str, &str) + Send + Sync + 'static>;

fn leveled_sink_cell() -> &'static OnceLock<LeveledSink> {
    static CELL: OnceLock<LeveledSink> = OnceLock::new();
    &CELL
}

/// Install where `clog!` lines go (the engine's ring). First call wins.
pub fn set_leveled_sink(f: impl Fn(Lv, &'static str, &str) + Send + Sync + 'static) {
    let _ = leveled_sink_cell().set(Box::new(f));
}

/// Write a line (call it through `clog!`, which checks [`enabled`] first).
pub fn log_at(lv: Lv, cat: &'static str, msg: &str) {
    match leveled_sink_cell().get() {
        Some(f) => f(lv, cat, msg),
        None => eprintln!("[core:{}:{cat}] {msg}", lv.name()),
    }
}

/// A leveled line in a category: `clog!(Debug, "rp", "stage {n}")`. The arguments are not
/// formatted unless the line is wanted. Never put a key, token, PIN or passcode in one: the sink
/// takes out what it can recognize, but a bare key looks like any other hex.
#[macro_export]
macro_rules! clog {
    ($lv:ident, $cat:expr, $($arg:tt)*) => {
        if $crate::log::enabled($crate::log::Lv::$lv, $cat) {
            $crate::log::log_at($crate::log::Lv::$lv, $cat, &format!($($arg)*))
        }
    };
}

/// A Remote Play debug line (the handshake). Kept for the call sites that predate `clog!`.
#[macro_export]
macro_rules! core_debug {
    ($($arg:tt)*) => { $crate::clog!(Debug, "rp", $($arg)*) };
}

// ---- "Detailed logs", the switch levels replaced ----

/// Turn "Detailed logs" on until `until_ms` (Unix ms), or off with 0: the global level raised to
/// `debug` until then, or the raise dropped. Kept for `/api/diag/log-level`.
pub fn set_verbose_until(until_ms: u64) {
    let mut l = levels();
    if until_ms > now_ms() {
        if l.level < Lv::Debug {
            l.level = Lv::Debug;
        }
        l.until_ms = Some(until_ms);
    } else {
        l.until_ms = Some(0);
        l.expire(now_ms());
    }
    set_levels(l);
}

/// When the raise ends (Unix ms), while something is raised.
pub fn verbose_until() -> Option<u64> {
    any_raised().then(|| UNTIL.load(Ordering::Relaxed))
}

/// Debug lines are wanted in the general category now.
pub fn verbose() -> bool {
    enabled(Lv::Debug, GENERAL)
}

#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn lv(level: Lv, cats: &[(&str, Lv)], until: Option<u64>) -> Levels {
        Levels {
            level,
            categories: cats.iter().map(|(c, l)| (c.to_string(), *l)).collect(),
            until_ms: until,
        }
    }

    #[test]
    fn detailed_logs_turn_themselves_off() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_levels(Levels::default());
        assert!(!verbose());
        let later = now_ms() + 60_000;
        set_verbose_until(later);
        assert!(verbose());
        assert_eq!(verbose_until(), Some(later));
        // A time already past is off.
        set_verbose_until(now_ms().saturating_sub(1));
        assert!(!verbose());
        assert_eq!(verbose_until(), None);
        set_levels(Levels::default());
    }

    #[test]
    fn overrides_and_the_global_level() {
        let now = 1_800_000_000_000;
        let l = lv(Lv::Warn, &[("rp", Lv::Trace)], Some(now + 1000));
        assert_eq!(l.effective("rp", now), Lv::Trace);
        assert_eq!(l.effective("install", now), Lv::Warn);
        // The raise ran out: rp is back at info, the lowered global level stays.
        assert_eq!(l.effective("rp", now + 1000), Lv::Info);
        assert_eq!(l.effective("install", now + 1000), Lv::Warn);
    }

    #[test]
    fn a_raise_always_has_an_end() {
        let now = 1_800_000_000_000;
        let mut l = lv(Lv::Debug, &[("nope", Lv::Trace), ("RP", Lv::Debug)], None);
        l.normalize(now);
        assert_eq!(l.until_ms, Some(now + DEFAULT_RAISE_MS));
        // Unknown categories go, and so does one the same as the global level.
        assert!(l.categories.is_empty(), "{:?}", l.categories);
        let mut l = lv(
            Lv::Info,
            &[("rp", Lv::Trace)],
            Some(now + 10 * MAX_RAISE_MS),
        );
        l.normalize(now);
        assert_eq!(l.until_ms, Some(now + MAX_RAISE_MS));
        // Nothing raised: no end.
        let mut l = lv(Lv::Warn, &[("rp", Lv::Error)], Some(now + 5));
        l.normalize(now);
        assert_eq!(l.until_ms, None);
    }

    #[test]
    fn expiry_keeps_lowered_levels() {
        let now = 1_800_000_000_000;
        let mut l = lv(
            Lv::Debug,
            &[("rp", Lv::Trace), ("install", Lv::Error)],
            Some(now),
        );
        assert!(l.expire(now));
        assert_eq!(l.level, Lv::Info);
        assert_eq!(l.categories.get("install"), Some(&Lv::Error));
        assert!(!l.categories.contains_key("rp"));
        assert_eq!(l.until_ms, None);
        assert!(!l.expire(now));
    }

    #[test]
    fn the_gate_follows_the_setting() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_levels(Levels::default());
        assert!(enabled(Lv::Info, "rp"));
        assert!(!enabled(Lv::Debug, "rp"));
        set_levels(lv(Lv::Info, &[("rp", Lv::Trace)], Some(now_ms() + 60_000)));
        assert!(enabled(Lv::Trace, "rp"));
        assert!(!enabled(Lv::Debug, "install"));
        assert!(any_raised());
        assert_eq!(ceiling(), Lv::Trace);
        set_levels(lv(Lv::Warn, &[], None));
        assert!(!enabled(Lv::Info, "install"));
        assert!(enabled(Lv::Error, "install"));
        // A raise that has run out writes nothing above info, before anyone expires it.
        set_levels(lv(Lv::Debug, &[], Some(now_ms() - 1)));
        assert!(!enabled(Lv::Debug, "rp"));
        assert_eq!(expire_now(), Expiry::RanOut);
        assert_eq!(levels().level, Lv::Info);
        assert_eq!(expire_now(), Expiry::Unchanged);
        // The clock went back a week after a day's raise began: the end is pulled in to a day.
        let far = now_ms() + 8 * MAX_RAISE_MS;
        *state().write().unwrap() = lv(Lv::Debug, &[], Some(far));
        assert_eq!(expire_now(), Expiry::EndPulledIn);
        let u = levels().until_ms.unwrap();
        assert!(u <= now_ms() + MAX_RAISE_MS && u + 60_000 > now_ms() + MAX_RAISE_MS);
        assert_eq!(levels().level, Lv::Debug);
        set_levels(Levels::default());
    }

    #[test]
    fn an_end_too_far_away_is_pulled_in() {
        let now = 1_800_000_000_000;
        let mut l = lv(Lv::Debug, &[], Some(now + 3 * MAX_RAISE_MS));
        assert!(l.clamp_end(now));
        assert_eq!(l.until_ms, Some(now + MAX_RAISE_MS));
        assert!(!l.clamp_end(now));
        let mut l = lv(Lv::Debug, &[], Some(u64::MAX));
        // No overflow at the edge of the clock.
        assert!(!l.clamp_end(u64::MAX - 5));
        assert_eq!(l.until_ms, Some(u64::MAX));
    }

    #[test]
    fn names_round_trip() {
        for l in Lv::ALL {
            assert_eq!(Lv::parse(l.name()), Some(l));
        }
        assert_eq!(Lv::parse("DEBUG"), Some(Lv::Debug));
        assert_eq!(Lv::parse("verbose"), None);
        assert_eq!(category("Install"), Some("install"));
        assert_eq!(category("nope"), None);
    }
}
