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
    /// The helper restarted (the log was moved to `.old` and begun again): finish the old file
    /// from `old_from`, then read the new one from 0.
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

/// Complete lines from `buf`, keeping a trailing partial line in `carry` for the next read.
pub fn split_lines(buf: &[u8], carry: &mut Vec<u8>) -> Vec<String> {
    carry.extend_from_slice(buf);
    let mut out = Vec::new();
    while let Some(i) = carry.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = carry.drain(..=i).collect();
        let s = String::from_utf8_lossy(&line[..line.len() - 1])
            .trim_end_matches('\r')
            .to_string();
        if !s.is_empty() {
            out.push(s);
        }
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
    if !host.is_empty() {
        consoles()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host);
    }
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
    };
    Ok(())
}

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
        loop {
            tokio::time::sleep(TICK).await;
            if busy() {
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
                    tokio::task::spawn_blocking(move || {
                        let src = ConsoleLog {
                            mgmt: crate::pkg_install::normalize_mgmt_addr(&host),
                        };
                        let r =
                            mirror_pass(&src, &host, &mut cur, &mut carry, &mut |e| emit_event(e));
                        (host, r.is_ok(), cur, carry)
                    })
                })
                .collect();
            for pass in passes {
                let Ok((host, answered, cur, carry)) = pass.await else {
                    continue;
                };
                // The cursor moves only past what was copied, so a console that stopped
                // answering half way picks up exactly there on its next answer.
                cursors.insert(host.clone(), cur);
                carries.insert(host.clone(), carry);
                if misses.record(&host, answered) {
                    consoles()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&host);
                    cursors.remove(&host);
                    carries.remove(&host);
                }
            }
        }
    });
}

#[derive(Deserialize)]
pub struct FtpLogQuery {
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
        let mut cur = Cursor {
            offset: 12,
            head: b"=== A\na1\n".to_vec(),
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
    fn lines_are_classified() {
        let e = line_event("192.168.86.100", "[fatal] signal 11 while serving frame 88");
        assert_eq!(
            (e.level, e.code.as_deref()),
            (Level::Error, Some("helper_fatal"))
        );
        assert!(matches!(e.src, Src::Helper));
        let ok = line_event(
            "192.168.86.100",
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
