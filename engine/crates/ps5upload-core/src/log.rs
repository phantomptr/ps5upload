//! Pluggable log sink for ps5upload-core.
//!
//! Core is a library crate, so it can't know anything about where logs
//! should land (stderr for the CLI, an in-memory ring for the engine
//! sidecar, a Tauri event for the desktop client). Each consumer
//! installs a sink via `set_sink` at startup; anything core emits via
//! the `core_log!` macro goes through it.
//!
//! Default sink is `eprintln!` so tests and standalone usage still
//! produce visible output without any setup.

use std::sync::OnceLock;

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

/// Emit a log line through the installed sink, or `eprintln!` if none
/// has been set yet. Safe to call before/without `set_sink`.
pub fn log(msg: &str) {
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

// ---- detailed logs ----
//
// "Detailed logs" (the Logs tab's switch) turns on extra debug lines where diagnosis is weakest
// (the Remote Play handshake, Play Together). They are written only while it is on, and it is
// on only until a set time: nobody stays verbose by accident. The engine owns the switch
// (`/api/diag/log-level`); core reads it so its own debug lines follow it.

use std::sync::atomic::{AtomicU64, Ordering};

/// Detailed logs are on while the wall clock (Unix ms) is before this; 0 = off.
static VERBOSE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

type DebugSink = Box<dyn Fn(&str) + Send + Sync + 'static>;

fn debug_sink_cell() -> &'static OnceLock<DebugSink> {
    static CELL: OnceLock<DebugSink> = OnceLock::new();
    &CELL
}

/// Install where debug lines go (the engine's ring at level `debug`). First call wins.
pub fn set_debug_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    let _ = debug_sink_cell().set(Box::new(f));
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Turn detailed logs on until `until_ms` (Unix ms), or off with 0.
pub fn set_verbose_until(until_ms: u64) {
    VERBOSE_UNTIL_MS.store(until_ms, Ordering::Relaxed);
}

/// When detailed logs turn themselves off (Unix ms), while they are on.
pub fn verbose_until() -> Option<u64> {
    let until = VERBOSE_UNTIL_MS.load(Ordering::Relaxed);
    (until > now_ms()).then_some(until)
}

/// Detailed logs are on now.
pub fn verbose() -> bool {
    verbose_until().is_some()
}

/// Emit a debug line (only call this when [`verbose`] is true; `core_debug!` checks it).
pub fn log_debug(msg: &str) {
    match debug_sink_cell().get() {
        Some(f) => f(msg),
        None => eprintln!("[core:debug] {msg}"),
    }
}

/// A debug line, written only while detailed logs are on. The arguments are not even
/// formatted otherwise. Never put a key, token or passcode in one.
#[macro_export]
macro_rules! core_debug {
    ($($arg:tt)*) => {
        if $crate::log::verbose() {
            $crate::log::log_debug(&format!($($arg)*))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detailed_logs_turn_themselves_off() {
        set_verbose_until(0);
        assert!(!verbose());
        let later = now_ms() + 60_000;
        set_verbose_until(later);
        assert!(verbose());
        assert_eq!(verbose_until(), Some(later));
        // A time already past is off.
        set_verbose_until(now_ms().saturating_sub(1));
        assert!(!verbose());
        assert_eq!(verbose_until(), None);
        set_verbose_until(0);
    }
}
