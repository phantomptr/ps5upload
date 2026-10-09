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
use uuid::Uuid;

use crate::JobState;

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

/// Reads `path` from `from` to its end (at most `cap` bytes), in chunks.
fn read_from(mgmt: &str, path: &str, from: u64, cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = from;
    loop {
        let want = CHUNK.min(cap.saturating_sub(out.len() as u64));
        if want == 0 {
            break;
        }
        let got = ps5upload_core::fs_ops::fs_read_with_timeout(
            mgmt,
            path,
            at,
            want,
            Some(READ_TIMEOUT),
            false,
        )?;
        at += got.len() as u64;
        let short = (got.len() as u64) < want;
        out.extend_from_slice(&got);
        if short {
            break;
        }
    }
    Ok(out)
}

/// One pass over one console. Returns the new cursor, or an error when the console did not
/// answer (the cursor is then left as it was).
fn mirror_once(host: &str, cur: &Cursor, carry: &mut Vec<u8>) -> anyhow::Result<Cursor> {
    let mgmt = crate::pkg_install::normalize_mgmt_addr(host);
    let size = ps5upload_core::fs_ops::fs_stat(&mgmt, LOG)?.size;
    let head_now = read_from(&mgmt, LOG, 0, HEAD as u64)?;
    let emit_all = |bytes: &[u8], carry: &mut Vec<u8>| {
        for line in split_lines(bytes, carry) {
            emit_event(line_event(host, &line));
        }
    };
    let start = match plan(cur, size, &head_now) {
        Plan::StartAt { at } => {
            return Ok(Cursor {
                offset: at,
                head: head_now,
            })
        }
        Plan::Read { from } => from,
        Plan::Rotated { old_from } => {
            if let Ok(old) = read_from(&mgmt, LOG_OLD, old_from, FIRST_READ_TAIL) {
                emit_all(&old, carry);
            }
            carry.clear();
            0
        }
    };
    // More than one tick's worth written since (a long gap): copy the most recent part.
    let start = start.max(size.saturating_sub(FIRST_READ_TAIL));
    let bytes = read_from(&mgmt, LOG, start, FIRST_READ_TAIL)?;
    emit_all(&bytes, carry);
    Ok(Cursor {
        offset: start + bytes.len() as u64,
        head: head_now,
    })
}

/// Starts the mirror loop. Called once from `run()`.
pub fn spawn(jobs: Arc<Mutex<HashMap<Uuid, JobState>>>) {
    tokio::spawn(async move {
        // Positions live in memory only: after an engine restart every console starts at the end
        // of its log again, so lines written while the engine was closed are not stamped "now".
        let mut cursors: HashMap<String, Cursor> = HashMap::new();
        let mut carries: HashMap<String, Vec<u8>> = HashMap::new();
        loop {
            tokio::time::sleep(TICK).await;
            let busy = jobs
                .lock()
                .map(|g| g.values().any(|s| matches!(s, JobState::Running { .. })))
                .unwrap_or(true);
            if busy {
                continue;
            }
            let hosts: Vec<String> = consoles()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect();
            for host in hosts {
                let cur = cursors.get(&host).cloned().unwrap_or_default();
                let mut carry = carries.remove(&host).unwrap_or_default();
                let h = host.clone();
                let res = tokio::task::spawn_blocking(move || {
                    let r = mirror_once(&h, &cur, &mut carry);
                    (r, carry)
                })
                .await;
                let Ok((res, carry)) = res else { continue };
                carries.insert(host.clone(), carry);
                // A console that did not answer keeps its cursor: the next tick that reaches it
                // copies everything written since, the lines before a crash included.
                if let Ok(next) = res {
                    cursors.insert(host, next);
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
    use ps5upload_core::events::{Level, Src};

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
