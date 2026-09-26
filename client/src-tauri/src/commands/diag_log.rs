//! Persistent on-disk application log.
//!
//! The in-app Log tab (`src/state/logs.ts`) only keeps the last 500 lines in
//! RAM and loses everything on a crash — which is exactly when a bug-reporter
//! needs the log most. This module gives the renderer a durable, time-stamped
//! sink: it batches its unified log (frontend + engine-bridge + console +
//! payload events) and flushes JSONL here every couple of seconds.
//!
//! Files live next to the user config at `~/.ps5upload/logs/` (app-private
//! config dir on mobile). One file per UTC day — `app-YYYYMMDD.jsonl` — each
//! line a JSON object the renderer already serialized: `{ts,level,source,
//! message,detail?}`. The directory is a bounded ring: files older than
//! `RETENTION_DAYS` are dropped, and the total is capped at `MAX_TOTAL_BYTES`
//! (oldest-first), so a chatty machine can't fill the disk.
//!
//! The Bug Report page reads a time-window back out of here (`read_window`)
//! and packages it; see `commands/bug_report.rs`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Drop log files older than this many days.
const RETENTION_DAYS: i64 = 3;
/// Cap the whole logs directory (oldest files pruned first). 64 MiB is far
/// more than any realistic window needs, but bounds a pathological error loop.
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Resolve `<config-base>/logs`, creating it. Mirrors `crash_reports::reports_dir`.
pub(crate) fn logs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let base = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("cannot resolve app config dir: {e}"))?;
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let base = app
        .path()
        .home_dir()
        .map_err(|e| format!("cannot resolve home dir: {e}"))?
        .join(".ps5upload");
    let dir = base.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    Ok(dir)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Calendar helpers (dependency-free; Howard Hinnant's civil algorithms) ──
// We avoid pulling in chrono/time just to name a file by date. These convert
// between a serial day count (days since 1970-01-01) and a (y, m, d) triple.

/// Days since 1970-01-01 for the given civil date. `m` in [1,12].
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil (y, m, d) for a serial day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `app-YYYYMMDD.jsonl` for the day containing `ms` (UTC).
fn file_name_for(ms: u64) -> String {
    let (y, m, d) = civil_from_days((ms / 86_400_000) as i64);
    format!("app-{y:04}{m:02}{d:02}.jsonl")
}

/// Parse `YYYYMMDD` out of `app-YYYYMMDD.jsonl` → serial day count. None if the
/// name doesn't match (so foreign files are ignored by the age prune).
fn day_of_file(p: &Path) -> Option<i64> {
    let name = p.file_name()?.to_str()?;
    let digits = name.strip_prefix("app-")?.strip_suffix(".jsonl")?;
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = digits[0..4].parse().ok()?;
    let m: i64 = digits[4..6].parse().ok()?;
    let d: i64 = digits[6..8].parse().ok()?;
    Some(days_from_civil(y, m, d))
}

/// All `app-*.jsonl` files, oldest-first (lexical sort is chronological).
fn list_log_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.starts_with("app-") && n.ends_with(".jsonl"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Enforce the retention policy: drop files older than `RETENTION_DAYS`, then
/// drop oldest files while the total exceeds `MAX_TOTAL_BYTES`. Never deletes
/// the newest file (the one currently being written).
fn prune(dir: &Path) {
    let today = (now_ms() / 86_400_000) as i64;
    for p in list_log_files(dir) {
        if let Some(day) = day_of_file(&p) {
            if today - day > RETENTION_DAYS {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let mut files = list_log_files(dir);
    let mut total: u64 = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    while total > MAX_TOTAL_BYTES && files.len() > 1 {
        let oldest = files.remove(0);
        let bytes = std::fs::metadata(&oldest).map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&oldest).is_ok() {
            total = total.saturating_sub(bytes);
        }
    }
}

/// Append already-serialized JSONL lines to today's file. The renderer batches
/// and flushes; each line is one `{ts,level,source,message,detail?}` object.
#[tauri::command]
pub async fn diag_log_append(app: AppHandle, lines: Vec<String>) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    let dir = logs_dir(&app)?;
    let path = dir.join(file_name_for(now_ms()));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {path:?}: {e}"))?;
    let mut buf = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in &lines {
        // Defensive: a stray newline inside a line would split one JSON object
        // across two physical lines and break the reader. Replace with space.
        buf.push_str(&l.replace('\n', " "));
        buf.push('\n');
    }
    f.write_all(buf.as_bytes())
        .map_err(|e| format!("write {path:?}: {e}"))?;
    prune(&dir);
    Ok(())
}

/// Read back every line whose `ts` field is ≥ `since_ms`. Lines that don't
/// parse (or lack `ts`) are kept — we'd rather over-include than silently drop
/// context from a bug report. Bounded by the retention cap above.
#[tauri::command]
pub async fn diag_log_read_window(app: AppHandle, since_ms: u64) -> Result<Vec<String>, String> {
    Ok(window_lines(&logs_dir(&app)?, since_ms))
}

/// Every retained line whose `ts` ≥ `since_ms` (unparseable lines kept).
/// Shared by the read command and the bug-report bundler.
pub(crate) fn window_lines(dir: &Path, since_ms: u64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in list_log_files(dir) {
        let content = match std::fs::read_to_string(&p) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            match line_ts(line) {
                Some(ts) if ts < since_ms => {}
                _ => out.push(line.to_string()),
            }
        }
    }
    out
}

/// Cheaply extract the `"ts": <number>` value from a JSONL line without a full
/// parse. Returns None if absent/unparseable (caller treats None as "keep").
fn line_ts(line: &str) -> Option<u64> {
    let i = line.find("\"ts\"")?;
    let rest = &line[i + 4..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

#[derive(Serialize)]
pub struct DiagLogStats {
    pub count: usize,
    pub bytes: u64,
    pub dir: String,
    /// Timestamp of the oldest retained line, for "logs since …" in the UI.
    pub oldest_ts: Option<u64>,
}

/// File count + total bytes + directory + oldest retained timestamp.
#[tauri::command]
pub async fn diag_log_stats(app: AppHandle) -> Result<DiagLogStats, String> {
    let dir = logs_dir(&app)?;
    let files = list_log_files(&dir);
    let bytes: u64 = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    // oldest_ts: first parseable ts in the lexically-oldest file.
    let oldest_ts = files.first().and_then(|p| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|c| c.lines().find_map(line_ts))
    });
    Ok(DiagLogStats {
        count: files.len(),
        bytes,
        dir: dir.to_string_lossy().into_owned(),
        oldest_ts,
    })
}

/// Delete all persisted log files. Returns how many were removed.
#[tauri::command]
pub async fn diag_log_clear(app: AppHandle) -> Result<usize, String> {
    let dir = logs_dir(&app)?;
    let mut n = 0usize;
    for p in list_log_files(&dir) {
        if std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// Reveal the logs directory in the OS file manager.
#[tauri::command]
pub async fn diag_log_open_dir(app: AppHandle) -> Result<(), String> {
    let dir = logs_dir(&app)?;
    super::reveal::reveal(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrips_epoch() {
        // 1970-01-01 is serial day 0.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // A known date: 2026-06-05.
        let d = days_from_civil(2026, 6, 5);
        assert_eq!(civil_from_days(d), (2026, 6, 5));
    }

    #[test]
    fn file_name_is_utc_dated() {
        // 2026-06-05T00:00:00Z = 1_780_963_200_000 ms.
        let ms = (days_from_civil(2026, 6, 5) as u64) * 86_400_000;
        assert_eq!(file_name_for(ms), "app-20260605.jsonl");
    }

    #[test]
    fn day_of_file_parses_and_rejects() {
        assert_eq!(
            day_of_file(Path::new("/x/app-20260605.jsonl")),
            Some(days_from_civil(2026, 6, 5))
        );
        assert_eq!(day_of_file(Path::new("/x/engine.log")), None);
        assert_eq!(day_of_file(Path::new("/x/app-2026.jsonl")), None);
    }

    #[test]
    fn line_ts_extracts() {
        assert_eq!(
            line_ts(r#"{"ts":1780963200000,"level":"info","msg":"hi"}"#),
            Some(1_780_963_200_000)
        );
        assert_eq!(line_ts(r#"{"ts": 42 ,"x":1}"#), Some(42));
        assert_eq!(line_ts(r#"{"level":"info"}"#), None);
    }

    #[test]
    fn window_filter_keeps_unparseable_and_recent() {
        let dir = std::env::temp_dir().join(format!("ps5up-diaglog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("app-20260605.jsonl");
        std::fs::write(
            &p,
            "{\"ts\":1000,\"m\":\"old\"}\n{\"ts\":5000,\"m\":\"new\"}\nnot-json\n",
        )
        .unwrap();
        // Replicate read_window's filter inline (the command needs an AppHandle).
        let mut out = vec![];
        for line in std::fs::read_to_string(&p).unwrap().lines() {
            if line.is_empty() {
                continue;
            }
            match line_ts(line) {
                Some(ts) if ts < 3000 => {}
                _ => out.push(line.to_string()),
            }
        }
        assert_eq!(out, vec!["{\"ts\":5000,\"m\":\"new\"}", "not-json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
