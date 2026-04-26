//! Tiny in-memory log ring with an HTTP tail endpoint.
//!
//! The engine runs as a sidecar process whose stdout/stderr is piped into
//! the Tauri main process's stderr. That's fine for a developer watching a
//! terminal during `tauri dev`, but end users running the packaged app have
//! no way to see engine log lines — the in-app Log tab only captures the
//! renderer's own console.
//!
//! This module records every log line into a process-local ring buffer
//! (bounded at ~500 lines) so the renderer can fetch the recent tail via
//! `GET /api/engine-logs?since=<seq>` and mirror the lines into the Log
//! tab. Each entry carries a monotonic `seq` so the renderer can poll
//! incrementally without duplicates.
//!
//! The usual `eprintln!` still fires so terminal output during `tauri dev`
//! is unchanged.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Bound the ring: 500 entries × ~200 bytes ≈ 100 KB max. Enough to cover
/// the reconcile + transfer lifecycle of any realistic upload; older
/// entries drop off the back.
const RING_CAP: usize = 500;

/// Cap per-entry message length. Our own log sites all format short
/// diagnostic lines, but `core_log!` forwards from ps5upload-core
/// where a buggy caller could someday interpolate a multi-MB blob
/// (e.g. the full body of a failed frame). Truncate defensively so a
/// single oversized line can't balloon ring memory from 100 KB to GBs.
const MSG_MAX_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LogEntry {
    pub(crate) seq: u64,
    pub(crate) ts_ms: u64,
    pub(crate) level: &'static str,
    pub(crate) msg: String,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn ring() -> &'static Mutex<VecDeque<LogEntry>> {
    // SAFETY: OnceLock in a plain `fn` needs it to be inside the fn body.
    use std::sync::OnceLock;
    static RING: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAP)))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Record a log line. Cheap: one allocation for the formatted message,
/// one mutex acquisition, bounded-time pop on overflow. Also mirrors to
/// stderr with a `[engine]` tag so terminal watchers see it too.
pub(crate) fn record(level: &'static str, mut msg: String) {
    if msg.len() > MSG_MAX_BYTES {
        // Truncate on a valid UTF-8 char boundary so the String stays
        // well-formed. `floor_char_boundary` is stable; use a manual
        // scan for msrv-portability.
        let mut cut = MSG_MAX_BYTES;
        while cut > 0 && !msg.is_char_boundary(cut) {
            cut -= 1;
        }
        msg.truncate(cut);
        msg.push_str("…[truncated]");
    }
    // Tag the stderr copy so `tauri dev` output is still readable.
    eprintln!("[engine:{level}] {msg}");
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let entry = LogEntry {
        seq,
        ts_ms: now_ms(),
        level,
        msg,
    };
    // Poison-safe: if a panic in another `record()` call left the
    // mutex poisoned, recover the inner data instead of panicking.
    // Every `log_info!`/`log_warn!` call goes through here, and the
    // logging sweep added ~30 such call sites — a single panic
    // anywhere in the engine (e.g. an unwrap on a None) used to
    // cascade through every subsequent log call and bring the
    // whole engine down. The ring buffer's only state is the
    // contained entries; a half-mutated ring is still safe to read
    // from and append to.
    let mut g = ring().lock().unwrap_or_else(|e| e.into_inner());
    if g.len() >= RING_CAP {
        g.pop_front();
    }
    g.push_back(entry);
}

/// Return every entry whose `seq` is strictly greater than `since`.
/// Callers poll with `since = last-seen-seq` to receive only new lines.
pub(crate) fn tail_since(since: u64) -> Vec<LogEntry> {
    let g = ring().lock().unwrap_or_else(|e| e.into_inner());
    g.iter().filter(|e| e.seq > since).cloned().collect()
}

/// Info-level shortcut with `format!`-style args.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::engine_log::record("info", format!($($arg)*)) };
}

/// Warn-level shortcut.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::engine_log::record("warn", format!($($arg)*)) };
}

/// Error-level shortcut.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::engine_log::record("error", format!($($arg)*)) };
}
