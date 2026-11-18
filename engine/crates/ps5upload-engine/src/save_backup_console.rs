//! A copy of each automatic save backup on a drive in the PS5 itself: its M.2 (extended storage)
//! or a USB drive, never the internal storage. Resetting a PS5 wipes the internal drive, saves
//! with it; an M.2 or USB drive keeps its files.
//!
//! When a drive is chosen (`Settings::console_drive`, such as `/mnt/ext0`), each backup the engine
//! makes in the backup folder (automatic, Back up now, snapshot) is checked there (every entry
//! read against its CRC), then uploaded with the engine's ordinary file upload to
//! `<drive>/ps5upload/save-backups/<title_id>/<same file name>` on the console it came from. The
//! copy is hashed on the console (BLAKE3) and compared with the local file; a copy that does not
//! match is deleted. No file is renamed on the console. Then the oldest copies past the local
//! `keep` are deleted, of that save and kind only, and only files named the way this app names its
//! backups.
//!
//! A drive that is not plugged in (or not mounted, or read-only) is skipped with a note in the
//! status: the backup in the folder is made either way, and never fails because of the copy.
//!
//! The game's backups list shows the copies on the drive, and restoring one downloads it to a
//! temporary folder and puts it back as a local backup is put back (`save_backup::restore_zip_any_user`),
//! which keeps the save that is there now first.

use std::path::{Path, PathBuf};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::save_archive::{
    auto_backup_stem, backup_tag, cleanup_temp_dir, make_temp_dir, tagged_backups_to_prune,
};
use ps5upload_core::volumes::Volume;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::save_backup::{self as local, enc, json_error, parse_backup_name, plain, BackupFile};

/// The folder on the drive the copies go in, by title.
pub const FOLDER: &str = "ps5upload/save-backups";

/// What the copying needs from a console. `Live` asks the engine's own routes over loopback, the
/// way the backups themselves are made; the tests use a fake one.
pub(crate) trait Drive {
    /// The console's mounted volumes.
    fn volumes(&self) -> Result<Vec<Volume>, String>;
    /// The entries of the folder `dir`: `(name, is_dir, size)`.
    fn list(&self, dir: &str) -> Result<Vec<(String, bool, u64)>, String>;
    /// Makes `dir` and any missing parents.
    fn mkdir(&self, dir: &str) -> Result<(), String>;
    /// Uploads the local file `src` to the console file `dest`.
    fn upload(&self, src: &Path, dest: &str) -> Result<(), String>;
    /// The BLAKE3 of the console file `path` (hex).
    fn hash(&self, path: &str) -> Result<String, String>;
    /// Deletes the console file `path`.
    fn delete(&self, path: &str) -> Result<(), String>;
    /// Copies the console file `src` into the local folder `dest_dir` (same name).
    fn download(&self, src: &str, dest_dir: &Path) -> Result<(), String>;
}

/// The console at `host`, through the engine's own routes.
pub(crate) struct Live<'a>(pub &'a str);

impl Drive for Live<'_> {
    fn volumes(&self) -> Result<Vec<Volume>, String> {
        let v = local::get(&format!("/api/ps5/volumes?addr={}", enc(self.0)))?;
        serde_json::from_value(v.get("volumes").cloned().unwrap_or(Value::Array(vec![])))
            .map_err(|e| format!("volumes: {e}"))
    }

    fn list(&self, dir: &str) -> Result<Vec<(String, bool, u64)>, String> {
        Ok(local::folder_entries(self.0, dir)?
            .into_iter()
            .filter_map(|e| {
                let name = e.get("name")?.as_str()?.to_string();
                let is_dir = e.get("kind").and_then(Value::as_str) == Some("dir");
                let size = e.get("size").and_then(Value::as_u64).unwrap_or(0);
                Some((name, is_dir, size))
            })
            .collect())
    }

    fn mkdir(&self, dir: &str) -> Result<(), String> {
        local::post("/api/ps5/fs/mkdir", json!({ "addr": self.0, "path": dir })).map(|_| ())
    }

    fn upload(&self, src: &Path, dest: &str) -> Result<(), String> {
        let start = local::post(
            "/api/transfer/file",
            json!({ "addr": self.0, "src": src.to_string_lossy(), "dest": dest }),
        )?;
        let job = start
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or("upload did not start")?;
        local::wait_job(job)
    }

    fn hash(&self, path: &str) -> Result<String, String> {
        let v = local::post(
            "/api/ps5/fs/hash",
            json!({ "addr": self.0, "path": path, "algo": "blake3" }),
        )?;
        v.get("hash")
            .and_then(Value::as_str)
            .map(|h| h.to_ascii_lowercase())
            .ok_or_else(|| "the console did not hash the copy".to_string())
    }

    fn delete(&self, path: &str) -> Result<(), String> {
        local::post(
            "/api/ps5/fs/delete",
            json!({ "addr": self.0, "path": path }),
        )
        .map(|_| ())
    }

    fn download(&self, src: &str, dest_dir: &Path) -> Result<(), String> {
        local::download(self.0, src, dest_dir, "file")
    }
}

/// A drive path the copies may go to: a top-level M.2 (`/mnt/ext<N>`) or USB (`/mnt/usb<N>`)
/// mount. Never the internal storage, a disk image or anything under another folder.
pub fn drive_path_ok(drive: &str) -> bool {
    let Some(rest) = drive
        .strip_prefix("/mnt/ext")
        .or_else(|| drive.strip_prefix("/mnt/usb"))
    else {
        return false;
    };
    !rest.is_empty() && rest.len() <= 2 && rest.bytes().all(|b| b.is_ascii_digit())
}

/// `ext` (M.2 / extended storage) or `usb`, for a drive path that `drive_path_ok` takes.
pub fn drive_kind(drive: &str) -> &'static str {
    if drive.starts_with("/mnt/usb") {
        "usb"
    } else {
        "ext"
    }
}

/// Whether `v` is a drive the copies can go to now: an M.2 or USB mount, really there,
/// writable, and not the internal storage or a disk image.
pub fn eligible(v: &Volume) -> bool {
    drive_path_ok(&v.path)
        && !v.is_placeholder
        && v.writable
        && !v.is_internal_user_storage()
        && !v.is_attached_image()
}

/// The folder of `title_id`'s copies on `drive`.
pub fn title_folder(drive: &str, title_id: &str) -> Result<String, String> {
    if !drive_path_ok(drive) {
        return Err(format!("{drive} is not a PS5 M.2 or USB drive"));
    }
    if !plain(title_id) {
        return Err("unexpected title id".into());
    }
    Ok(format!("{drive}/{FOLDER}/{title_id}"))
}

/// Where the copy of the backup `file` of `title_id` goes on `drive`: the same file name.
pub fn copy_path(drive: &str, title_id: &str, file: &str) -> Result<String, String> {
    if !ours(title_id, file) {
        return Err("unexpected backup name".into());
    }
    Ok(format!("{}/{file}", title_folder(drive, title_id)?))
}

/// Whether `name` is a backup of `title_id` named the way this app names them:
/// `<title>_user<8 hex>_<YYYY-MM-DD_HHMMSS>[_<tag>].zip`. Anything else in the folder (a file the
/// person put there, a copy cut short) is never listed, restored or deleted.
pub fn ours(title_id: &str, name: &str) -> bool {
    if !plain(name) {
        return false;
    }
    let Some((_, stamp)) = parse_backup_name(title_id, name) else {
        return false;
    };
    let b = stamp.as_bytes();
    let shape = b.len() >= 17
        && b[..17].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'_',
            _ => c.is_ascii_digit(),
        });
    shape
        && match &stamp[17..] {
            "" => true,
            tag => tag
                .strip_prefix('_')
                .is_some_and(|t| !t.is_empty() && t.bytes().all(|c| c.is_ascii_lowercase())),
        }
}

/// Which of `names` (the files in a title's folder on the drive) to delete so `keep` copies of
/// this save (`title_id`, `user`) and kind (`tag`) remain: the oldest, and only files of ours.
pub fn to_prune(
    names: &[String],
    title_id: &str,
    user: u32,
    tag: Option<&str>,
    keep: u32,
) -> Vec<String> {
    let mine: Vec<String> = names
        .iter()
        .filter(|n| ours(title_id, n))
        .cloned()
        .collect();
    tagged_backups_to_prune(&mine, &auto_backup_stem(title_id, user), tag, keep)
}

/// The volume at `drive` on the console, when it is one the copies can go to; why not otherwise.
pub(crate) fn check_drive(c: &dyn Drive, drive: &str) -> Result<(), String> {
    if !drive_path_ok(drive) {
        return Err(format!("{drive} is not a PS5 M.2 or USB drive"));
    }
    let vols = c.volumes()?;
    match vols.iter().find(|v| v.path == drive) {
        None => Err(format!("{drive} is not plugged in or not mounted")),
        Some(v) if v.is_placeholder => Err(format!("{drive} is not plugged in or not mounted")),
        Some(v) if !v.writable => Err(format!("{drive} is read-only")),
        Some(v) if !eligible(v) => Err(format!("{drive} can't hold the copies")),
        Some(_) => Ok(()),
    }
}

/// What copying one backup to the console did.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// On the drive at this path, checked.
    Copied(String),
    /// Not copied, and not an error of the copy (the drive is not there).
    Skipped(String),
    /// The copy failed (and nothing of it was left).
    Failed(String),
}

/// Copies the backup `zip` (just made in the backup folder) of `title_id` to `drive` on the
/// console, then prunes the copies there past `keep`. `drive_ok`: what `check_drive` answered,
/// asked once per console and pass.
pub(crate) fn copy_one(
    c: &dyn Drive,
    drive: &str,
    drive_ok: &Result<(), String>,
    title_id: &str,
    zip: &Path,
    keep: u32,
) -> Outcome {
    if let Err(why) = drive_ok {
        return Outcome::Skipped(why.clone());
    }
    let Some(file) = zip.file_name().and_then(|f| f.to_str()) else {
        return Outcome::Failed("unexpected backup name".into());
    };
    let Some((user, _)) = parse_backup_name(title_id, file) else {
        return Outcome::Failed(format!("{file} is not a backup of {title_id}"));
    };
    let (folder, dest) = match (
        title_folder(drive, title_id),
        copy_path(drive, title_id, file),
    ) {
        (Ok(f), Ok(d)) => (f, d),
        (Err(e), _) | (_, Err(e)) => return Outcome::Failed(e),
    };
    // Only a backup that reads back whole here goes to the console.
    if let Err(e) = crate::save_backup_server::check_zip(zip) {
        return Outcome::Failed(format!("{file}: {e}"));
    }
    let want = match blake3_file(zip) {
        Ok(h) => h,
        Err(e) => return Outcome::Failed(e),
    };
    if let Err(e) = c.mkdir(&folder).and_then(|()| c.upload(zip, &dest)) {
        let _ = c.delete(&dest);
        return Outcome::Failed(format!("{file}: {e}"));
    }
    match c.hash(&dest) {
        Ok(got) if got == want => {}
        Ok(_) => {
            let _ = c.delete(&dest);
            return Outcome::Failed(format!("{file}: the copy on the PS5 does not match"));
        }
        Err(e) => {
            let _ = c.delete(&dest);
            return Outcome::Failed(format!("{file}: could not check the copy on the PS5: {e}"));
        }
    }
    // Older copies go only once the new one is there and checked.
    if let Ok(entries) = c.list(&folder) {
        let names: Vec<String> = entries
            .into_iter()
            .filter(|(_, is_dir, _)| !is_dir)
            .map(|(n, _, _)| n)
            .collect();
        for old in to_prune(&names, title_id, user, backup_tag(file), keep) {
            if old != file {
                let _ = c.delete(&format!("{folder}/{old}"));
            }
        }
    }
    Outcome::Copied(dest)
}

/// What the last copies to a PS5 drive did, for the Saves screen and a bug report.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CopyStatus {
    /// Unix ms of the last copy (or skip).
    pub at: u64,
    /// The drive, such as `/mnt/ext0`.
    pub drive: String,
    pub copied: u32,
    pub skipped: u32,
    pub failed: u32,
    /// Why the last one was skipped or failed; None when they all went.
    pub note: Option<String>,
}

/// The copies of one backup pass (or one Back up now): the drive checked once per console.
pub(crate) struct Copies {
    drive: String,
    keep: u32,
    checked: std::collections::BTreeMap<String, Result<(), String>>,
    pub status: CopyStatus,
}

impl Copies {
    /// None when no drive is chosen.
    pub fn new(drive: &str, keep: u32) -> Option<Self> {
        let drive = drive.trim();
        (!drive.is_empty()).then(|| Copies {
            drive: drive.to_string(),
            keep,
            checked: Default::default(),
            status: CopyStatus {
                drive: drive.to_string(),
                ..CopyStatus::default()
            },
        })
    }

    /// Copies the backup `zip` of `title_id`, just made from `host`, to that console's drive.
    /// Never fails: what happened goes in `status` (and the log).
    pub fn copy(&mut self, c: &dyn Drive, host: &str, title_id: &str, zip: &Path) {
        let ok = self
            .checked
            .entry(host.to_string())
            .or_insert_with(|| check_drive(c, &self.drive))
            .clone();
        self.status.at = local::now_ms();
        match copy_one(c, &self.drive, &ok, title_id, zip, self.keep) {
            Outcome::Copied(_) => self.status.copied += 1,
            Outcome::Skipped(why) => {
                if self.status.skipped == 0 {
                    crate::log_info!("save backup: no copy on {host}: {why}");
                }
                self.status.skipped += 1;
                self.status.note = Some(why);
            }
            Outcome::Failed(why) => {
                crate::log_warn!("save backup: copy on {host} {}: {why}", self.drive);
                self.status.failed += 1;
                self.status.note = Some(why);
            }
        }
    }
}

fn blake3_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(h.finalize().to_hex().to_string())
}

/// The copies of `title_id` on `drive`, newest first.
pub(crate) fn list_copies(
    c: &dyn Drive,
    drive: &str,
    title_id: &str,
) -> Result<Vec<BackupFile>, String> {
    let folder = title_folder(drive, title_id)?;
    // No folder yet (nothing copied, or the drive is away): no copies.
    let Ok(entries) = c.list(&folder) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<BackupFile> = entries
        .into_iter()
        .filter(|(name, is_dir, _)| !is_dir && ours(title_id, name))
        .filter_map(|(file, _, size)| {
            let (user_id, stamp) = parse_backup_name(title_id, &file)?;
            Some(BackupFile {
                tag: backup_tag(&file).map(String::from),
                title_id: title_id.to_string(),
                user_id,
                stamp,
                size,
                file,
            })
        })
        .collect();
    out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    Ok(out)
}

/// Copies the copy `file` of `title_id` off `drive` into the local file `dest`, and checks it
/// reads back whole.
pub(crate) fn fetch(
    c: &dyn Drive,
    drive: &str,
    title_id: &str,
    file: &str,
    dest: &Path,
) -> Result<(), String> {
    let src = copy_path(drive, title_id, file)?;
    let dir = dest.parent().ok_or("no folder to download into")?;
    c.download(&src, dir)?;
    let got = dir.join(file);
    if got != dest {
        std::fs::rename(&got, dest).map_err(|e| format!("{}: {e}", got.display()))?;
    }
    crate::save_backup_server::check_zip(dest).map(|_| ())
}

/// The drive the settings name, or Err when none is chosen.
fn chosen_drive() -> Result<String, String> {
    let d = local::settings().console_drive;
    if d.trim().is_empty() {
        return Err("no PS5 drive is chosen for save backup copies".into());
    }
    Ok(d)
}

/// Copies the backup file `file` of `title_id` off the chosen drive on `host` into the local
/// file `dest` (restoring one to another user, `save_copy`).
pub(crate) fn fetch_to(host: &str, title_id: &str, file: &str, dest: &Path) -> Result<(), String> {
    fetch(&Live(host), &chosen_drive()?, title_id, file, dest)
}

/// Puts the copy `file` of `title_id` on the chosen drive back into the save folder `save_path`
/// on `host`, the way a backup in the backup folder is put back (the save there now kept first).
fn restore(
    host: &str,
    title_id: &str,
    file: &str,
    save_path: &str,
    user_id: u32,
) -> Result<(), String> {
    let drive = chosen_drive()?;
    let tmp = make_temp_dir(title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let zip: PathBuf = tmp.join(file);
        fetch(&Live(host), &drive, title_id, file, &zip)?;
        local::restore_zip_any_user(host, title_id, &zip, save_path, user_id, None)
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    if result.is_ok() {
        crate::log_info!("save backup: {file} restored from {drive} on {host} ({save_path})");
        local::mark_restored(host, save_path);
    }
    result
}

// ── HTTP ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
}

/// One drive the copies can go to.
#[derive(Debug, Serialize)]
pub struct DriveChoice {
    /// The mount, such as `/mnt/ext0`.
    pub path: String,
    /// `ext` (M.2 / extended storage) or `usb`.
    pub kind: &'static str,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// `GET /api/save-backup/console/drives?addr=` — `{drives: [{path, kind, total_bytes,
/// free_bytes}]}`, the console's M.2 and USB drives a copy of each save backup can go to (never
/// the internal storage).
pub async fn get_drives(Query(q): Query<AddrQuery>) -> Response {
    let host = q.addr.trim().to_string();
    match tokio::task::spawn_blocking(move || Live(&host).volumes()).await {
        Ok(Ok(vols)) => {
            let drives: Vec<DriveChoice> = vols
                .iter()
                .filter(|v| eligible(v))
                .map(|v| DriveChoice {
                    path: v.path.clone(),
                    kind: drive_kind(&v.path),
                    total_bytes: v.total_bytes,
                    free_bytes: v.free_bytes,
                })
                .collect();
            Json(json!({ "drives": drives })).into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
    pub title_id: String,
}

/// `GET /api/save-backup/console/list?addr=&title_id=` — `{drive, kind, backups}`: that game's
/// backup copies on the chosen PS5 drive of that console, newest first (none when no drive is
/// chosen, or it holds none).
pub async fn get_list(Query(q): Query<ListQuery>) -> Response {
    let drive = local::settings().console_drive;
    let title = q.title_id.trim().to_ascii_uppercase();
    if !plain(&title) {
        return json_error(StatusCode::BAD_REQUEST, "unexpected title id");
    }
    if drive.trim().is_empty() {
        return Json(json!({ "drive": "", "kind": "", "backups": [] })).into_response();
    }
    let host = q.addr.trim().to_string();
    let d = drive.clone();
    match tokio::task::spawn_blocking(move || list_copies(&Live(&host), &d, &title)).await {
        Ok(Ok(list)) => Json(json!({
            "drive": drive,
            "kind": drive_kind(&drive),
            "backups": list,
        }))
        .into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /api/save-backup/console/restore` `{addr, title_id, file, save_path, user_id}` — puts a
/// backup copy on the chosen PS5 drive back, with the refusals and the backup first of `restore`.
pub async fn post_restore(Json(b): Json<local::RestoreBody>) -> Response {
    let r = tokio::task::spawn_blocking(move || {
        restore(
            b.addr.trim(),
            &b.title_id.trim().to_ascii_uppercase(),
            b.file.trim(),
            b.save_path.trim(),
            b.user_id,
        )
    })
    .await;
    match r {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A console with drives and files in memory.
    #[derive(Default)]
    struct Fake {
        vols: Vec<Volume>,
        files: RefCell<BTreeMap<String, Vec<u8>>>,
        deleted: RefCell<Vec<String>>,
        uploads: RefCell<u32>,
        /// The console garbles what it is sent.
        corrupt: bool,
        /// The console can't hash.
        no_hash: bool,
    }

    fn vol(path: &str, from: &str) -> Volume {
        serde_json::from_value(json!({
            "path": path, "mount_from": from, "fs_type": "ufs",
            "total_bytes": 1u64 << 40, "free_bytes": 1u64 << 39, "writable": true,
        }))
        .unwrap()
    }

    fn with_m2() -> Fake {
        Fake {
            vols: vec![
                vol("/data", "/dev/ssd0.user"),
                vol("/user", "/dev/ssd0.user"),
                vol("/mnt/ext0", "/dev/nvme1"),
            ],
            ..Fake::default()
        }
    }

    impl Drive for Fake {
        fn volumes(&self) -> Result<Vec<Volume>, String> {
            Ok(self.vols.clone())
        }
        fn list(&self, dir: &str) -> Result<Vec<(String, bool, u64)>, String> {
            let prefix = format!("{dir}/");
            let files = self.files.borrow();
            let out: Vec<_> = files
                .iter()
                .filter_map(|(p, b)| {
                    let rest = p.strip_prefix(&prefix)?;
                    (!rest.contains('/')).then(|| (rest.to_string(), false, b.len() as u64))
                })
                .collect();
            if out.is_empty() {
                return Err("no such folder".into());
            }
            Ok(out)
        }
        fn mkdir(&self, _dir: &str) -> Result<(), String> {
            Ok(())
        }
        fn upload(&self, src: &Path, dest: &str) -> Result<(), String> {
            *self.uploads.borrow_mut() += 1;
            let mut b = std::fs::read(src).map_err(|e| e.to_string())?;
            if self.corrupt {
                b[0] ^= 0xff;
            }
            self.files.borrow_mut().insert(dest.to_string(), b);
            Ok(())
        }
        fn hash(&self, path: &str) -> Result<String, String> {
            if self.no_hash {
                return Err("unknown frame".into());
            }
            let files = self.files.borrow();
            let b = files.get(path).ok_or("no such file")?;
            Ok(blake3::hash(b).to_hex().to_string())
        }
        fn delete(&self, path: &str) -> Result<(), String> {
            self.deleted.borrow_mut().push(path.to_string());
            self.files.borrow_mut().remove(path);
            Ok(())
        }
        fn download(&self, src: &str, dest_dir: &Path) -> Result<(), String> {
            let files = self.files.borrow();
            let b = files.get(src).ok_or("no such file")?;
            let name = src.rsplit('/').next().unwrap();
            std::fs::write(dest_dir.join(name), b).map_err(|e| e.to_string())
        }
    }

    /// A real backup zip named `name` in a fresh temp folder.
    fn backup_zip(name: &str) -> (PathBuf, PathBuf) {
        let tmp = make_temp_dir("console-copy-test").unwrap();
        let src = tmp.join("src").join("PPSA01234");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("sdimg_SAVE"), b"save bytes").unwrap();
        let zip = tmp.join(name);
        ps5upload_core::save_archive::zip_folder_noted(&tmp.join("src"), "PPSA01234", &zip, "")
            .unwrap();
        (tmp, zip)
    }

    const T: &str = "PPSA01234";

    #[test]
    fn copies_go_under_the_drive_by_title_with_the_same_name() {
        assert_eq!(
            copy_path(
                "/mnt/ext0",
                T,
                "PPSA01234_user1a2b3c4d_2026-11-17_101500.zip"
            )
            .unwrap(),
            "/mnt/ext0/ps5upload/save-backups/PPSA01234/PPSA01234_user1a2b3c4d_2026-11-17_101500.zip"
        );
        assert_eq!(
            title_folder("/mnt/usb1", T).unwrap(),
            "/mnt/usb1/ps5upload/save-backups/PPSA01234"
        );
        // Never the internal storage, a folder below a drive, or a name of another shape.
        for bad in [
            "/data",
            "/user",
            "/mnt/ext",
            "/mnt/ext0/x",
            "/mnt/sandbox",
            "",
            "/mnt/usbX",
        ] {
            assert!(title_folder(bad, T).is_err(), "{bad}");
        }
        assert!(copy_path("/mnt/ext0", T, "../evil.zip").is_err());
        assert!(copy_path("/mnt/ext0", T, "PPSA01234_user1a2b3c4d_notes.zip").is_err());
        assert!(title_folder("/mnt/ext0", "../x").is_err());
    }

    #[test]
    fn only_our_names_are_ours() {
        assert!(ours(T, "PPSA01234_user1a2b3c4d_2026-11-17_101500.zip"));
        assert!(ours(T, "PPSA01234_user1a2b3c4d_2026-11-17_101500_snap.zip"));
        assert!(!ours(
            T,
            "PPSA01234_user1a2b3c4d_2026-11-17_101500.zip.part"
        ));
        assert!(!ours(T, "PPSA01234_user1a2b3c4d_mine.zip"));
        assert!(!ours(T, "PPSA01234_user1a2b3c4d_2026-11-17_101500_Old.zip"));
        assert!(!ours(T, "PPSA09999_user1a2b3c4d_2026-11-17_101500.zip"));
        assert!(!ours(T, "notes.txt"));
    }

    #[test]
    fn pruning_keeps_the_newest_of_this_save_and_kind_and_touches_nothing_else() {
        let names: Vec<String> = [
            "PPSA01234_user00000001_2026-11-01_000000.zip",
            "PPSA01234_user00000001_2026-11-02_000000.zip",
            "PPSA01234_user00000001_2026-11-03_000000.zip",
            "PPSA01234_user00000001_2026-11-01_000000_snap.zip",
            "PPSA01234_user00000002_2026-10-01_000000.zip",
            "PPSA01234_user00000001_keep-me.zip",
            "PPSA01234_user00000001_2026-10-01_000000.zip.part",
            "my notes.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            to_prune(&names, T, 1, None, 2),
            vec!["PPSA01234_user00000001_2026-11-01_000000.zip".to_string()]
        );
        assert!(to_prune(&names, T, 1, Some("snap"), 1).is_empty());
        assert!(to_prune(&names, T, 2, None, 1).is_empty());
    }

    #[test]
    fn only_m2_and_usb_drives_are_offered() {
        let mut img = vol("/mnt/ext1", "/dev/lvd2");
        img.source_image = "/data/x.exfat".into();
        let mut ro = vol("/mnt/usb0", "/dev/da0");
        ro.writable = false;
        let mut ph = vol("/mnt/usb1", "tmpfs");
        ph.is_placeholder = true;
        let vols = [
            vol("/data", "/dev/ssd0.user"),
            vol("/user", "/dev/ssd0.user"),
            vol("/mnt/ext0", "/dev/nvme1"),
            vol("/mnt/usb2", "/dev/da1"),
            vol("/mnt/sandbox/PPSA01234_000", "/dev/lvd3"),
            img,
            ro,
            ph,
        ];
        let ok: Vec<&str> = vols
            .iter()
            .filter(|v| eligible(v))
            .map(|v| v.path.as_str())
            .collect();
        assert_eq!(ok, vec!["/mnt/ext0", "/mnt/usb2"]);
        assert_eq!(drive_kind("/mnt/ext0"), "ext");
        assert_eq!(drive_kind("/mnt/usb2"), "usb");
    }

    #[test]
    fn a_missing_drive_is_skipped_and_nothing_is_sent() {
        let fake = Fake {
            vols: vec![vol("/data", "/dev/ssd0.user")],
            ..Fake::default()
        };
        let ok = check_drive(&fake, "/mnt/ext0");
        assert!(
            ok.as_ref().unwrap_err().contains("not plugged in"),
            "{ok:?}"
        );
        let (tmp, zip) = backup_zip("PPSA01234_user00000001_2026-11-17_101500.zip");
        match copy_one(&fake, "/mnt/ext0", &ok, T, &zip, 5) {
            Outcome::Skipped(why) => assert!(why.contains("/mnt/ext0"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(*fake.uploads.borrow(), 0);
        let mut ro = with_m2();
        ro.vols[2].writable = false;
        assert!(check_drive(&ro, "/mnt/ext0")
            .unwrap_err()
            .contains("read-only"));
        assert!(check_drive(&with_m2(), "/data").is_err());
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn a_copy_is_checked_and_older_ones_of_ours_are_pruned() {
        let fake = with_m2();
        let folder = title_folder("/mnt/ext0", T).unwrap();
        for old in [
            "PPSA01234_user00000001_2026-11-01_000000.zip",
            "PPSA01234_user00000001_2026-11-02_000000.zip",
            "PPSA01234_user00000001_old-by-hand.zip",
        ] {
            fake.files
                .borrow_mut()
                .insert(format!("{folder}/{old}"), b"old".to_vec());
        }
        let (tmp, zip) = backup_zip("PPSA01234_user00000001_2026-11-17_101500.zip");
        let ok = check_drive(&fake, "/mnt/ext0");
        assert!(ok.is_ok());
        let got = copy_one(&fake, "/mnt/ext0", &ok, T, &zip, 2);
        let dest = format!("{folder}/PPSA01234_user00000001_2026-11-17_101500.zip");
        assert_eq!(got, Outcome::Copied(dest.clone()));
        assert_eq!(
            fake.files.borrow().get(&dest).unwrap(),
            &std::fs::read(&zip).unwrap()
        );
        assert_eq!(
            *fake.deleted.borrow(),
            vec![format!(
                "{folder}/PPSA01234_user00000001_2026-11-01_000000.zip"
            )]
        );
        assert!(fake
            .files
            .borrow()
            .contains_key(&format!("{folder}/PPSA01234_user00000001_old-by-hand.zip")));
        // It lists, newest first, only ours; and it comes back whole.
        let listed = list_copies(&fake, "/mnt/ext0", T).unwrap();
        let files: Vec<&str> = listed.iter().map(|b| b.file.as_str()).collect();
        assert_eq!(
            files,
            vec![
                "PPSA01234_user00000001_2026-11-17_101500.zip",
                "PPSA01234_user00000001_2026-11-02_000000.zip"
            ]
        );
        let back = tmp.join("back").join("restore.zip");
        std::fs::create_dir_all(back.parent().unwrap()).unwrap();
        fetch(&fake, "/mnt/ext0", T, &listed[0].file, &back).unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), std::fs::read(&zip).unwrap());
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn a_copy_that_does_not_match_is_removed_and_nothing_is_pruned() {
        let fake = Fake {
            corrupt: true,
            ..with_m2()
        };
        let folder = title_folder("/mnt/ext0", T).unwrap();
        let old = format!("{folder}/PPSA01234_user00000001_2026-11-01_000000.zip");
        fake.files.borrow_mut().insert(old.clone(), b"old".to_vec());
        let (tmp, zip) = backup_zip("PPSA01234_user00000001_2026-11-17_101500.zip");
        let got = copy_one(&fake, "/mnt/ext0", &Ok(()), T, &zip, 1);
        assert!(
            matches!(&got, Outcome::Failed(why) if why.contains("does not match")),
            "{got:?}"
        );
        let files = fake.files.borrow();
        assert_eq!(files.keys().collect::<Vec<_>>(), vec![&old]);
        drop(files);
        // A console that can't hash keeps nothing it can't vouch for either.
        let fake = Fake {
            no_hash: true,
            ..with_m2()
        };
        let got = copy_one(&fake, "/mnt/ext0", &Ok(()), T, &zip, 1);
        assert!(matches!(got, Outcome::Failed(_)));
        assert!(fake.files.borrow().is_empty());
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn a_broken_local_backup_is_never_sent() {
        let fake = with_m2();
        let tmp = make_temp_dir("console-copy-bad").unwrap();
        let zip = tmp.join("PPSA01234_user00000001_2026-11-17_101500.zip");
        std::fs::write(&zip, b"not a zip").unwrap();
        assert!(matches!(
            copy_one(&fake, "/mnt/ext0", &Ok(()), T, &zip, 5),
            Outcome::Failed(_)
        ));
        assert_eq!(*fake.uploads.borrow(), 0);
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn a_drive_with_no_copies_lists_none() {
        assert!(list_copies(&with_m2(), "/mnt/ext0", T).unwrap().is_empty());
        assert!(list_copies(&with_m2(), "/data", T).is_err());
    }
}
