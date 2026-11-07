//! Copies each connected console's helper `stderr.log` into the event journal (bug-report spec
//! §1.6), so a helper's last words are on the PC before it dies. Every ~10 s per console seen
//! by a successful status call; skipped while any job runs (the management port is the
//! transfer's then, see the upload-speed poller contention fix). A console that stops answering
//! keeps its place, so the next read that reaches it copies the lines a crashing helper wrote last.
//!
//! Also `GET /api/ps5/helper-log-ftp`: the same log read through the console's FTP server
//! (:2121), the report's fallback when the helper itself is not answering.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use ps5upload_core::events::{console_host, emit_event, Cat, Event, Level, Src};
use serde::Deserialize;

pub const LOG: &str = "/data/ps5upload/stderr.log";
pub const LOG_OLD: &str = "/data/ps5upload/stderr.log.old";
const HEAD: usize = 64;
const CHUNK: u64 = 64 * 1024;
/// At most this much is copied per console per tick: a first contact with a long log copies its
/// tail, not megabytes of history.
const FIRST_READ_TAIL: u64 = 256 * 1024;
const TICK: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the mirror got to in one console's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor {
    pub offset: u64,
    /// The file's first bytes when the offset was taken: a different start means a new file.
    pub head: Vec<u8>,
    /// Passes in a row that found the size unchanged and read nothing more.
    pub quiet: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Never seen this console: start at the log's end. Its history is not copied, because every
    /// copied line is stamped with the time it was copied; the report reads history directly.
    StartAt {
        at: u64,
    },
    Read {
        from: u64,
    },
    /// The log was rotated (a helper start found it over 512 KiB, moved it to `.old` and began
    /// again; a start below that appends): finish the old file from `old_from`, then read the
    /// new one from 0.
    Rotated {
        old_from: u64,
    },
}

pub fn plan(cur: &Cursor, size_now: u64, head_now: &[u8]) -> Plan {
    if cur.head.is_empty() && cur.offset == 0 {
        return Plan::StartAt { at: size_now };
    }
    let n = cur.head.len().min(head_now.len());
    if size_now < cur.offset || (n > 0 && cur.head[..n] != head_now[..n]) {
        Plan::Rotated {
            old_from: cur.offset,
        }
    } else {
        Plan::Read { from: cur.offset }
    }
}

/// The longest partial line kept between reads; a "line" longer than this is not a log line
/// (a binary file, a log with no newlines) and is cut there.
const MAX_CARRY: usize = 64 * 1024;

/// Complete lines from `buf`, keeping a trailing partial line in `carry` for the next read.
pub fn split_lines(buf: &[u8], carry: &mut Vec<u8>) -> Vec<String> {
    carry.extend_from_slice(buf);
    let mut out = Vec::new();
    let mut push = |line: &[u8]| {
        let s = String::from_utf8_lossy(line)
            .trim_end_matches('\r')
            .to_string();
        if !s.is_empty() {
            out.push(s);
        }
    };
    // One pass, one drain: draining per line moved the rest of the buffer every time.
    let mut start = 0;
    while let Some(i) = carry[start..].iter().position(|&b| b == b'\n') {
        push(&carry[start..start + i]);
        start += i + 1;
    }
    carry.drain(..start);
    if carry.len() > MAX_CARRY {
        push(&carry[..MAX_CARRY]);
        carry.clear();
    }
    out
}

pub fn line_event(console: &str, line: &str) -> Event {
    let (level, code) = if line.starts_with("[fatal]") {
        (Level::Error, "helper_fatal")
    } else if line.contains("payload ready on port") {
        (Level::Info, "helper_ready")
    } else if line.contains("REFUSING TO START") || line.contains("takeover failed") {
        (Level::Error, "helper_takeover_failed")
    } else if line.contains("shutdown requested") {
        (Level::Info, "helper_exit")
    } else {
        (Level::Info, "helper_log")
    };
    let mut e = Event::new(
        Cat::Helper,
        level,
        code,
        Some(console),
        line.to_string(),
        None,
    );
    e.src = Src::Helper;
    e
}

fn consoles() -> &'static Mutex<HashSet<String>> {
    static C: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

/// A console answered a status call: mirror its log from now on.
pub fn note_console(addr: &str) {
    let host = console_host(addr);
    *last_status_call().lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    if !host.is_empty() {
        consoles()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host);
    }
}

/// When a window last asked for a console's status.
fn last_status_call() -> &'static Mutex<Option<std::time::Instant>> {
    static AT: OnceLock<Mutex<Option<std::time::Instant>>> = OnceLock::new();
    AT.get_or_init(Default::default)
}

/// The consoles that have answered a status call since this engine started, sorted.
pub(crate) fn answered_consoles() -> Vec<String> {
    let mut hosts: Vec<String> = consoles()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect();
    hosts.sort();
    hosts
}

/// Where the mirror reads a console's log files from: the console, or a fake in tests.
pub trait LogSource {
    fn size(&self, path: &str) -> anyhow::Result<u64>;
    fn read(&self, path: &str, from: u64, len: u64) -> anyhow::Result<Vec<u8>>;
}

/// A console's helper logs over the management connection, every call bounded to READ_TIMEOUT.
struct ConsoleLog {
    mgmt: String,
}

impl LogSource for ConsoleLog {
    fn size(&self, path: &str) -> anyhow::Result<u64> {
        Ok(ps5upload_core::fs_ops::fs_stat_with_timeout(&self.mgmt, path, READ_TIMEOUT)?.size)
    }
    fn read(&self, path: &str, from: u64, len: u64) -> anyhow::Result<Vec<u8>> {
        ps5upload_core::fs_ops::fs_read_with_timeout(
            &self.mgmt,
            path,
            from,
            len,
            Some(READ_TIMEOUT),
            false,
        )
    }
}

/// Reads `path` from `from` to its end (at most `cap` bytes), in chunks.
fn read_from(src: &impl LogSource, path: &str, from: u64, cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = from;
    loop {
        let want = CHUNK.min(cap.saturating_sub(out.len() as u64));
        if want == 0 {
            break;
        }
        let got = src.read(path, at, want)?;
        at += got.len() as u64;
        let short = (got.len() as u64) < want;
        out.extend_from_slice(&got);
        if short {
            break;
        }
    }
    Ok(out)
}

/// One pass over one console: emits each new complete line and moves `cur` past it. `cur` moves
/// after every stage that succeeded, so a pass that fails half way (the console stopped
/// answering after the `.old` tail was copied) never copies those lines again.
pub fn mirror_pass(
    src: &impl LogSource,
    host: &str,
    cur: &mut Cursor,
    carry: &mut Vec<u8>,
    emit: &mut dyn FnMut(Event),
) -> anyhow::Result<()> {
    let size = src.size(LOG)?;
    // Nothing written since: the size says so with one call instead of three. A log rotated to
    // exactly the same size is the one thing that hides, so the head is still looked at now and then.
    let met = cur.offset > 0 || !cur.head.is_empty();
    if met && size == cur.offset && cur.quiet + 1 < HEAD_CHECK_EVERY {
        cur.quiet += 1;
        return Ok(());
    }
    cur.quiet = 0;
    let head_now = read_from(src, LOG, 0, HEAD as u64)?;
    let mut emit_all = |bytes: &[u8], carry: &mut Vec<u8>| {
        for line in split_lines(bytes, carry) {
            emit(line_event(host, &line));
        }
    };
    let start = match plan(cur, size, &head_now) {
        Plan::StartAt { at } => {
            *cur = Cursor {
                offset: at,
                head: head_now,
                quiet: 0,
            };
            return Ok(());
        }
        Plan::Read { from } => from,
        Plan::Rotated { old_from } => {
            if let Ok(old) = read_from(src, LOG_OLD, old_from, FIRST_READ_TAIL) {
                emit_all(&old, carry);
            }
            carry.clear();
            // The old file is done: from here the new one is read from its start.
            *cur = Cursor {
                offset: 0,
                head: head_now.clone(),
                quiet: 0,
            };
            0
        }
    };
    // More than one tick's worth written since (a long gap): copy the most recent part.
    let start = start.max(size.saturating_sub(FIRST_READ_TAIL));
    let bytes = read_from(src, LOG, start, FIRST_READ_TAIL)?;
    emit_all(&bytes, carry);
    *cur = Cursor {
        offset: start + bytes.len() as u64,
        head: head_now,
        quiet: 0,
    };
    Ok(())
}

/// With the size unchanged, the head is read once per this many passes (a minute at the usual tick).
const HEAD_CHECK_EVERY: u32 = 6;
/// With no status call from any window for this long, nobody is looking: read at a slower pace.
const NOBODY_LOOKING: Duration = Duration::from_secs(300);
/// The slower pace: every this many ticks.
const IDLE_EVERY_TICKS: u64 = 6;

/// A console that has not answered this many ticks in a row (5 minutes) stops being read; its
/// next successful status call ([`note_console`]) brings it back.
pub const FORGET_AFTER_MISSES: u32 = 30;

/// Consecutive unanswered reads per console.
#[derive(Default)]
pub struct Misses(HashMap<String, u32>);

impl Misses {
    /// Records one tick's outcome; true when the console should be forgotten.
    pub fn record(&mut self, host: &str, answered: bool) -> bool {
        if answered {
            self.0.remove(host);
            return false;
        }
        let n = self.0.entry(host.to_string()).or_insert(0);
        *n += 1;
        if *n >= FORGET_AFTER_MISSES {
            self.0.remove(host);
            return true;
        }
        false
    }
}

/// One run of a console's helper: its `instance_id` and `started_at_unix` from `node.status`.
/// Both change when the helper restarts, which its log does not show: the helper appends to
/// `stderr.log` and only rotates it at 512 KiB, so the log's first bytes outlive a restart.
pub type HelperRun = (u64, u64);

/// What one console's helper was told of the log levels (`log.level`, log_levels.rs). A helper
/// that restarts starts again at info, so a new [`HelperRun`] means telling it again.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LevelPush {
    /// The level setting's generation and the helper run it was sent to.
    pub sent: Option<(u64, HelperRun)>,
    /// The helper run that answered "unknown method" (an older helper): not asked again.
    pub unsupported: Option<HelperRun>,
}

impl LevelPush {
    /// The helper is now `run`: an "unsupported" mark from another run (the helper was updated
    /// since) no longer holds.
    pub fn note_run(&mut self, run: HelperRun) {
        if self.unsupported.is_some_and(|u| u != run) {
            self.unsupported = None;
        }
    }
}

/// What to do about the levels for a helper run given the setting's `generation`.
#[derive(Debug, PartialEq, Eq)]
pub enum PushPlan {
    /// Up to date, or an older helper.
    Nothing,
    /// A new helper run and the setting is the default: it already is at info.
    NoteOnly,
    Send,
}

/// Whether the helper's run is needed to decide: with the default setting already sent, any
/// run (old or new) is at info, so the console is not asked for its status.
pub fn needs_run(p: &LevelPush, generation: u64, is_default: bool) -> bool {
    !is_default || p.sent.map(|(g, _)| g) != Some(generation)
}

pub fn push_plan(p: &LevelPush, generation: u64, run: HelperRun, is_default: bool) -> PushPlan {
    if p.unsupported == Some(run) {
        return PushPlan::Nothing;
    }
    match &p.sent {
        Some((g, r)) if *g == generation && *r == run => PushPlan::Nothing,
        Some((_, r)) if *r == run => PushPlan::Send,
        _ if is_default => PushPlan::NoteOnly,
        _ => PushPlan::Send,
    }
}

/// The helper's current run from `node.status`, bounded like the log reads.
fn helper_run(mgmt: &str) -> anyhow::Result<HelperRun> {
    let m = ps5upload_core::mgmt::m::NODE_STATUS;
    let body = ps5upload_core::mgmt::call_with(mgmt, m, m.label, b"", Some(READ_TIMEOUT))?;
    let v: serde_json::Value = serde_json::from_slice(&body)?;
    let n = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    let run = (n("instance_id"), n("started_at_unix"));
    anyhow::ensure!(
        run != (0, 0),
        "node.status has no instance_id or started_at_unix"
    );
    Ok(run)
}

/// Tells one helper the level setting if it needs telling (blocking: on the mirror's own thread).
fn push_levels(mgmt: &str, host: &str, push: &mut LevelPush) {
    let generation = ps5upload_core::log::generation();
    let is_default = ps5upload_core::log::levels() == ps5upload_core::log::Levels::default();
    if !needs_run(push, generation, is_default) {
        return;
    }
    let run = match helper_run(mgmt) {
        Ok(r) => r,
        // Tried again on the next tick.
        Err(e) => {
            crate::log_at!(Debug, "helper", "log levels to {host}: status: {e:#}");
            return;
        }
    };
    push.note_run(run);
    match push_plan(push, generation, run, is_default) {
        PushPlan::Nothing => {}
        PushPlan::NoteOnly => push.sent = Some((generation, run)),
        PushPlan::Send => {
            let config = crate::log_levels::helper_config();
            match ps5upload_core::helper_log_level::set(mgmt, &config) {
                Ok(Some(answer)) => {
                    crate::log_at!(
                        Debug,
                        "helper",
                        "log levels to {host}: {config} -> {answer}"
                    );
                    push.sent = Some((generation, run));
                }
                Ok(None) => {
                    crate::log_at!(
                        Debug,
                        "helper",
                        "log levels to {host}: this helper has no log.level; it logs as before"
                    );
                    push.unsupported = Some(run);
                }
                // Tried again on the next tick.
                Err(e) => crate::log_at!(Debug, "helper", "log levels to {host}: {e:#}"),
            }
        }
    }
}

/// Whether the engine is using a console's management port for real work right now.
pub type BusyCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Starts the mirror loop. Called once from `run()`. Every tick reads all known consoles at
/// once (a slow or dead one cannot hold up the others), unless `busy` says a transfer or an
/// install is running.
pub fn spawn(busy: BusyCheck) {
    tokio::spawn(async move {
        // Positions live in memory only: after an engine restart every console starts at the end
        // of its log again, so lines written while the engine was closed are not stamped "now".
        let mut cursors: HashMap<String, Cursor> = HashMap::new();
        let mut carries: HashMap<String, Vec<u8>> = HashMap::new();
        let mut misses = Misses::default();
        let mut pushes: HashMap<String, LevelPush> = HashMap::new();
        let mut tick_no: u64 = 0;
        loop {
            tokio::time::sleep(TICK).await;
            tick_no += 1;
            if busy() {
                continue;
            }
            let looking = last_status_call()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|t| t.elapsed() < NOBODY_LOOKING);
            if !looking && !tick_no.is_multiple_of(IDLE_EVERY_TICKS) {
                continue;
            }
            let hosts: Vec<String> = consoles()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect();
            let passes: Vec<_> = hosts
                .into_iter()
                .map(|host| {
                    let mut cur = cursors.get(&host).cloned().unwrap_or_default();
                    let mut carry = carries.remove(&host).unwrap_or_default();
                    let mut push = pushes.remove(&host).unwrap_or_default();
                    tokio::task::spawn_blocking(move || {
                        let src = ConsoleLog {
                            mgmt: crate::pkg_install::normalize_mgmt_addr(&host),
                        };
                        let r =
                            mirror_pass(&src, &host, &mut cur, &mut carry, &mut |e| emit_event(e));
                        // The helper answered: the moment to bring its log levels up to date.
                        // Still inside the tick, so a transfer that starts later is never waited on.
                        if r.is_ok() {
                            push_levels(&src.mgmt, &host, &mut push);
                        }
                        (host, r.is_ok(), cur, carry, push)
                    })
                })
                .collect();
            for pass in passes {
                let Ok((host, answered, cur, carry, push)) = pass.await else {
                    continue;
                };
                // The cursor moves only past what was copied, so a console that stopped
                // answering half way picks up exactly there on its next answer.
                cursors.insert(host.clone(), cur);
                carries.insert(host.clone(), carry);
                pushes.insert(host.clone(), push);
                if misses.record(&host, answered) {
                    consoles()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&host);
                    cursors.remove(&host);
                    carries.remove(&host);
                    pushes.remove(&host);
                }
            }
        }
    });
}

#[derive(Deserialize)]
pub struct FtpLogQuery {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
    pub max_bytes: Option<u64>,
}

async fn ftp_tail(
    ftp: &mut suppaftp::tokio::AsyncRustlsFtpStream,
    path: &str,
    max: u64,
) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let size = match ftp.size(path).await {
        Ok(n) => n as u64,
        Err(_) => return Ok(String::new()), // no such file: nothing to show, not a failure
    };
    let start = size.saturating_sub(max);
    ftp.resume_transfer(start as usize)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let mut stream = ftp
        .retr_as_stream(path)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let _ = stream.finish().await;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// `GET /api/ps5/helper-log-ftp?addr=<console>` -> `{"stderr": "...", "stderr_old": "..."}`, read
/// through the console's FTP server on :2121 (anonymous). 502 `{"error": "<step>: ..."}` when it
/// cannot be reached; the whole read is bounded to 8 s.
pub async fn ftp_log_handler(Query(q): Query<FtpLogQuery>) -> Response {
    let host = console_host(&q.addr);
    let max = q.max_bytes.unwrap_or(256 * 1024).min(1 << 20);
    let work = async {
        let mut ftp = suppaftp::tokio::AsyncRustlsFtpStream::connect(format!("{host}:2121"))
            .await
            .map_err(|e| format!("connect: {e}"))?;
        ftp.login("anonymous", "anonymous")
            .await
            .map_err(|e| format!("login: {e}"))?;
        ftp.transfer_type(suppaftp::types::FileType::Binary)
            .await
            .map_err(|e| format!("login: {e}"))?;
        let stderr = ftp_tail(&mut ftp, LOG, max).await?;
        let stderr_old = ftp_tail(&mut ftp, LOG_OLD, max).await?;
        let _ = ftp.quit().await;
        Ok::<_, String>((stderr, stderr_old))
    };
    match tokio::time::timeout(Duration::from_secs(8), work).await {
        Ok(Ok((stderr, stderr_old))) => {
            Json(serde_json::json!({ "stderr": stderr, "stderr_old": stderr_old })).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "timed out after 8 s" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_go_to_each_helper_run_once_per_change() {
        let run1: HelperRun = (7, 1_700_000_000);
        let run2: HelperRun = (8, 1_700_000_100);
        let mut p = LevelPush::default();
        // A new helper and the default setting: nothing to send, it is at info already.
        assert!(needs_run(&p, 1, true));
        assert_eq!(push_plan(&p, 1, run1, true), PushPlan::NoteOnly);
        p.sent = Some((1, run1));
        assert_eq!(push_plan(&p, 1, run1, true), PushPlan::Nothing);
        // The default already sent: no status call at all, whatever the helper does.
        assert!(!needs_run(&p, 1, true));
        // The setting changed: send, even back to the default.
        assert!(needs_run(&p, 2, true));
        assert_eq!(push_plan(&p, 2, run1, false), PushPlan::Send);
        assert_eq!(push_plan(&p, 2, run1, true), PushPlan::Send);
        p.sent = Some((2, run1));
        // A raise in force: the run is looked at every tick.
        assert!(needs_run(&p, 2, false));
        // The helper restarted (same log, new run) with a raise in force: send again.
        assert_eq!(push_plan(&p, 2, run2, false), PushPlan::Send);
        // An older helper is not asked again in the same run.
        p.unsupported = Some(run2);
        p.note_run(run2);
        assert_eq!(push_plan(&p, 3, run2, false), PushPlan::Nothing);
        // An updated helper (a new run) loses the mark and is asked.
        let run3: HelperRun = (9, 1_700_000_200);
        p.note_run(run3);
        assert_eq!(p.unsupported, None);
        assert_eq!(push_plan(&p, 3, run3, false), PushPlan::Send);
    }

    #[test]
    fn a_restart_within_the_same_second_is_still_a_new_run() {
        let mut p = LevelPush {
            sent: Some((4, (1, 1_700_000_000))),
            unsupported: None,
        };
        assert_eq!(push_plan(&p, 4, (2, 1_700_000_000), false), PushPlan::Send);
        p.unsupported = Some((2, 1_700_000_000));
        p.note_run((2, 1_700_000_000));
        assert_eq!(p.unsupported, Some((2, 1_700_000_000)));
    }

    /// A console's two log files in memory; `fail_log` makes reads of the live log fail.
    struct Fake {
        log: Vec<u8>,
        old: Vec<u8>,
        fail_log: std::cell::Cell<bool>,
    }
    impl LogSource for Fake {
        fn size(&self, path: &str) -> anyhow::Result<u64> {
            Ok(if path == LOG {
                self.log.len()
            } else {
                self.old.len()
            } as u64)
        }
        fn read(&self, path: &str, from: u64, len: u64) -> anyhow::Result<Vec<u8>> {
            if path == LOG && self.fail_log.get() && len > HEAD as u64 {
                anyhow::bail!("console stopped answering");
            }
            let b = if path == LOG { &self.log } else { &self.old };
            let from = (from as usize).min(b.len());
            let to = (from + len as usize).min(b.len());
            Ok(b[from..to].to_vec())
        }
    }

    fn texts(lines: &[Event]) -> Vec<&str> {
        lines.iter().map(|e| e.msg.as_str()).collect()
    }

    #[test]
    fn a_rotation_whose_new_file_fails_does_not_repeat_the_old_tail() {
        // Last seen: "=== A" log at offset 12. The helper restarted: A moved to .old with one
        // more line, a new log began. The new log's read fails this tick, then works.
        let fake = Fake {
            log: b"=== B\nb1\nb2\n".to_vec(),
            old: b"=== A\na1\na2\na3\n".to_vec(),
            fail_log: std::cell::Cell::new(true),
        };
        // The new log happens to be as long as the old offset: the head look is what finds it.
        let mut cur = Cursor {
            offset: 12,
            head: b"=== A\na1\n".to_vec(),
            quiet: HEAD_CHECK_EVERY,
        };
        let mut carry = Vec::new();
        let mut got = Vec::new();
        let first = mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e));
        assert!(first.is_err());
        fake.fail_log.set(false);
        mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e)).unwrap();
        assert_eq!(texts(&got), vec!["a3", "=== B", "b1", "b2"]);
    }

    #[test]
    fn an_unchanged_log_is_only_stat_ed_with_a_look_at_its_head_now_and_then() {
        // Any read fails here, so a pass that reads shows as an error.
        let fake = Fake {
            log: b"=== A\nx1\n".to_vec(),
            old: vec![],
            fail_log: std::cell::Cell::new(true),
        };
        let mut cur = Cursor {
            offset: 9,
            head: b"=== A\nx1\n".to_vec(),
            quiet: 0,
        };
        let (mut carry, mut got) = (Vec::new(), Vec::new());
        for _ in 1..HEAD_CHECK_EVERY {
            mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e)).unwrap();
        }
        assert!(mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e)).is_err());
        assert!(got.is_empty());
    }

    #[test]
    fn a_growing_log_is_copied_once() {
        let fake = Fake {
            log: b"=== A\nx1\n".to_vec(),
            old: vec![],
            fail_log: std::cell::Cell::new(false),
        };
        let mut cur = Cursor::default();
        let (mut carry, mut got) = (Vec::new(), Vec::new());
        mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e)).unwrap(); // first contact: end
        assert!(got.is_empty());
        let fake = Fake {
            log: b"=== A\nx1\nx2\nx3\n".to_vec(),
            ..fake
        };
        mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e)).unwrap();
        mirror_pass(&fake, "h", &mut cur, &mut carry, &mut |e| got.push(e)).unwrap();
        assert_eq!(texts(&got), vec!["x2", "x3"]);
    }
    use ps5upload_core::events::{Level, Src};

    #[test]
    fn a_console_that_stops_answering_is_forgotten_after_five_minutes() {
        let mut misses = Misses::default();
        for _ in 0..FORGET_AFTER_MISSES - 1 {
            assert!(!misses.record("10.0.0.9", false));
        }
        assert!(misses.record("10.0.0.9", false));
        // One answer resets the count.
        let mut m2 = Misses::default();
        m2.record("h", false);
        m2.record("h", true);
        for _ in 0..FORGET_AFTER_MISSES - 1 {
            assert!(!m2.record("h", false));
        }
    }

    #[test]
    fn grows_reads_from_offset() {
        let c = Cursor {
            offset: 100,
            head: b"=== ps5upload".to_vec(),
            quiet: 0,
        };
        assert!(matches!(
            plan(&c, 180, b"=== ps5upload"),
            Plan::Read { from: 100 }
        ));
    }

    #[test]
    fn shrunk_file_means_rotation() {
        let c = Cursor {
            offset: 5000,
            head: b"=== A".to_vec(),
            quiet: 0,
        };
        assert!(matches!(
            plan(&c, 300, b"=== B"),
            Plan::Rotated { old_from: 5000 }
        ));
    }

    #[test]
    fn same_size_different_head_means_rotation() {
        let c = Cursor {
            offset: 300,
            head: b"=== A".to_vec(),
            quiet: 0,
        };
        assert!(matches!(
            plan(&c, 300, b"=== B"),
            Plan::Rotated { old_from: 300 }
        ));
    }

    #[test]
    fn a_first_contact_starts_at_the_end_not_with_history() {
        // Old lines would all be stamped "now"; the report reads history from the log itself.
        let c = Cursor::default();
        assert!(matches!(
            plan(&c, 50_000, b"=== A"),
            Plan::StartAt { at: 50_000 }
        ));
    }

    #[test]
    fn partial_line_is_carried_to_next_read() {
        let mut carry = Vec::new();
        assert_eq!(split_lines(b"one\ntw", &mut carry), vec!["one"]);
        assert_eq!(
            split_lines(b"o\r\nthree\n", &mut carry),
            vec!["two", "three"]
        );
        assert!(carry.is_empty());
    }

    #[test]
    fn a_line_that_never_ends_does_not_grow_the_carry_without_bound() {
        let mut carry = Vec::new();
        let mut lines = Vec::new();
        for _ in 0..40 {
            lines.extend(split_lines(&[b'x'; 4096], &mut carry));
            assert!(carry.len() <= MAX_CARRY);
        }
        assert_eq!(lines.len(), 2, "cut at the bound");
        assert_eq!(lines[0].len(), MAX_CARRY);
        // Many lines in one read come out in order.
        let many: Vec<u8> = (0..1000)
            .flat_map(|i| format!("l{i}\n").into_bytes())
            .collect();
        let mut carry = Vec::new();
        let got = split_lines(&many, &mut carry);
        assert_eq!((got.len(), got[999].as_str()), (1000, "l999"));
    }

    #[test]
    fn lines_are_classified() {
        let e = line_event("192.0.2.100", "[fatal] signal 11 while serving frame 88");
        assert_eq!(
            (e.level, e.code.as_deref()),
            (Level::Error, Some("helper_fatal"))
        );
        assert!(matches!(e.src, Src::Helper));
        let ok = line_event(
            "192.0.2.100",
            "ps5upload2 payload ready on port 9120 (instance=1)",
        );
        assert_eq!(
            (ok.level, ok.code.as_deref()),
            (Level::Info, Some("helper_ready"))
        );
        let t = line_event(
            "h",
            "takeover failed even after sweeping — ports still held",
        );
        assert_eq!(t.code.as_deref(), Some("helper_takeover_failed"));
        let other = line_event("h", "[ava1] rpc method 33 -> status 12");
        assert_eq!(other.code.as_deref(), Some("helper_log"));
    }
}
