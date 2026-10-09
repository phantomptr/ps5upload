//! The app's own event journal on desktop and Android (bug-report spec §1.4): one JSONL file per
//! UTC day under `<logs_dir>/events`, kept 7 days and 32 MB. The front end collapses repeats and
//! batches lines; this side only stores them and reads a time range back.
use std::io::Write;
use std::path::{Path, PathBuf};

use tauri::AppHandle;

use super::diag_log::{civil_from_days, logs_dir, now_ms};

const MAX_AGE_MS: u64 = 7 * 86_400_000;
const MAX_TOTAL: u64 = 32 << 20;
const PREFIX: &str = "app-events-";

fn file_name_for(ms: u64) -> String {
    let (y, m, d) = civil_from_days((ms / 86_400_000) as i64);
    format!("{PREFIX}{y:04}{m:02}{d:02}.jsonl")
}

fn files_oldest_first(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut v: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(PREFIX) && n.ends_with(".jsonl")
        })
        .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .collect();
    v.sort();
    v
}

pub(crate) fn append_lines(dir: &Path, lines: &[String], now_ms: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file_name_for(now_ms)))?;
    for l in lines {
        writeln!(f, "{}", l.replace('\n', " "))?;
    }
    Ok(())
}

pub(crate) fn prune(dir: &Path, now_ms: u64, max_age_ms: u64, max_total: u64) {
    let oldest_keep = file_name_for(now_ms.saturating_sub(max_age_ms));
    let today = file_name_for(now_ms);
    let mut kept = Vec::new();
    for (p, n) in files_oldest_first(dir) {
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if name < oldest_keep {
            let _ = std::fs::remove_file(&p);
        } else {
            kept.push((p, n, name));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, n, _)| n).sum();
    for (p, n, name) in kept {
        if total <= max_total || name == today {
            break;
        }
        let _ = std::fs::remove_file(&p);
        total -= n;
    }
}

/// Every stored line whose event (`ts` .. `last_ts`) overlaps `since..=until`, oldest file first.
pub(crate) fn read_dir_range(dir: &Path, since_ms: u64, until_ms: u64) -> Vec<String> {
    let mut out = Vec::new();
    for (p, _) in files_oldest_first(dir) {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(ts) = v.get("ts").and_then(|x| x.as_u64()) else {
                continue;
            };
            let end = v.get("last_ts").and_then(|x| x.as_u64()).unwrap_or(ts);
            if end >= since_ms && ts <= until_ms {
                out.push(line.to_string());
            }
        }
    }
    out
}

fn events_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(logs_dir(app)?.join("events"))
}

#[tauri::command]
pub async fn app_events_append(app: AppHandle, lines: Vec<String>) -> Result<(), String> {
    let dir = events_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let now = now_ms();
        append_lines(&dir, &lines, now).map_err(|e| e.to_string())?;
        prune(&dir, now, MAX_AGE_MS, MAX_TOTAL);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn app_events_read(
    app: AppHandle,
    since_ms: u64,
    until_ms: Option<u64>,
) -> Result<Vec<String>, String> {
    let dir = events_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        read_dir_range(&dir, since_ms, until_ms.unwrap_or(u64::MAX))
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("appev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn append_then_read_range_filters_by_ts() {
        let dir = tmp("range");
        let now = 1_791_509_000_000;
        append_lines(
            &dir,
            &[
                format!(r#"{{"ts":{},"msg":"a"}}"#, now - 9_000),
                format!(r#"{{"ts":{now},"msg":"b"}}"#),
            ],
            now,
        )
        .unwrap();
        let got = read_dir_range(&dir, now - 2_000, u64::MAX);
        assert_eq!(got, vec![format!(r#"{{"ts":{now},"msg":"b"}}"#)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collapsed_entry_reaching_into_range_is_kept() {
        let dir = tmp("lastts");
        let now = 1_791_509_000_000;
        append_lines(
            &dir,
            &[format!(
                r#"{{"ts":{},"last_ts":{now},"msg":"a"}}"#,
                now - 50_000
            )],
            now,
        )
        .unwrap();
        assert_eq!(read_dir_range(&dir, now - 10_000, u64::MAX).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_removes_old_days_and_keeps_today() {
        let dir = tmp("prune");
        std::fs::write(dir.join("app-events-20200101.jsonl"), "{}\n").unwrap();
        let now = 1_791_509_000_000;
        append_lines(&dir, &[format!(r#"{{"ts":{now},"msg":"x"}}"#)], now).unwrap();
        prune(&dir, now, 7 * 86_400_000, 32 << 20);
        assert!(!dir.join("app-events-20200101.jsonl").exists());
        assert_eq!(read_dir_range(&dir, 0, u64::MAX).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lines_that_are_not_events_are_skipped() {
        let dir = tmp("junk");
        let now = 1_791_509_000_000;
        append_lines(
            &dir,
            &["not json".to_string(), r#"{"msg":"no ts"}"#.to_string()],
            now,
        )
        .unwrap();
        assert!(read_dir_range(&dir, 0, u64::MAX).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
