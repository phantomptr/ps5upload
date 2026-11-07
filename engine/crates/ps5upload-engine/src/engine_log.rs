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
///
/// Debug and trace lines (a line per HTTP request, per range a console
/// fetches) keep a ring of their own of the same size: sharing one, a
/// busy minute of requests pushed every warning out of the tail.
const RING_CAP: usize = 500;
/// The debug and trace ring is bigger: with a level raised it fills fast, and `/api/logs/recent`
/// (MCP `logs_recent`) reads from it. 2000 lines of ~200 bytes is ~400 KB.
const DEBUG_RING_CAP: usize = 2000;
/// The per-request access log (`GET /api/x -> 200 (3ms)`) has a small ring of its own: in the
/// debug ring, an open window's requests pushed out the debug lines someone raised a level for
/// (a Remote Play handshake was gone within minutes). Every line still reaches engine.log.
const ACCESS_RING_CAP: usize = 300;

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
    /// The line's category (`ps5upload_core::log::CATEGORIES`).
    pub(crate) cat: &'static str,
    pub(crate) msg: String,
}

// Start at 1, not 0: the client bridge begins polling with `since = 0` and
// tail_since() returns entries where `seq > since`, so a seq-0 entry (`0 > 0`
// is false) would never be delivered — the very first engine log line was
// silently dropped from the Log tab. Reserving 0 makes the first record seq=1.
static SEQ: AtomicU64 = AtomicU64::new(1);

fn ring() -> &'static Mutex<VecDeque<LogEntry>> {
    // SAFETY: OnceLock in a plain `fn` needs it to be inside the fn body.
    use std::sync::OnceLock;
    static RING: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAP)))
}

/// The ring for `debug` and `trace` lines (see `RING_CAP`).
fn debug_ring() -> &'static Mutex<VecDeque<LogEntry>> {
    use std::sync::OnceLock;
    static RING: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(DEBUG_RING_CAP)))
}

/// The ring for the access log (see `ACCESS_RING_CAP`).
fn access_ring() -> &'static Mutex<VecDeque<LogEntry>> {
    use std::sync::OnceLock;
    static RING: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(ACCESS_RING_CAP)))
}

/// Which ring a line goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ring {
    Main,
    Debug,
    Access,
}

impl Ring {
    fn for_level(level: &str) -> Ring {
        if matches!(level, "debug" | "trace") {
            Ring::Debug
        } else {
            Ring::Main
        }
    }

    fn cap(self) -> usize {
        match self {
            Ring::Main => RING_CAP,
            Ring::Debug => DEBUG_RING_CAP,
            Ring::Access => ACCESS_RING_CAP,
        }
    }

    fn get(self) -> &'static Mutex<VecDeque<LogEntry>> {
        match self {
            Ring::Main => ring(),
            Ring::Debug => debug_ring(),
            Ring::Access => access_ring(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Dedicated stderr-writer sink. `record()` is called on the tokio reactor
/// (the per-request log middleware fires it on every HTTP request, and the
/// inline `log_info!` at the top of each async handler), and a bare `eprintln!`
/// does a BLOCKING write to the piped stderr. If the Tauri parent stalls
/// draining that pipe, the write would block whatever thread called `record()`
/// — including a reactor worker — and with up to ~12 consoles logging that can
/// stall SSE, /pkg-host serving, and every console's requests. Hand the line to
/// this dedicated writer thread instead; the blocking write happens off the
/// reactor. The channel is bounded so a wedged parent can't grow the queue
/// without bound — once full we drop the stderr MIRROR for that line (the
/// in-memory ring + /api/engine-logs still has it), which is strictly better
/// than blocking the engine.
fn stderr_sink() -> &'static std::sync::mpsc::SyncSender<String> {
    use std::sync::OnceLock;
    static SINK: OnceLock<std::sync::mpsc::SyncSender<String>> = OnceLock::new();
    SINK.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(4096);
        let _ = std::thread::Builder::new()
            .name("engine-log-stderr".into())
            .spawn(move || {
                use std::io::Write;
                let mut err = std::io::stderr();
                // Single consumer → lines stay ordered among themselves. The
                // blocking write lives HERE, never on a reactor worker.
                for line in rx {
                    let _ = err.write_all(line.as_bytes());
                    let _ = err.write_all(b"\n");
                }
            });
        tx
    })
}

/// A line as it may be kept: every copy of it, the ring and engine.log, goes through this.
fn clean(msg: String) -> String {
    match ps5upload_core::secret_text::without_secrets(&msg) {
        std::borrow::Cow::Owned(clean) => clean,
        std::borrow::Cow::Borrowed(_) => msg,
    }
}

/// Record a log line in the `general` category, whatever the level setting says (the macros
/// check it). Cheap: one allocation for the formatted message,
/// one mutex acquisition, bounded-time pop on overflow. Also mirrors to
/// stderr with a `[engine]` tag so terminal watchers see it too.
pub(crate) fn record(level: &'static str, msg: String) {
    record_cat(level, ps5upload_core::log::GENERAL, msg)
}

/// [`record`] in a category.
pub(crate) fn record_cat(level: &'static str, cat: &'static str, msg: String) {
    record_in(Ring::for_level(level), level, cat, msg)
}

/// One line of the per-request access log: a debug line in `api`, written to engine.log like any
/// other but kept in its own ring, so requests never push other debug lines out.
pub(crate) fn record_access(msg: String) {
    record_in(Ring::Access, "debug", "api", msg)
}

fn record_in(which: Ring, level: &'static str, cat: &'static str, msg: String) {
    let mut msg = clean(msg);
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
    let ts = now_ms();
    // Tag the stderr copy with a level + epoch-ms so it's still readable in
    // `tauri dev` AND time-filterable once captured. The Tauri shell tees this
    // stderr to `<app_local_data_dir>/engine/engine.log` (see engine.rs
    // pipe_tagged); the leading timestamp lets the bug-report bundle window it.
    // Mirror to stderr off the calling (possibly reactor) thread — see
    // `stderr_sink`. try_send: if the parent is wedged and the queue is full,
    // drop the stderr mirror for this line rather than block the engine.
    if mirrors_to_stderr(level) {
        let line = if cat == ps5upload_core::log::GENERAL {
            format!("[engine:{level}] ts={ts} {msg}")
        } else {
            format!("[engine:{level}] ts={ts} [{cat}] {msg}")
        };
        let _ = stderr_sink().try_send(line);
    }
    push(which, level, cat, ts, msg);
}

/// Only the ring, not the stderr mirror: for a line the host has already written elsewhere.
pub(crate) fn record_quiet(level: &'static str, msg: String) {
    push(
        Ring::for_level(level),
        level,
        ps5upload_core::log::GENERAL,
        now_ms(),
        clean(msg),
    );
}

fn push(which: Ring, level: &'static str, cat: &'static str, ts: u64, msg: String) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let entry = LogEntry {
        seq,
        ts_ms: ts,
        level,
        cat,
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
    let mut g = which.get().lock().unwrap_or_else(|e| e.into_inner());
    if g.len() >= which.cap() {
        g.pop_front();
    }
    g.push_back(entry);
}

/// In a container, stderr is `docker logs`: a file nothing rotates by default, while an open tab
/// adds a debug line per request. The mirror exists for the desktop shell to tee, so there it only
/// takes what someone reading `docker logs` wants, unless "Detailed logs" is on. The debug ring
/// and the Log tab are not affected.
fn mirrors_to_stderr(level: &str) -> bool {
    use std::sync::OnceLock;
    static QUIET: OnceLock<bool> = OnceLock::new();
    !matches!(level, "debug" | "trace")
        || !*QUIET.get_or_init(crate::pkg_install::in_container)
        || ps5upload_core::log::any_raised()
}

/// What `/api/logs/recent` asks for.
#[derive(Debug, Default, Clone)]
pub(crate) struct Filter {
    /// Only lines at least this severe (`warn` keeps warn and error).
    pub(crate) level: Option<ps5upload_core::log::Lv>,
    /// Only these categories (empty: all).
    pub(crate) cats: Vec<&'static str>,
    /// Only lines from this time on (Unix ms).
    pub(crate) since_ms: u64,
    /// Only lines after this sequence number.
    pub(crate) since_seq: u64,
    /// At most this many, the newest.
    pub(crate) limit: usize,
}

/// The ring's lines that match `f`, oldest first.
pub(crate) fn recent(f: &Filter) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = tail_since(f.since_seq)
        .into_iter()
        .filter(|e| e.ts_ms >= f.since_ms)
        .filter(|e| {
            f.level.is_none_or(|want| {
                ps5upload_core::log::Lv::parse(e.level).is_some_and(|l| l <= want)
            })
        })
        .filter(|e| f.cats.is_empty() || f.cats.contains(&e.cat))
        .collect();
    if f.limit > 0 && out.len() > f.limit {
        out.drain(..out.len() - f.limit);
    }
    out
}

/// Bridges `ps5upload_core`'s leveled lines (`clog!`) into the ring.
pub(crate) fn core_sink(lv: ps5upload_core::log::Lv, cat: &'static str, msg: &str) {
    record_cat(lv.name(), cat, msg.to_string())
}

/// Return every entry whose `seq` is strictly greater than `since`.
/// Callers poll with `since = last-seen-seq` to receive only new lines.
pub(crate) fn tail_since(since: u64) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = {
        let g = ring().lock().unwrap_or_else(|e| e.into_inner());
        g.iter().filter(|e| e.seq > since).cloned().collect()
    };
    for r in [debug_ring(), access_ring()] {
        let g = r.lock().unwrap_or_else(|e| e.into_inner());
        out.extend(g.iter().filter(|e| e.seq > since).cloned());
    }
    out.sort_by_key(|e| e.seq);
    out
}

/// A line at a level in a category, written when the level setting wants it
/// (`ps5upload_core::log`, `/api/logs/level`): `log_at!(Debug, "rp", "stage {n}")`. The arguments
/// are not formatted otherwise, so a debug or trace line costs a couple of atomic loads while it
/// is off. Never put a key, token, PIN or passcode in one.
#[macro_export]
macro_rules! log_at {
    ($lv:ident, $cat:expr, $($arg:tt)*) => {
        if ps5upload_core::log::enabled(ps5upload_core::log::Lv::$lv, $cat) {
            $crate::engine_log::record_cat(
                ps5upload_core::log::Lv::$lv.name(),
                $cat,
                format!($($arg)*),
            )
        }
    };
}

/// Info-level shortcut with `format!`-style args, in the `general` category.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log_at!(Info, ps5upload_core::log::GENERAL, $($arg)*) };
}

/// Warn-level shortcut.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log_at!(Warn, ps5upload_core::log::GENERAL, $($arg)*) };
}

/// Error-level shortcut. Errors are always written.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::engine_log::record("error", format!($($arg)*)) };
}

/// Debug-level shortcut in the `general` category, written while debug is wanted there.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::log_at!(Debug, ps5upload_core::log::GENERAL, $($arg)*) };
}

/// A Remote Play debug line (what "Detailed logs" used to switch on). Kept for the call sites that
/// predate `log_at!`.
#[macro_export]
macro_rules! log_detail {
    ($($arg:tt)*) => { $crate::log_at!(Debug, "rp", $($arg)*) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A busy window's requests never push a raised level's debug lines out of the tail.
    #[test]
    fn requests_do_not_push_debug_lines_out() {
        let id = std::process::id();
        let mark = format!("rp-handshake-kept-{id}");
        record_cat("debug", "rp", mark.clone());
        for i in 0..DEBUG_RING_CAP + 50 {
            record_access(format!("GET /api/x-{id}-{i} -> 200 (1ms)"));
        }
        let lines = tail_since(0);
        assert!(lines.iter().any(|e| e.msg == mark && e.cat == "rp"));
        let access = lines
            .iter()
            .filter(|e| e.cat == "api" && e.msg.starts_with(&format!("GET /api/x-{id}-")))
            .count();
        assert!(access <= ACCESS_RING_CAP, "{access}");
        assert!(lines
            .iter()
            .any(|e| e.msg == format!("GET /api/x-{id}-{} -> 200 (1ms)", DEBUG_RING_CAP + 49)));
    }

    /// `/api/engine-logs` answers from the rings, at both levels, and quiet lines too.
    #[test]
    fn no_line_keeps_a_secret() {
        let id = std::process::id();
        record("warn", format!("fetch https://u:hunter2@h/a-{id} failed"));
        record("debug", format!("call-{id}?token=hunter2"));
        record_quiet("info", format!("quiet-{id} password=hunter2"));
        let mine: Vec<LogEntry> = tail_since(0)
            .into_iter()
            .filter(|e| e.msg.contains(&id.to_string()))
            .collect();
        assert!(mine.len() >= 3);
        assert!(
            mine.iter().all(|e| !e.msg.contains("hunter2")),
            "{}",
            mine.len()
        );
    }

    /// A flood of debug lines (one per request) does not push a warning out of the tail.
    #[test]
    fn debug_lines_do_not_evict_warnings() {
        let marker = format!("warn-kept-{}", std::process::id());
        record("warn", marker.clone());
        for i in 0..(RING_CAP * 2) {
            record("debug", format!("GET /api/x -> 200 #{i}"));
        }
        let tail = tail_since(0);
        assert!(tail.iter().any(|e| e.level == "warn" && e.msg == marker));
        assert!(tail.windows(2).all(|w| w[0].seq < w[1].seq), "in order");
    }
}
