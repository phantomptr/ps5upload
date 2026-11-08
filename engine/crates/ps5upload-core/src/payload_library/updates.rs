//! Keeping the payloads on this computer up to date ("any way to auto update all elfs?").
//!
//! - **Check**: every payload in the cache (and any the user keeps in a console's Payload Manager)
//!   against its source's latest release. The release cache's hour keeps a daily check cheap;
//!   once the host answers with its rate limit (403/429), the rest of the check reads the cache
//!   instead of asking again.
//! - **Update**: the newer release goes through [`Library::download`], so it gets exactly the
//!   checks every download gets (https on the repo's host, size cap, checksum list, ELF magic).
//!   The copy it replaces is kept in `<id>/previous/`, so one click puts it back.
//! - **Roll back**: swaps the current copy and the kept one; rolling back twice is rolling forward.
//!
//! Every write goes through a temporary name in the same folder and a rename, so a reader never
//! sees half a file. These are files on this computer; nothing here renames on a console.
//!
//! Files: `<dir>/update-settings.json` (the "Keep payloads up to date" switch),
//! `<dir>/update-state.json` (the last check, what was applied, the last Payload Manager syncs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::download::remove_other_elfs;
use super::{
    atomic_write, elf_in_dir, is_safe_payload_id, replace_file, unique_suffix, Library,
    LibraryError, ReleaseInfo, TmpFile,
};

/// The folder in a payload's cache that holds the copy an update replaced.
pub const PREVIOUS_DIR: &str = "previous";
const SETTINGS_FILE: &str = "update-settings.json";
const STATE_FILE: &str = "update-state.json";
/// How many applied updates the state remembers.
const APPLIED_CAP: usize = 30;
/// A staging folder older than this was left by a crash.
const STALE_STAGING_AGE: Duration = Duration::from_secs(3600);

/// The user's update settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSettings {
    /// "Keep payloads up to date": check at start and daily, and apply what is newer. Off
    /// unless the user turns it on.
    #[serde(default)]
    pub auto_update: bool,
}

/// One payload in an update check.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateRow {
    pub payload_id: String,
    pub display_name: String,
    pub is_custom: bool,
    /// The version on this computer; empty when it is not downloaded yet.
    pub installed_version: String,
    /// The source's latest release; empty when it could not be read.
    pub latest_version: String,
    pub update_available: bool,
    /// The version an update replaced, kept for a rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub html_url: String,
    /// Why the latest release is unknown or may be old (a failed fetch, the rate limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An update that was applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedUpdate {
    pub payload_id: String,
    pub display_name: String,
    /// Empty when there was no copy before.
    pub from: String,
    pub to: String,
    /// Seconds since the Unix epoch.
    pub at: i64,
    /// True when it was a rollback.
    #[serde(default)]
    pub rollback: bool,
}

/// What the app shows without asking the network: the last check, what was applied, the last
/// Payload Manager syncs. The two counters only grow, so a screen can tell what is new to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateState {
    /// When the last check ran (seconds since the Unix epoch); 0 for never.
    #[serde(default)]
    pub checked_at: i64,
    #[serde(default)]
    pub rows: Vec<UpdateRow>,
    /// The last check met the host's rate limit and showed cached releases from then on.
    #[serde(default)]
    pub rate_limited: bool,
    /// Newest first.
    #[serde(default)]
    pub applied: Vec<AppliedUpdate>,
    /// Grows when a check finds an update it had not found before.
    #[serde(default)]
    pub available_seq: u64,
    /// Grows when an update or a rollback is applied.
    #[serde(default)]
    pub applied_seq: u64,
    /// When the automatic check last ran.
    #[serde(default)]
    pub last_auto_run_at: i64,
    /// The last Payload Manager sync of each console.
    #[serde(default)]
    pub pm_syncs: BTreeMap<String, super::pm_sync::PmSyncReport>,
}

impl UpdateState {
    /// The rows with an update.
    pub fn available(&self) -> impl Iterator<Item = &UpdateRow> {
        self.rows.iter().filter(|r| r.update_available)
    }
}

/// What an update did.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateOutcome {
    pub payload_id: String,
    pub display_name: String,
    /// False when the copy here was already the latest.
    pub updated: bool,
    pub from: String,
    pub to: String,
    pub path: String,
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The numbers of a tag (`v1.2.10-rc1` → [1, 2, 10]) and whether something follows them (a
/// pre-release suffix). None when it has no number at all (`latest`, `nightly`).
fn version_parts(tag: &str) -> Option<(Vec<u64>, bool)> {
    let start = tag.find(|c: char| c.is_ascii_digit())?;
    let rest = &tag[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let nums: Vec<u64> = rest[..end]
        .split('.')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(u64::MAX))
        .collect();
    (!nums.is_empty()).then_some((nums, end < rest.len()))
}

/// Whether `latest` is newer than `installed`. Numbered tags compare by number, so a newer
/// pre-release the user picked by hand is not "updated" back to an older stable one. A tag
/// without numbers (a rolling `latest`) is newer whenever it differs.
pub fn is_newer(latest: &str, installed: &str) -> bool {
    let (latest, installed) = (latest.trim(), installed.trim());
    if latest.is_empty() || latest == installed {
        return false;
    }
    if installed.is_empty() {
        return true;
    }
    match (version_parts(latest), version_parts(installed)) {
        (Some((l, l_suffix)), Some((i, i_suffix))) => {
            let n = l.len().max(i.len());
            let pad = |v: &[u64]| -> Vec<u64> {
                let mut v = v.to_vec();
                v.resize(n, 0);
                v
            };
            match pad(&l).cmp(&pad(&i)) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                // Same numbers: the release beats its own pre-release, nothing else changes.
                std::cmp::Ordering::Equal => i_suffix && !l_suffix,
            }
        }
        _ => true,
    }
}

/// A release that rebuilt a raw `.elf` under the same tag (a rolling `latest`): the asset's
/// size says so where the tag cannot.
fn rebuilt_under_same_tag(release: &ReleaseInfo, installed: &str, local_size: u64) -> bool {
    release.tag == installed
        && release
            .picked_asset_name
            .to_ascii_lowercase()
            .ends_with(".elf")
        && release.picked_asset_size > 0
        && local_size > 0
        && release.picked_asset_size != local_size
}

/// A host answering with its rate limit: GitHub says 403 to an anonymous client over its 60
/// requests an hour, and 429 to its abuse limit.
fn is_rate_limited(why: &str) -> bool {
    why.contains("HTTP 403") || why.contains("HTTP 429")
}

const RATE_LIMITED: &str =
    "The payload host's rate limit was reached; this is the release as last fetched.";

fn io_err(what: impl std::fmt::Display) -> LibraryError {
    LibraryError::new("io_failed", what.to_string())
}

/// Updates and rollbacks of this library run one at a time in this process.
fn update_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read-modify-write of the state file, one at a time.
fn state_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read-modify-write of the Payload Manager choices, one at a time.
pub(crate) fn choices_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A staging folder beside the cache; removed when dropped unless it was moved into place.
pub(crate) struct StagingDir(PathBuf);

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn write_json<T: Serialize>(dir: &Path, path: &Path, v: &T) -> Result<(), LibraryError> {
    std::fs::create_dir_all(dir).map_err(|e| io_err(format!("mkdir {}: {e}", dir.display())))?;
    let body = serde_json::to_vec_pretty(v).map_err(|e| io_err(format!("encode: {e}")))?;
    atomic_write(path, &body).map_err(|e| io_err(format!("write {}: {e}", path.display())))
}

impl Library {
    pub fn update_settings(&self) -> UpdateSettings {
        read_json(&self.dir.join(SETTINGS_FILE))
    }

    pub fn set_update_settings(&self, s: &UpdateSettings) -> Result<UpdateSettings, LibraryError> {
        write_json(&self.dir, &self.dir.join(SETTINGS_FILE), s)?;
        Ok(s.clone())
    }

    /// The last check, what was applied, and the last syncs. Never touches the network.
    pub fn update_state(&self) -> UpdateState {
        read_json(&self.dir.join(STATE_FILE))
    }

    /// Change the state under its lock.
    pub fn edit_update_state<R>(
        &self,
        f: impl FnOnce(&mut UpdateState) -> R,
    ) -> Result<R, LibraryError> {
        let _g = state_lock();
        let mut state = self.update_state();
        let r = f(&mut state);
        write_json(&self.dir, &self.dir.join(STATE_FILE), &state)?;
        Ok(r)
    }

    /// The version kept by the last update of `id`, if a copy is kept.
    pub fn previous_version(&self, id: &str) -> Option<String> {
        if !is_safe_payload_id(id) {
            return None;
        }
        let dir = self.dir.join(id).join(PREVIOUS_DIR);
        elf_in_dir(&dir)?;
        Some(
            std::fs::read_to_string(dir.join("VERSION"))
                .unwrap_or_default()
                .trim()
                .to_string(),
        )
    }

    /// Check every payload this computer has, and `extra_ids` (payloads kept in a console's
    /// Payload Manager), for a newer release. `force` asks the source even when the hour-long
    /// release cache is fresh; once the host answers with its rate limit, the rest come from
    /// the cache. Saves the result as the state's `rows`.
    pub fn check_updates(
        &self,
        force: bool,
        extra_ids: &[String],
    ) -> Result<UpdateState, LibraryError> {
        let catalog = self.catalog();
        let mut ids: Vec<String> = self
            .local_inventory()
            .into_iter()
            .map(|e| e.payload_id)
            .collect();
        for id in extra_ids {
            if !ids.contains(id) && catalog.iter().any(|p| &p.id == id) {
                ids.push(id.clone());
            }
        }
        let mut rate_limited = false;
        let mut rows = Vec::with_capacity(ids.len());
        for id in &ids {
            let info = catalog.iter().find(|p| &p.id == id);
            let local = self.local_entry(id);
            let (installed, local_size) = local
                .as_ref()
                .map(|l| (l.version.clone(), l.size))
                .unwrap_or_default();
            let release = if rate_limited {
                self.cached_release(id, RATE_LIMITED)
                    .ok_or_else(|| LibraryError::new("rate_limited", RATE_LIMITED))
            } else {
                self.release(id, force)
            };
            let mut row = UpdateRow {
                payload_id: id.clone(),
                display_name: info.map(|p| p.display_name.clone()).unwrap_or_default(),
                is_custom: info.is_some_and(|p| p.is_custom),
                installed_version: installed.clone(),
                previous_version: self.previous_version(id),
                ..UpdateRow::default()
            };
            match release {
                Ok(r) => {
                    if r.refresh_error.as_deref().is_some_and(is_rate_limited) {
                        rate_limited = true;
                    }
                    row.update_available = !r.picked_asset_url.is_empty()
                        && (is_newer(&r.tag, &installed)
                            || rebuilt_under_same_tag(&r, &installed, local_size));
                    row.latest_version = r.tag;
                    row.prerelease = r.prerelease;
                    row.published_at = r.published_at;
                    row.html_url = r.html_url;
                    row.error = r.refresh_error;
                }
                Err(e) => {
                    if is_rate_limited(&e.message) {
                        rate_limited = true;
                    }
                    row.error = Some(e.message);
                }
            }
            rows.push(row);
        }
        self.edit_update_state(|s| {
            let known: Vec<(String, String)> = s
                .available()
                .map(|r| (r.payload_id.clone(), r.latest_version.clone()))
                .collect();
            let found_new = rows.iter().any(|r| {
                r.update_available
                    && !known.contains(&(r.payload_id.clone(), r.latest_version.clone()))
            });
            if found_new {
                s.available_seq += 1;
            }
            s.rows = rows;
            s.checked_at = now_secs();
            s.rate_limited = rate_limited;
            s.clone()
        })
    }

    /// Bring one payload to its latest release, keeping the copy it replaces for a rollback.
    /// A payload not downloaded yet is downloaded. A failed or refused download leaves the copy
    /// here, and the kept one, as they were.
    pub fn update_payload(&self, id: &str) -> Result<UpdateOutcome, LibraryError> {
        let _g = update_lock();
        self.resolve_entry(id)?;
        let display_name = self
            .catalog()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.display_name)
            .unwrap_or_default();
        let before = self.local_entry(id);
        let release = self.release(id, false)?;
        if release.picked_asset_url.is_empty() {
            return Err(LibraryError::new(
                "no_asset",
                format!("release {} of {id} has no payload file", release.tag),
            ));
        }
        if let Some(b) = &before {
            if !is_newer(&release.tag, &b.version)
                && !rebuilt_under_same_tag(&release, &b.version, b.size)
            {
                return Ok(UpdateOutcome {
                    payload_id: id.to_string(),
                    display_name,
                    updated: false,
                    from: b.version.clone(),
                    to: b.version.clone(),
                    path: b.path.clone(),
                });
            }
        }
        let staged = match &before {
            Some(_) => Some(self.stage_current(id)?),
            None => None,
        };
        let local = self.download(id, &release.picked_asset_url, &release.tag)?;
        if let Some(staged) = staged {
            self.promote_previous(id, staged)?;
        }
        let from = before.map(|b| b.version).unwrap_or_default();
        let applied = AppliedUpdate {
            payload_id: id.to_string(),
            display_name: display_name.clone(),
            from: from.clone(),
            to: local.version.clone(),
            at: now_secs(),
            rollback: false,
        };
        self.note_applied(applied)?;
        Ok(UpdateOutcome {
            payload_id: id.to_string(),
            display_name,
            updated: true,
            from,
            to: local.version,
            path: local.path,
        })
    }

    /// Put back the copy the last update replaced; the current one is kept in its place, so a
    /// second rollback rolls forward again.
    pub fn rollback_payload(&self, id: &str) -> Result<UpdateOutcome, LibraryError> {
        let _g = update_lock();
        self.resolve_entry(id)?;
        let display_name = self
            .catalog()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.display_name)
            .unwrap_or_default();
        let current = self.local_entry(id).ok_or_else(|| {
            LibraryError::new("not_downloaded", format!("{id} is not downloaded"))
        })?;
        let prev_dir = self.dir.join(id).join(PREVIOUS_DIR);
        let prev_elf = elf_in_dir(&prev_dir).ok_or_else(|| {
            LibraryError::new("no_previous", format!("no earlier copy of {id} is kept"))
        })?;
        let prev_version = std::fs::read_to_string(prev_dir.join("VERSION"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let name = prev_elf
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io_err("the kept copy has no file name"))?
            .to_string();

        let staged = self.stage_current(id)?;
        let dir = self.dir.join(id);
        let tmp = TmpFile(dir.join(format!("{name}.{}.rollback.tmp", unique_suffix())));
        std::fs::copy(&prev_elf, &tmp.0).map_err(|e| io_err(format!("copy kept copy: {e}")))?;
        let head = std::fs::read(&tmp.0)
            .map(|b| b.starts_with(b"\x7FELF"))
            .unwrap_or(false);
        if !head {
            return Err(LibraryError::new(
                "asset_not_payload",
                format!("the kept copy of {id} is not an ELF"),
            ));
        }
        replace_file(&tmp.0, &dir.join(&name))
            .map_err(|e| io_err(format!("put back {name}: {e}")))?;
        remove_other_elfs(&dir, &name);
        atomic_write(&dir.join("VERSION"), prev_version.as_bytes())
            .map_err(|e| io_err(format!("write VERSION: {e}")))?;
        self.promote_previous(id, staged)?;

        self.note_applied(AppliedUpdate {
            payload_id: id.to_string(),
            display_name: display_name.clone(),
            from: current.version.clone(),
            to: prev_version.clone(),
            at: now_secs(),
            rollback: true,
        })?;
        let path = dir.join(&name).to_string_lossy().to_string();
        Ok(UpdateOutcome {
            payload_id: id.to_string(),
            display_name,
            updated: true,
            from: current.version,
            to: prev_version,
            path,
        })
    }

    fn note_applied(&self, a: AppliedUpdate) -> Result<(), LibraryError> {
        self.edit_update_state(|s| {
            for row in s.rows.iter_mut().filter(|r| r.payload_id == a.payload_id) {
                row.previous_version = Some(a.from.clone()).filter(|v| !v.is_empty());
                row.installed_version = a.to.clone();
                row.update_available = is_newer(&row.latest_version, &a.to);
            }
            s.applied.insert(0, a);
            s.applied.truncate(APPLIED_CAP);
            s.applied_seq += 1;
        })
    }

    /// A copy of the current file and its VERSION in a staging folder beside the cache.
    fn stage_current(&self, id: &str) -> Result<StagingDir, LibraryError> {
        let dir = self.dir.join(id);
        sweep_stale_staging(&dir);
        let current = super::elf_in_dir(&dir).ok_or_else(|| {
            LibraryError::new("not_downloaded", format!("{id} is not downloaded"))
        })?;
        let staging = StagingDir(dir.join(format!("{PREVIOUS_DIR}.{}.tmp", unique_suffix())));
        std::fs::create_dir_all(&staging.0)
            .map_err(|e| io_err(format!("mkdir {}: {e}", staging.0.display())))?;
        let name = current.file_name().ok_or_else(|| io_err("no file name"))?;
        std::fs::copy(&current, staging.0.join(name))
            .map_err(|e| io_err(format!("keep {}: {e}", current.display())))?;
        let version = std::fs::read(dir.join("VERSION")).unwrap_or_default();
        std::fs::write(staging.0.join("VERSION"), version)
            .map_err(|e| io_err(format!("keep VERSION: {e}")))?;
        Ok(staging)
    }

    /// Make a staging folder the kept copy, replacing the one kept before. Both folders are in
    /// the payload's own cache folder: the renames never leave it.
    fn promote_previous(&self, id: &str, staged: StagingDir) -> Result<(), LibraryError> {
        let dir = self.dir.join(id);
        let prev = dir.join(PREVIOUS_DIR);
        let old = StagingDir(dir.join(format!("{PREVIOUS_DIR}.{}.old.tmp", unique_suffix())));
        let had_prev = prev.exists();
        if had_prev {
            std::fs::rename(&prev, &old.0)
                .map_err(|e| io_err(format!("move kept copy aside: {e}")))?;
        }
        if let Err(e) = std::fs::rename(&staged.0, &prev) {
            if had_prev {
                let _ = std::fs::rename(&old.0, &prev);
            }
            return Err(io_err(format!("keep the replaced copy: {e}")));
        }
        drop(old);
        std::mem::forget(staged);
        Ok(())
    }
}

/// Staging folders a crash left behind.
fn sweep_stale_staging(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for f in read.flatten() {
        let name = f.file_name().to_string_lossy().to_string();
        if !(name.starts_with(&format!("{PREVIOUS_DIR}.")) && name.ends_with(".tmp")) {
            continue;
        }
        let old = f
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= STALE_STAGING_AGE);
        if old && f.path().is_dir() {
            let _ = std::fs::remove_dir_all(f.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::library;
    use super::*;

    const ID: &str = "kstuff-echostretch";
    const LATEST: &str = "https://api.github.com/repos/EchoStretch/kstuff-lite/releases/latest";

    fn asset(tag: &str) -> String {
        format!("https://github.com/EchoStretch/kstuff-lite/releases/download/{tag}/kstuff.elf")
    }

    fn release_json(tag: &str, size: usize) -> String {
        format!(
            r#"{{"tag_name":"{tag}","assets":[{{"name":"kstuff.elf","browser_download_url":"{}","size":{size}}}]}}"#,
            asset(tag)
        )
    }

    fn elf(fill: u8, len: usize) -> Vec<u8> {
        let mut v = b"\x7FELF".to_vec();
        v.resize(len, fill);
        v
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("v1.10", "v1.9"));
        assert!(is_newer("2.0", "v1.9.9"));
        assert!(!is_newer("v1.9", "v1.10"));
        assert!(!is_newer("v1.2", "v1.2"));
        assert!(!is_newer("1.2", "v1.2"), "same numbers, other decoration");
        assert!(
            is_newer("v1.2", "v1.2-rc1"),
            "the release after its pre-release"
        );
        assert!(
            !is_newer("v1.2", "v1.3-rc1"),
            "a newer pre-release picked by hand stays"
        );
        assert!(
            is_newer("latest", "nightly"),
            "tags without numbers: any change"
        );
        assert!(!is_newer("latest", "latest"));
        assert!(is_newer("v1", ""), "nothing here yet");
        assert!(!is_newer("", "v1"));
    }

    #[test]
    fn a_check_finds_the_newer_release_and_counts_it_once() {
        let (_d, http, lib) = library("check");
        http.ok(&asset("v1"), &elf(1, 64));
        lib.download(ID, &asset("v1"), "v1").unwrap();
        http.ok(LATEST, release_json("v2", 80).as_bytes());

        let s = lib.check_updates(true, &[]).unwrap();
        assert_eq!(s.rows.len(), 1);
        let row = &s.rows[0];
        assert_eq!(row.payload_id, ID);
        assert_eq!(row.installed_version, "v1");
        assert_eq!(row.latest_version, "v2");
        assert!(row.update_available);
        assert!(row.display_name.contains("kstuff"));
        assert_eq!(s.available_seq, 1);
        assert!(s.checked_at > 0);

        // The same update found again is not news.
        let again = lib.check_updates(false, &[]).unwrap();
        assert_eq!(again.available_seq, 1);
        assert_eq!(lib.update_state().rows, again.rows);
    }

    #[test]
    fn the_current_release_is_not_an_update() {
        let (_d, http, lib) = library("current");
        http.ok(&asset("v2"), &elf(1, 64));
        lib.download(ID, &asset("v2"), "v2").unwrap();
        http.ok(LATEST, release_json("v2", 64).as_bytes());
        let s = lib.check_updates(true, &[]).unwrap();
        assert!(!s.rows[0].update_available);
        assert_eq!(s.available_seq, 0);
    }

    #[test]
    fn a_rolling_tag_rebuilt_shows_by_its_size() {
        let (_d, http, lib) = library("rolling");
        let url = "https://github.com/EchoStretch/kstuff-lite/releases/download/latest/kstuff.elf";
        http.ok(url, &elf(1, 64));
        lib.download(ID, url, "latest").unwrap();
        let rel = |size: usize| {
            format!(
                r#"{{"tag_name":"latest","assets":[{{"name":"kstuff.elf","browser_download_url":"{url}","size":{size}}}]}}"#
            )
        };
        http.ok(LATEST, rel(64).as_bytes());
        assert!(!lib.check_updates(true, &[]).unwrap().rows[0].update_available);
        http.ok(LATEST, rel(96).as_bytes());
        assert!(lib.check_updates(true, &[]).unwrap().rows[0].update_available);
    }

    #[test]
    fn after_the_rate_limit_the_check_reads_the_cache_only() {
        let (_d, http, lib) = library("ratelimit");
        let smp_latest = "https://api.github.com/repos/drakmor/shadowMountPlus/releases/latest";
        // Two payloads here; both release lists cached from an earlier check.
        for (id, url, tag) in [
            (ID, asset("v1"), "v1"),
            (
                "shadowmountplus",
                "https://github.com/drakmor/shadowMountPlus/releases/download/v1/shadowmountplus.elf"
                    .to_string(),
                "v1",
            ),
        ] {
            http.ok(&url, &elf(1, 64));
            lib.download(id, &url, tag).unwrap();
        }
        http.ok(LATEST, release_json("v2", 80).as_bytes());
        http.ok(
            smp_latest,
            br#"{"tag_name":"v3","assets":[{"name":"shadowmountplus.elf","browser_download_url":"https://github.com/drakmor/shadowMountPlus/releases/download/v3/shadowmountplus.elf","size":9}]}"#,
        );
        let first = lib.check_updates(true, &[]).unwrap();
        assert_eq!(first.rows.len(), 2, "{:?}", first.rows);
        assert!(!first.rate_limited);

        // Now the host says 403 to everything.
        http.status(LATEST, 403);
        http.status(
            "https://api.github.com/repos/EchoStretch/kstuff-lite/releases?per_page=1",
            403,
        );
        http.status(smp_latest, 403);
        let before = http.asked.lock().unwrap().len();
        let s = lib.check_updates(true, &[]).unwrap();
        assert!(s.rate_limited);
        let asked: Vec<String> = http.asked.lock().unwrap()[before..].to_vec();
        assert!(
            !asked.iter().any(|u| u.contains("shadowMountPlus")),
            "nothing more is asked after the limit: {asked:?}"
        );
        // Both still show what was last known.
        assert!(s.rows.iter().all(|r| r.update_available), "{:?}", s.rows);
        assert!(s.rows.iter().all(|r| r.error.is_some()));
    }

    #[test]
    fn an_update_keeps_the_replaced_copy_and_a_rollback_swaps_them() {
        let (_d, http, lib) = library("update");
        let v1 = elf(1, 64);
        let v2 = elf(2, 80);
        http.ok(&asset("v1"), &v1);
        lib.download(ID, &asset("v1"), "v1").unwrap();
        http.ok(LATEST, release_json("v2", 80).as_bytes());
        http.ok(&asset("v2"), &v2);

        let out = lib.update_payload(ID).unwrap();
        assert!(out.updated);
        assert_eq!((out.from.as_str(), out.to.as_str()), ("v1", "v2"));
        assert_eq!(std::fs::read(&out.path).unwrap(), v2);
        assert_eq!(lib.previous_version(ID).as_deref(), Some("v1"));
        let dir = lib.dir().join(ID);
        assert_eq!(
            files_in(&dir),
            ["VERSION", "kstuff.elf", "previous", "release.json"]
        );
        assert_eq!(
            std::fs::read(dir.join(PREVIOUS_DIR).join("kstuff.elf")).unwrap(),
            v1
        );
        let state = lib.update_state();
        assert_eq!(state.applied_seq, 1);
        assert_eq!(state.applied[0].to, "v2");

        // Already current: nothing is downloaded and the kept copy stays.
        let asked = http.asked.lock().unwrap().len();
        assert!(!lib.update_payload(ID).unwrap().updated);
        assert_eq!(http.asked.lock().unwrap().len(), asked);

        // One click back, and one click forward again.
        let back = lib.rollback_payload(ID).unwrap();
        assert_eq!((back.from.as_str(), back.to.as_str()), ("v2", "v1"));
        assert_eq!(std::fs::read(dir.join("kstuff.elf")).unwrap(), v1);
        assert_eq!(lib.local_entry(ID).unwrap().version, "v1");
        assert_eq!(lib.previous_version(ID).as_deref(), Some("v2"));
        let fwd = lib.rollback_payload(ID).unwrap();
        assert_eq!(fwd.to, "v2");
        assert_eq!(std::fs::read(dir.join("kstuff.elf")).unwrap(), v2);
        assert!(lib.update_state().applied[0].rollback);
        // No staging folder or temporary file is left.
        assert_eq!(
            files_in(&dir),
            ["VERSION", "kstuff.elf", "previous", "release.json"]
        );
    }

    #[test]
    fn a_refused_download_leaves_both_copies_as_they_were() {
        let (_d, http, lib) = library("refused");
        let v1 = elf(1, 64);
        http.ok(&asset("v1"), &v1);
        lib.download(ID, &asset("v1"), "v1").unwrap();
        http.ok(LATEST, release_json("v2", 80).as_bytes());
        // An HTML page where the ELF should be: download verification refuses it.
        http.ok(&asset("v2"), b"<html>not found</html>");
        let e = lib.update_payload(ID).unwrap_err();
        assert_eq!(e.code, "asset_not_payload");
        let dir = lib.dir().join(ID);
        assert_eq!(std::fs::read(dir.join("kstuff.elf")).unwrap(), v1);
        assert_eq!(lib.local_entry(ID).unwrap().version, "v1");
        assert!(lib.previous_version(ID).is_none());
        assert_eq!(files_in(&dir), ["VERSION", "kstuff.elf", "release.json"]);
        assert_eq!(lib.update_state().applied_seq, 0);
    }

    #[test]
    fn a_size_that_lies_is_refused_too() {
        let (_d, http, lib) = library("truncated");
        http.ok(&asset("v1"), &elf(1, 64));
        lib.download(ID, &asset("v1"), "v1").unwrap();
        http.ok(LATEST, release_json("v2", 80).as_bytes());
        http.ok(&asset("v2"), b"\x7FE");
        assert_eq!(
            lib.update_payload(ID).unwrap_err().code,
            "asset_not_payload"
        );
        assert_eq!(lib.local_entry(ID).unwrap().version, "v1");
    }

    #[test]
    fn nothing_to_roll_back_says_so() {
        let (_d, http, lib) = library("noprev");
        assert_eq!(lib.rollback_payload(ID).unwrap_err().code, "not_downloaded");
        http.ok(&asset("v1"), &elf(1, 64));
        lib.download(ID, &asset("v1"), "v1").unwrap();
        assert_eq!(lib.rollback_payload(ID).unwrap_err().code, "no_previous");
        assert_eq!(
            lib.rollback_payload("../x").unwrap_err().code,
            "unknown_payload"
        );
    }

    #[test]
    fn settings_default_off_and_round_trip() {
        let (_d, _h, lib) = library("settings");
        assert!(!lib.update_settings().auto_update);
        lib.set_update_settings(&UpdateSettings { auto_update: true })
            .unwrap();
        assert!(lib.update_settings().auto_update);
    }
}
