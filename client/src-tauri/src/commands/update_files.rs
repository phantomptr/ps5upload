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
//!
//! While an install works, the `installing` file beside the record names the installer (its
//! PID and when it started); the installer deletes it when it ends. A launch that finds it and
//! a live installer does not judge yet (`settle_unless_installing`): it waits for the installer
//! and starts again into the new version (`self_install::update_wait_for_install`). A marker
//! whose installer is gone is stale: it is removed, the log says so, and the install is judged
//! as usual. So "update failed" is said only for an install that ended without installing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const RECORD: &str = "install.json";
const LOG: &str = "update.log";
/// The file an installer keeps while it works: `<pid> <start, Unix seconds>`.
const MARKER: &str = "installing";
/// A marker older than this is stale whatever its PID says (a PID is used again in time): an
/// install waits at most 30 s for the app to quit and then copies a few hundred megabytes.
pub const MARKER_MAX_AGE_SECS: u64 = 15 * 60;
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
        let _ = writeln!(f, "{} {line}", now_secs());
    }
}

/// The installer at work, as its marker names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    pub pid: u32,
    /// When it started, Unix seconds.
    pub started: u64,
}

/// What a marker says at a launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    /// No install is running.
    None,
    /// The installer is still working: wait for it, judge nothing yet.
    Live(Marker),
    /// The installer is gone (or the marker is too old to believe) but left its marker: it
    /// did not finish.
    Stale(Marker),
}

pub(crate) fn marker_path(dir: &Path) -> PathBuf {
    dir.join(MARKER)
}

/// The marker's one line. The scripts write the same with `echo "$$ $(date +%s)"`.
pub fn marker_line(m: Marker) -> String {
    format!("{} {}\n", m.pid, m.started)
}

/// Read a marker's text: exactly a PID and a start time. Anything else is no marker.
pub fn parse_marker(text: &str) -> Option<Marker> {
    let mut parts = text.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok().filter(|p| *p != 0)?;
    let started = parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some(Marker { pid, started })
}

pub(crate) fn read_marker(dir: &Path) -> Option<Marker> {
    parse_marker(&std::fs::read_to_string(marker_path(dir)).ok()?)
}

pub(crate) fn write_marker(dir: &Path, m: Marker) {
    let _ = std::fs::write(marker_path(dir), marker_line(m));
}

pub(crate) fn remove_marker(dir: &Path) {
    let _ = std::fs::remove_file(marker_path(dir));
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Judge a marker: live only while its process runs (`alive`) and it is younger than
/// `MARKER_MAX_AGE_SECS`. A start time in the future (the clock was set back) is not trusted
/// past the same margin.
pub fn marker_state(marker: Option<Marker>, now: u64, alive: bool) -> MarkerState {
    let Some(m) = marker else {
        return MarkerState::None;
    };
    let age_ok = now.abs_diff(m.started) <= MARKER_MAX_AGE_SECS;
    if alive && age_ok {
        MarkerState::Live(m)
    } else {
        MarkerState::Stale(m)
    }
}

/// The marker in `dir` judged now; `alive` tells whether a PID still runs.
pub fn marker_state_in(dir: &Path, now: u64, alive: &dyn Fn(u32) -> bool) -> MarkerState {
    let marker = read_marker(dir);
    marker_state(marker, now, marker.is_some_and(|m| alive(m.pid)))
}

/// A stale marker is removed. When the version it was installing is still not the one running
/// and nothing else gave a reason, the log says what happened, so the failure reported next
/// carries the real one. (The Windows setup never removes its marker: gone with the new version
/// running is its normal end, and nothing is logged.)
pub(crate) fn clear_stale_marker(dir: &Path, m: Marker, current: &str) {
    remove_marker(dir);
    let failed = read_record(dir)
        .is_some_and(|r| r.error.is_none() && super::updates::is_newer(current, &r.to));
    if failed {
        log(
            dir,
            &format!(
                "the installer (process {}) ended without finishing the update",
                m.pid
            ),
        );
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
    /// The installer is still working: nothing is judged yet, and the renderer waits for it
    /// (`update_wait_for_install`) instead of saying anything.
    pub pending: bool,
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
        pending: false,
    }
}

/// `settle`, unless an installer is still at work (a live marker) and this is not yet the
/// version it installs: then nothing is judged, cleaned or marked as reported, and the answer
/// only says an install is pending. A stale marker is cleared first (`clear_stale_marker`),
/// then the install is judged as usual. Already running the new version while the installer
/// tidies up (or the Windows setup, which starts the app before it ends) is a success now.
pub fn settle_unless_installing(
    dir: &Path,
    current: &str,
    now: u64,
    alive: &dyn Fn(u32) -> bool,
) -> Option<LastInstall> {
    match marker_state_in(dir, now, alive) {
        MarkerState::Live(_) => {
            let record = read_record(dir);
            let arrived = record
                .as_ref()
                .is_some_and(|r| !super::updates::is_newer(current, &r.to));
            if !arrived {
                return Some(LastInstall {
                    version: record.map(|r| r.to).unwrap_or_default(),
                    installed: false,
                    message: String::new(),
                    new: false,
                    pending: true,
                });
            }
        }
        MarkerState::Stale(m) => clear_stale_marker(dir, m, current),
        MarkerState::None => {}
    }
    settle(dir, current)
}

/// What the last install came to, once per launch, and the clean-up that goes with it: after a
/// success the record and the download go (the log says it worked); after a failure the download is kept for
/// "Try again". A download of a newer version stays too: a finished one is reused, and a `.part`
/// carries on where it stopped (a slow link may need more than one sitting). Anything else in the
/// updates folder (older downloads, a `.part` of a version that is no longer new) goes.
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
            // The log stays (#611): an update that failed before this one leaves its steps
            // for a report, which a clean folder could not explain. `log` keeps it small.
            log(
                dir,
                &format!("{current} is running: the install of {} worked", r.to),
            );
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
/// `current`, downloads of a version newer than that, finished or not.
pub fn clean_dir(dir: &Path, keep: Option<&Path>, current: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        if name == RECORD || name == LOG || name == MARKER || Some(path.as_path()) == keep {
            continue;
        }
        let pending = current.is_some_and(|cur| {
            name.to_str().is_some_and(|n| {
                release_file_version(n).is_some_and(|v| super::updates::is_newer(cur, v))
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

/// Runs file work off the main thread: a Downloads folder can hold thousands of entries (or be on
/// a network home), and the temp folder more.
async fn off_main_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("update file task: {e}"))?
}

/// The last install's outcome (once per launch; see `settle`), or None when there was none.
#[tauri::command]
pub async fn update_last_install(app: AppHandle) -> Result<Option<LastInstall>, String> {
    let current = app.package_info().version.to_string();
    off_main_thread(move || {
        let dir = updates_dir(&app)?;
        super::self_install::clean_leftovers();
        Ok(settle_unless_installing(
            &dir,
            &current,
            now_secs(),
            &super::self_install::pid_alive,
        ))
    })
    .await
}

/// Forget a failed install, so it may be tried again by itself ("Try again").
#[tauri::command]
pub async fn update_forget_failure(app: AppHandle) -> Result<(), String> {
    off_main_thread(move || {
        forget_failure(&updates_dir(&app)?);
        Ok(())
    })
    .await
}

/// Update downloads earlier versions of the app left in the Downloads folder.
#[tauri::command]
pub async fn update_old_downloads(app: AppHandle) -> Result<OldDownloads, String> {
    let Ok(dir) = app.path().download_dir() else {
        return Ok(OldDownloads { count: 0, bytes: 0 });
    };
    let current = app.package_info().version.to_string();
    off_main_thread(move || {
        let files = old_downloads(&dir, &current);
        Ok(OldDownloads {
            count: files.len(),
            bytes: files.iter().map(|(_, n)| n).sum(),
        })
    })
    .await
}

/// Delete them (the person asked: the "Remove" button). Returns how many were deleted.
#[tauri::command]
pub async fn update_remove_old_downloads(app: AppHandle) -> Result<usize, String> {
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| format!("no Downloads folder: {e}"))?;
    let current = app.package_info().version.to_string();
    off_main_thread(move || {
        let files = old_downloads(&dir, &current);
        Ok(files
            .iter()
            .filter(|(p, _)| std::fs::remove_file(p).is_ok())
            .count())
    })
    .await
}

/// Open `update.log` in the system's viewer for it (Settings › Updates). False when there is
/// no log yet. Never waits for the viewer (`reveal` only spawns it).
#[tauri::command]
pub async fn update_open_log(app: AppHandle) -> Result<bool, String> {
    let path = log_path(&updates_dir(&app)?);
    if !path.is_file() {
        return Ok(false);
    }
    super::reveal::reveal(&path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp::temp_path(format!(
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
    fn a_successful_install_leaves_only_its_log() {
        let dir = tmp("ok");
        let r = record(&dir, "10.5.1");
        std::fs::write(&r.file, b"zip").unwrap();
        std::fs::write(dir.join("PS5Upload-10.5.0-win-x64.zip"), b"older").unwrap();
        std::fs::write(dir.join("PS5Upload-10.5.1-win-x64.part"), b"half").unwrap();
        write_record(&dir, &r).unwrap();
        log(&dir, "replaced PS5Upload.exe");
        let out = settle(&dir, "10.5.1").unwrap();
        assert!(out.installed && out.message.is_empty() && out.version == "10.5.1");
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec![LOG.to_string()], "only the log stays");
        let log_text = std::fs::read_to_string(log_path(&dir)).unwrap();
        assert!(log_text.contains("replaced PS5Upload.exe"), "{log_text}");
        assert!(
            log_text.contains("10.5.1 is running: the install of 10.5.1 worked"),
            "{log_text}"
        );
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
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        // The `.part` stays too: a slow link's download carries on after a restart (F1).
        assert_eq!(
            left,
            [
                "PS5Upload-10.6.0-win-x64.part",
                "PS5Upload-10.6.0-win-x64.zip"
            ]
        );
        // Once that version runs, they go too.
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

    #[test]
    fn a_marker_is_a_pid_and_a_start_time_and_nothing_else() {
        let m = Marker {
            pid: 4242,
            started: 1_800_000_000,
        };
        assert_eq!(parse_marker(&marker_line(m)), Some(m));
        // What the scripts write: `echo "$$ $(date +%s)"`.
        assert_eq!(parse_marker("4242 1800000000\n"), Some(m));
        for bad in [
            "",
            "4242",
            "0 1800000000",
            "-1 1800000000",
            "abc 1800000000",
            "4242 soon",
            "4242 1800000000 extra",
        ] {
            assert_eq!(parse_marker(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_marker_is_live_only_while_its_installer_runs_and_it_is_recent() {
        let m = Marker {
            pid: 7,
            started: 1_000,
        };
        assert_eq!(marker_state(None, 1_000, true), MarkerState::None);
        assert_eq!(marker_state(Some(m), 1_030, true), MarkerState::Live(m));
        // The installer is gone and left its marker.
        assert_eq!(marker_state(Some(m), 1_030, false), MarkerState::Stale(m));
        // Too old to believe: that PID is some other program by now.
        assert_eq!(
            marker_state(Some(m), 1_000 + MARKER_MAX_AGE_SECS, true),
            MarkerState::Live(m)
        );
        assert_eq!(
            marker_state(Some(m), 1_001 + MARKER_MAX_AGE_SECS, true),
            MarkerState::Stale(m)
        );
        // The clock was set back: a start far in the future is not trusted either.
        assert_eq!(marker_state(Some(m), 900, true), MarkerState::Live(m));
        let future = Marker {
            pid: 7,
            started: 1_000 + 2 * MARKER_MAX_AGE_SECS,
        };
        assert_eq!(
            marker_state(Some(future), 1_000, true),
            MarkerState::Stale(future)
        );
    }

    /// The app opened again while the install from the last quit still works: nothing is
    /// judged, reported or cleaned, however often it is asked; once the installer is done the
    /// install is judged as usual.
    #[test]
    fn a_launch_during_an_install_waits_instead_of_reporting_a_failure() {
        // A folder of its own: "pending" is the test above's, and the two run at once.
        let dir = tmp("installing");
        let r = record(&dir, "10.5.1");
        std::fs::write(&r.file, b"zip").unwrap();
        write_record(&dir, &r).unwrap();
        log(
            &dir,
            "installing 10.5.1 over 10.4.1 from PS5Upload-10.5.1-win-x64.zip",
        );
        let now = now_secs();
        write_marker(
            &dir,
            Marker {
                pid: 4242,
                started: now,
            },
        );
        for _ in 0..2 {
            let pending =
                settle_unless_installing(&dir, "10.4.1", now, &|pid| pid == 4242).unwrap();
            assert!(pending.pending && !pending.installed && !pending.new);
            assert_eq!(pending.version, "10.5.1");
            assert_eq!(pending.message, "");
        }
        assert!(!read_record(&dir).unwrap().reported, "nothing was reported");
        assert!(r.file.is_file() && marker_path(&dir).is_file());
        // The installer finished and removed its marker; the new version runs.
        remove_marker(&dir);
        let done = settle_unless_installing(&dir, "10.5.1", now, &|_| false).unwrap();
        assert!(done.installed && !done.pending);
        assert_eq!(read_record(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_install_that_really_failed_is_still_reported_with_its_reason() {
        let dir = tmp("pending-failed");
        let r = record(&dir, "10.5.1");
        std::fs::write(&r.file, b"zip").unwrap();
        write_record(&dir, &r).unwrap();
        let now = now_secs();
        write_marker(
            &dir,
            Marker {
                pid: 4242,
                started: now,
            },
        );
        assert!(
            settle_unless_installing(&dir, "10.4.1", now, &|_| true)
                .unwrap()
                .pending
        );
        // The script gave its reason and ended (marker removed).
        log(&dir, "could not move /Applications/PS5Upload.app aside");
        remove_marker(&dir);
        let failed = settle_unless_installing(&dir, "10.4.1", now, &|_| false).unwrap();
        assert!(!failed.installed && !failed.pending && failed.new);
        assert_eq!(
            failed.message,
            "could not move /Applications/PS5Upload.app aside"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_marker_is_cleared_and_says_what_happened() {
        let dir = tmp("stale");
        let r = record(&dir, "10.5.1");
        std::fs::write(&r.file, b"zip").unwrap();
        write_record(&dir, &r).unwrap();
        log(&dir, "installing 10.5.1 over 10.4.1");
        let now = now_secs();
        write_marker(
            &dir,
            Marker {
                pid: 4242,
                started: now,
            },
        );
        // The installer's process is gone (killed, or the computer restarted) mid-way.
        let out = settle_unless_installing(&dir, "10.4.1", now, &|_| false).unwrap();
        assert!(!out.installed && !out.pending && out.new);
        assert_eq!(
            out.message,
            "the installer (process 4242) ended without finishing the update"
        );
        assert!(!marker_path(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Windows setup cannot remove its marker: its process ending with the new version
    /// running is a plain success, and the log does not call it unfinished. The same while it
    /// is still closing (it starts the app before it ends).
    #[test]
    fn the_new_version_running_is_a_success_whatever_the_marker_says() {
        for alive in [false, true] {
            let dir = tmp(if alive {
                "arrived-live"
            } else {
                "arrived-stale"
            });
            let r = record(&dir, "10.5.1");
            std::fs::write(&r.file, b"setup").unwrap();
            write_record(&dir, &r).unwrap();
            let now = now_secs();
            write_marker(
                &dir,
                Marker {
                    pid: 4242,
                    started: now,
                },
            );
            let out = settle_unless_installing(&dir, "10.5.1", now, &|_| alive).unwrap();
            assert!(out.installed && !out.pending, "alive={alive}");
            let text = std::fs::read_to_string(log_path(&dir)).unwrap();
            assert!(!text.contains("without finishing"), "{text}");
            assert!(text.contains("the install of 10.5.1 worked"), "{text}");
            // A marker left behind (Windows) is cleared by the next launch, silently.
            if alive {
                assert!(marker_path(&dir).is_file());
                assert_eq!(
                    settle_unless_installing(&dir, "10.5.1", now, &|_| false),
                    None
                );
            }
            assert!(!marker_path(&dir).exists());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn cleaning_the_folder_keeps_a_running_installers_marker() {
        let dir = tmp("clean-marker");
        write_marker(&dir, Marker { pid: 1, started: 1 });
        std::fs::write(dir.join("stray.tmp"), b"x").unwrap();
        clean_dir(&dir, None, Some("10.4.1"));
        assert!(marker_path(&dir).is_file());
        assert!(!dir.join("stray.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
