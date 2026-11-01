//! Where desktop updates are kept, what an install left behind, and cleaning up after both (#611).
//!
//! An update is downloaded into the app's own `updates` folder (app local data), never the
//! user's Downloads: until 10.5.1 every release landed in Downloads and stayed there, and when
//! the install never happened (the Windows zip, #611) one more zip piled up per release. Only a
//! download the person has to install by hand still goes to Downloads, since they need to find it.
//!
//! Before an install starts, `install.json` records which version it goes to and which file
//! it came from, and the steps are appended to `update.log` beside it. The next launch judges
//! it (`update_last_install`): running that version (or newer) means it worked, and the record,
//! the download and the log are deleted. Still the old version means it did not: the reason
//! (the last line of the log, or the error the app saw) is shown once, and the record stays so
//! the automatic install and install-on-quit do not try that version again by themselves.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const RECORD: &str = "install.json";
const LOG: &str = "update.log";
/// `update.log` is cut back to empty past this; one install writes a few hundred bytes.
const LOG_MAX_BYTES: u64 = 64 * 1024;

/// The app's own folder for update downloads, created when missing.
pub(crate) fn updates_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("no app data folder for updates: {e}"))?;
    let dir = base.join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The log the install steps append to (scripts too).
pub(crate) fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG)
}

/// An install that was started: written before it starts, judged on the next launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    /// The version installed over.
    pub from: String,
    /// The version being installed.
    pub to: String,
    /// The download it installs from.
    pub file: PathBuf,
    /// Why it failed, when the app saw it fail itself (it then stayed open).
    #[serde(default)]
    pub error: Option<String>,
    /// The failure has been shown once.
    #[serde(default)]
    pub reported: bool,
}

pub(crate) fn read_record(dir: &Path) -> Option<InstallRecord> {
    let raw = std::fs::read(dir.join(RECORD)).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub(crate) fn write_record(dir: &Path, record: &InstallRecord) -> Result<(), String> {
    let path = dir.join(RECORD);
    let body = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

pub(crate) fn remove_record(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(RECORD));
}

/// Append one line to `update.log` (best effort). Starts the log over when it grew too big.
pub(crate) fn log(dir: &Path, line: &str) {
    use std::io::Write;
    let path = log_path(dir);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{secs} {line}");
    }
}

/// The last line of `update.log`, without its timestamp.
fn last_log_line(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log_path(dir)).ok()?;
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let msg = match line.split_once(' ') {
        Some((ts, rest)) if ts.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => line,
    };
    Some(msg.trim().chars().take(300).collect())
}

/// What the next launch tells the renderer about the last install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastInstall {
    /// The version that was being installed.
    pub version: String,
    /// It is installed: that version (or a newer one) is running now.
    pub installed: bool,
    /// Why it failed: the error the app saw or the log's last line; empty when unknown.
    pub message: String,
    /// Not shown before: the renderer says so once.
    pub new: bool,
}

/// Judge `record` against the version running now. Ok when the install's version (or a newer
/// one) runs: `is_newer(current, to)` is false.
pub fn judge(record: &InstallRecord, current: &str, log_tail: Option<String>) -> LastInstall {
    let ok = !super::updates::is_newer(current, &record.to);
    LastInstall {
        version: record.to.clone(),
        installed: ok,
        message: if ok {
            String::new()
        } else {
            record.error.clone().or(log_tail).unwrap_or_default()
        },
        new: !record.reported,
    }
}

/// What the last install came to, once per launch, and the clean-up that goes with it: after a
/// success the record, the download and the log go; after a failure the download is kept for
/// "Try again". A finished download of a newer version stays too (it is reused, not downloaded
/// again); anything else in the updates folder (older downloads, a `.part` a crash left) goes.
pub fn settle(dir: &Path, current: &str) -> Option<LastInstall> {
    let record = read_record(dir);
    let outcome = record
        .as_ref()
        .map(|r| judge(r, current, last_log_line(dir)));
    let mut keep: Option<PathBuf> = None;
    match (&record, &outcome) {
        (Some(r), Some(o)) if o.installed => {
            let _ = std::fs::remove_file(&r.file);
            remove_record(dir);
            let _ = std::fs::remove_file(log_path(dir));
        }
        (Some(r), Some(_)) => {
            keep = Some(r.file.clone());
            if !r.reported {
                let mut r = r.clone();
                r.reported = true;
                let _ = write_record(dir, &r);
            }
        }
        _ => {}
    }
    clean_dir(dir, keep.as_deref(), Some(current));
    outcome
}

/// Delete the files in the updates folder except the record, the log, `keep` and, with
/// `current`, finished downloads of a version newer than that.
pub fn clean_dir(dir: &Path, keep: Option<&Path>, current: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        if name == RECORD || name == LOG || Some(path.as_path()) == keep {
            continue;
        }
        let pending = current.is_some_and(|cur| {
            name.to_str().is_some_and(|n| {
                !n.ends_with(".part")
                    && release_file_version(n).is_some_and(|v| super::updates::is_newer(cur, v))
            })
        });
        if pending {
            continue;
        }
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The "Try again" button: the failed version may be installed by itself again.
pub fn forget_failure(dir: &Path) {
    if read_record(dir).is_some_and(|r| r.reported || r.error.is_some()) {
        remove_record(dir);
    }
}

/// The release a file name belongs to, when it is exactly one of ours: `PS5Upload-<version>-`
/// followed by one of the desktop assets the release publishes (or the `.part` an interrupted
/// download of one leaves, `update_download` names it so). None for anything else; a name
/// with " (1)" or any other addition is not one we wrote.
pub fn release_file_version(name: &str) -> Option<&str> {
    const ASSETS: &[&str] = &[
        "-win-x64.zip",
        "-win-arm64.zip",
        "-win-x64-setup.exe",
        "-win-arm64-setup.exe",
        "-mac-arm64.dmg",
        "-mac-x64.dmg",
        "-linux-x64.zip",
        "-linux-arm64.zip",
        "-linux-x64.deb",
        "-linux-arm64.deb",
        "-linux-x64.rpm",
        "-linux-arm64.rpm",
        "-win-x64.part",
        "-win-arm64.part",
        "-win-x64-setup.part",
        "-win-arm64-setup.part",
        "-mac-arm64.part",
        "-mac-x64.part",
        "-linux-x64.part",
        "-linux-arm64.part",
    ];
    let rest = name.strip_prefix("PS5Upload-")?;
    let version = ASSETS.iter().find_map(|a| rest.strip_suffix(a))?;
    let core = version.split(['-', '+']).next()?;
    let numeric = core.split('.').count() == 3
        && core
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    let tail_ok = version[core.len()..]
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.'));
    (numeric && tail_ok).then_some(version)
}

/// Update downloads earlier versions left in `dir` (the Downloads folder): our exact release
/// file names for this version or an older one. A newer one may be waiting to be installed by
/// hand, so it is not counted.
pub fn old_downloads(dir: &Path, current: &str) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let version = release_file_version(name.to_str()?)?;
            if super::updates::is_newer(current, version) {
                return None;
            }
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (e.path(), meta.len()))
        })
        .collect();
    out.sort();
    out
}

#[derive(Serialize)]
pub struct OldDownloads {
    pub count: usize,
    pub bytes: u64,
}

/// The last install's outcome (once per launch; see `settle`), or None when there was none.
#[tauri::command]
pub fn update_last_install(app: AppHandle) -> Result<Option<LastInstall>, String> {
    let dir = updates_dir(&app)?;
    let current = app.package_info().version.to_string();
    super::self_install::clean_leftovers();
    Ok(settle(&dir, &current))
}

/// Forget a failed install, so it may be tried again by itself ("Try again").
#[tauri::command]
pub fn update_forget_failure(app: AppHandle) -> Result<(), String> {
    forget_failure(&updates_dir(&app)?);
    Ok(())
}

/// Update downloads earlier versions of the app left in the Downloads folder.
#[tauri::command]
pub fn update_old_downloads(app: AppHandle) -> Result<OldDownloads, String> {
    let Ok(dir) = app.path().download_dir() else {
        return Ok(OldDownloads { count: 0, bytes: 0 });
    };
    let files = old_downloads(&dir, &app.package_info().version.to_string());
    Ok(OldDownloads {
        count: files.len(),
        bytes: files.iter().map(|(_, n)| n).sum(),
    })
}

/// Delete them (the person asked: the "Remove" button). Returns how many were deleted.
#[tauri::command]
pub fn update_remove_old_downloads(app: AppHandle) -> Result<usize, String> {
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| format!("no Downloads folder: {e}"))?;
    let files = old_downloads(&dir, &app.package_info().version.to_string());
    Ok(files
        .iter()
        .filter(|(p, _)| std::fs::remove_file(p).is_ok())
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ps5upload-update-files-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn record(dir: &Path, to: &str) -> InstallRecord {
        InstallRecord {
            from: "10.4.1".into(),
            to: to.into(),
            file: dir.join(format!("PS5Upload-{to}-win-x64.zip")),
            error: None,
            reported: false,
        }
    }

    #[test]
    fn a_successful_install_leaves_nothing_behind() {
        let dir = tmp("ok");
        let r = record(&dir, "10.5.1");
        std::fs::write(&r.file, b"zip").unwrap();
        std::fs::write(dir.join("PS5Upload-10.5.0-win-x64.zip"), b"older").unwrap();
        std::fs::write(dir.join("PS5Upload-10.5.2-win-x64.part"), b"half").unwrap();
        write_record(&dir, &r).unwrap();
        log(&dir, "replaced PS5Upload.exe");
        let out = settle(&dir, "10.5.1").unwrap();
        assert!(out.installed && out.message.is_empty() && out.version == "10.5.1");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "all gone");
        // Nothing more to say on the launch after.
        assert_eq!(settle(&dir, "10.5.1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_install_is_reported_once_and_keeps_its_download() {
        let dir = tmp("failed");
        let r = record(&dir, "10.5.1");
        std::fs::write(&r.file, b"zip").unwrap();
        std::fs::write(dir.join("PS5Upload-10.4.0-win-x64.zip"), b"old").unwrap();
        write_record(&dir, &r).unwrap();
        log(&dir, "replacing C:\\Apps\\PS5Upload.exe");
        log(
            &dir,
            "could not replace C:\\Apps\\PS5Upload.exe: Access is denied.",
        );
        let first = settle(&dir, "10.4.1").unwrap();
        assert!(!first.installed && first.new);
        assert_eq!(
            first.message,
            "could not replace C:\\Apps\\PS5Upload.exe: Access is denied."
        );
        assert!(r.file.is_file(), "kept for Try again");
        assert!(!dir.join("PS5Upload-10.4.0-win-x64.zip").exists());
        // Shown once: the next launch still knows (no automatic retry) but says nothing new.
        let second = settle(&dir, "10.4.1").unwrap();
        assert!(!second.installed && !second.new);
        // Try again: forgotten.
        forget_failure(&dir);
        assert_eq!(settle(&dir, "10.4.1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_downloaded_newer_version_waits_for_its_install() {
        let dir = tmp("pending");
        for n in [
            "PS5Upload-10.6.0-win-x64.zip",
            "PS5Upload-10.6.0-win-x64.part",
            "PS5Upload-10.4.0-win-x64.zip",
            "stray.tmp",
        ] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        assert_eq!(settle(&dir, "10.5.1"), None);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["PS5Upload-10.6.0-win-x64.zip"]);
        // Once that version runs, it goes too.
        assert_eq!(settle(&dir, "10.6.0"), None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_error_the_app_saw_wins_over_the_log() {
        let dir = tmp("err");
        let mut r = record(&dir, "10.5.1");
        r.error = Some("the zip has no PS5Upload.exe".into());
        log(&dir, "something else");
        assert_eq!(
            judge(&r, "10.4.1", last_log_line(&dir)).message,
            "the zip has no PS5Upload.exe"
        );
        // A newer version than the one installed counts as installed.
        assert!(judge(&r, "10.6.0", None).installed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_our_own_release_names_count_as_old_downloads() {
        assert_eq!(
            release_file_version("PS5Upload-10.4.1-win-x64.zip"),
            Some("10.4.1")
        );
        assert_eq!(
            release_file_version("PS5Upload-10.5.0-rc1-mac-arm64.dmg"),
            Some("10.5.0-rc1")
        );
        assert_eq!(
            release_file_version("PS5Upload-9.0.0-win-x64-setup.exe"),
            Some("9.0.0")
        );
        assert_eq!(
            release_file_version("PS5Upload-10.4.1-win-x64.part"),
            Some("10.4.1")
        );
        for not_ours in [
            "PS5Upload-10.4.1-win-x64 (1).zip",
            "PS5Upload-10.4.1-win-x64.zip.crdownload",
            "PS5Upload.zip",
            "PS5Upload-latest-win-x64.zip",
            "ps5upload-10.4.1-win-x64.zip",
            "PS5Upload-10.4-win-x64.zip",
            "PS5Upload-10.4.1-android.apk",
            "My PS5Upload-10.4.1-win-x64.zip",
            "PS5Upload-10.4.1 copy-win-x64.zip",
        ] {
            assert_eq!(release_file_version(not_ours), None, "{not_ours}");
        }

        let dir = tmp("downloads");
        for n in [
            "PS5Upload-10.3.0-win-x64.zip",
            "PS5Upload-10.4.1-win-x64.zip",
            // Newer than the running copy: maybe waiting to be installed by hand.
            "PS5Upload-10.6.0-win-x64.zip",
            "PS5Upload-10.3.0-win-x64 (1).zip",
            "holiday.zip",
        ] {
            std::fs::write(dir.join(n), b"1234").unwrap();
        }
        let found: Vec<String> = old_downloads(&dir, "10.4.1")
            .into_iter()
            .map(|(p, n)| {
                assert_eq!(n, 4);
                p.file_name().unwrap().to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(
            found,
            [
                "PS5Upload-10.3.0-win-x64.zip",
                "PS5Upload-10.4.1-win-x64.zip"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
