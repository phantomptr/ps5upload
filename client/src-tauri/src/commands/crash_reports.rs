//! Automatic crash/error report collection.
//!
//! The renderer writes a detailed JSON diagnostic bundle here every time it
//! detects an error or crash (see `src/lib/crashReporter.ts`). Reports live
//! next to the user-facing config at `~/.ps5upload/crash-reports/` (or the
//! app-private config dir on mobile, where a home dotfile is denied). The
//! directory is a bounded ring buffer — only the most recent `MAX_REPORTS`
//! are kept — so it can't grow without bound on a machine that errors a lot.
//!
//! From Settings the user can bundle every kept report into a single `.zip`
//! to attach to a bug report.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Keep at most this many reports on disk (newest win). One report is a few
/// KB of JSON, so 60 is well under a megabyte even in the worst case.
const MAX_REPORTS: usize = 60;

/// Resolve `<config-base>/crash-reports`, creating it. Mirrors the path logic
/// in `user_config.rs` so reports sit alongside `settings.json`.
pub(crate) fn reports_dir(app: &AppHandle) -> Result<PathBuf, String> {
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
    let dir = base.join("crash-reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    Ok(dir)
}

/// All `*.json` report files in `dir`, sorted oldest-first (the timestamped
/// filenames sort chronologically, so a lexical sort is chronological).
pub(crate) fn list_report_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[derive(Serialize)]
pub struct CrashReportStats {
    pub count: usize,
    pub bytes: u64,
    pub dir: String,
}

/// Persist one report. `contents` is the JSON the renderer already built.
/// Prunes the oldest reports so the directory stays a bounded ring buffer.
#[tauri::command]
pub async fn crash_report_save(app: AppHandle, contents: String) -> Result<String, String> {
    let dir = reports_dir(&app)?;

    // Prune oldest so that, after adding one, we're at MAX_REPORTS.
    let existing = list_report_files(&dir);
    if existing.len() >= MAX_REPORTS {
        let remove = existing.len() + 1 - MAX_REPORTS;
        for old in existing.iter().take(remove) {
            let _ = std::fs::remove_file(old);
        }
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Include the pid so two reports written in the same millisecond (e.g. a
    // crash burst) don't clobber each other.
    let path = dir.join(format!("ps5upload-report-{ts}-{}.json", std::process::id()));
    std::fs::write(&path, contents.as_bytes()).map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Count + total bytes of kept reports, for the Settings UI.
#[tauri::command]
pub async fn crash_reports_stats(app: AppHandle) -> Result<CrashReportStats, String> {
    let dir = reports_dir(&app)?;
    let files = list_report_files(&dir);
    let bytes: u64 = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    Ok(CrashReportStats {
        count: files.len(),
        bytes,
        dir: dir.to_string_lossy().into_owned(),
    })
}

/// Absolute path of the reports directory (for "open folder" / display).
#[tauri::command]
pub async fn crash_reports_dir_resolved(app: AppHandle) -> Result<String, String> {
    Ok(reports_dir(&app)?.to_string_lossy().into_owned())
}

/// Bundle every kept report into `dest` (a user-picked `.zip` path). Returns
/// the number of reports packaged.
#[tauri::command]
pub async fn crash_reports_zip(app: AppHandle, dest: String) -> Result<usize, String> {
    let dir = reports_dir(&app)?;
    let files = list_report_files(&dir);
    if files.is_empty() {
        return Err("no crash reports to package".into());
    }
    // Android save dialogs return content:// URIs std::fs can't create; redirect
    // those to a real path under Downloads (desktop paths pass through).
    let dest_path = super::save_dest::resolve_save_dest(&dest, "ps5upload-crash-reports.zip")?;
    let f = std::fs::File::create(&dest_path)
        .map_err(|e| format!("create {}: {e}", dest_path.display()))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut n = 0usize;
    for p in &files {
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("report-{n}.json"));
        let data = std::fs::read(p).map_err(|e| format!("read {p:?}: {e}"))?;
        zw.start_file(name, opts)
            .map_err(|e| format!("zip start_file: {e}"))?;
        zw.write_all(&data).map_err(|e| format!("zip write: {e}"))?;
        n += 1;
    }
    zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(n)
}

/// Delete all kept reports (after the user has shared them, if they want a
/// clean slate). Returns how many were removed.
#[tauri::command]
pub async fn crash_reports_clear(app: AppHandle) -> Result<usize, String> {
    let dir = reports_dir(&app)?;
    let files = list_report_files(&dir);
    let mut n = 0usize;
    for p in &files {
        if std::fs::remove_file(p).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// Reveal the reports directory in the OS file manager. Delegates to the
/// shared `reveal` helper (see commands/reveal.rs) so the per-platform spawn
/// logic lives in one place.
#[tauri::command]
pub async fn crash_reports_open_dir(app: AppHandle) -> Result<(), String> {
    let dir = reports_dir(&app)?;
    super::reveal::reveal(&dir)
}
