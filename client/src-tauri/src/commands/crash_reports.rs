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

/// All report files in `dir`: the renderer's `*.json` and the panic hook's `*.txt` (crash_hook.rs),
/// sorted oldest-first (the timestamped filenames sort chronologically, so a lexical sort is
/// chronological).
pub(crate) fn list_report_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json" || x == "txt"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Drops the oldest reports so that, after adding one, `dir` holds at most `MAX_REPORTS`.
pub(crate) fn prune(dir: &Path) {
    let existing = list_report_files(dir);
    if existing.len() >= MAX_REPORTS {
        let remove = existing.len() + 1 - MAX_REPORTS;
        for old in existing.iter().take(remove) {
            let _ = std::fs::remove_file(old);
        }
    }
}

/// Persist one report. `contents` is the JSON the renderer already built.
/// Prunes the oldest reports so the directory stays a bounded ring buffer.
#[tauri::command]
pub async fn crash_report_save(app: AppHandle, contents: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let dir = reports_dir(&app)?;

        prune(&dir);

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // Include the pid so two reports written in the same millisecond (e.g. a
        // crash burst) don't clobber each other.
        let path = dir.join(format!("ps5upload-report-{ts}-{}.json", std::process::id()));
        std::fs::write(&path, contents.as_bytes()).map_err(|e| format!("write {path:?}: {e}"))?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("crash_report_save: {e}"))?
}

/// Bundle every kept report into `dest` (a user-picked `.zip` path). Returns
/// the number of reports packaged. A zip that fails half way leaves nothing at that path, and a
/// file already there as it was.
#[tauri::command]
pub async fn crash_reports_zip(app: AppHandle, dest: String) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || {
        let dir = reports_dir(&app)?;
        let files = list_report_files(&dir);
        if files.is_empty() {
            return Err("no crash reports to package".to_string());
        }
        // Android save dialogs return content:// URIs std::fs can't create; redirect
        // those to a real path under Downloads (desktop paths pass through).
        let dest_path = super::save_dest::resolve_save_dest(&dest, "ps5upload-crash-reports.zip")?;
        zip_reports(&files, &dest_path)
    })
    .await
    .map_err(|e| format!("crash_reports_zip: {e}"))?
}

/// `files` as a zip at `dest`, committed only once it is whole.
fn zip_reports(files: &[PathBuf], dest: &Path) -> Result<usize, String> {
    let (replacing, f) = super::save_text_file::ReplacingFile::create(dest)
        .map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut n = 0usize;
    for p in files {
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("report-{n}.txt"));
        let data = std::fs::read(p).map_err(|e| format!("read {p:?}: {e}"))?;
        zw.start_file(name, opts)
            .map_err(|e| format!("zip start_file: {e}"))?;
        zw.write_all(&data).map_err(|e| format!("zip write: {e}"))?;
        n += 1;
    }
    zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    replacing
        .commit()
        .map_err(|e| format!("save {}: {e}", dest.display()))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ps5upload-crashzip-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_zip_that_fails_half_way_leaves_nothing_and_keeps_what_was_there() {
        // F26: a failed crash-report zip stayed at the picked path as a broken file.
        let dir = scratch("failed");
        let dest = dir.join("reports.zip");
        std::fs::write(&dest, b"an earlier zip").unwrap();
        let good = dir.join("a.json");
        std::fs::write(&good, b"{}").unwrap();
        let err = zip_reports(&[good, dir.join("missing.json")], &dest).unwrap_err();
        assert!(err.contains("missing.json"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"an earlier zip");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "nothing added");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panic_reports_are_listed_with_the_others_and_pruned_oldest_first() {
        let dir = scratch("list");
        std::fs::write(dir.join("ps5upload-report-1-1.json"), b"{}").unwrap();
        std::fs::write(dir.join("ps5upload-report-2-1-0.txt"), b"x").unwrap();
        std::fs::write(dir.join("notes.bin"), b"x").unwrap();
        assert_eq!(list_report_files(&dir).len(), 2);
        for i in 0..MAX_REPORTS {
            std::fs::write(dir.join(format!("ps5upload-report-3{i:03}-1-0.txt")), b"x").unwrap();
        }
        prune(&dir);
        let left = list_report_files(&dir);
        assert_eq!(left.len(), MAX_REPORTS - 1);
        assert!(!left
            .iter()
            .any(|p| p.ends_with("ps5upload-report-1-1.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_whole_zip_lands_at_the_picked_path() {
        let dir = scratch("ok");
        let dest = dir.join("reports.zip");
        let report = dir.join("a.json");
        std::fs::write(&report, b"{}").unwrap();
        assert_eq!(zip_reports(&[report], &dest).unwrap(), 1);
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        assert_eq!(zip.len(), 1);
        assert_eq!(zip.by_index(0).unwrap().name().unwrap(), "a.json");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
