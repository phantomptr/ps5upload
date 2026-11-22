//! Folders on the PS5 backed up with the saves (#899): the saves of a port or an emulator kept in
//! its own folder, such as a game's `My Games` folder in a prefix under `/data`. Each chosen folder
//! is zipped whole and kept like a save's backup, with the same versions to keep, in
//! `Folders/<name>/folder_<stamp>.zip` in each place (the network share, the backup folder here, a
//! PS5 drive; the same in both layouts). `<name>` is the folder's path made safe, with a short
//! hash of the exact path so two folders never share one. The zip's comment names the folder, so
//! a restore puts it back where it came from, after keeping what is there first (a
//! `folder_<stamp>_beforerestore.zip` beside it).
//!
//! Only folders the helper lets a client read and write, below `/data`, `/user`, `/mnt/ext<N>` or
//! `/mnt/usb<N>`; never the helper's own `/data/ps5upload` (its keys and records), the console's
//! save folders (backed up as saves, and put back through the console's own save handling), the
//! installed games and the copies of these backups.

use std::path::Path;
use std::sync::Arc;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::save_archive::{
    cleanup_temp_dir, make_temp_dir, zip_comment, zip_folder_noted,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::remote::{PutProgress, RemoteFs};
use crate::save_backup::{self as local, json_error};
use crate::save_backup_console::Drive;
use crate::save_layout as layout;

/// The folder of every folder's backups, in each place.
pub const DIR: &str = "Folders";
/// The tag of the copy of a folder kept before a restore replaced it.
pub const BEFORE_RESTORE_TAG: &str = "beforerestore";
/// Most folders a setting may list.
pub const MAX_FOLDERS: usize = 32;
/// Most characters of a folder's path kept in its backups' folder name.
const NAME_CHARS: usize = 100;
/// How far down a folder is looked at to tell whether it changed, and how many entries.
const WALK_DEPTH: u32 = 24;
const WALK_ENTRIES: usize = 50_000;

// ── which folders (pure, tested) ────────────────────────────────────────

/// `path` made plain (no trailing `/`), when it is a console folder the backups may take; else why
/// not, in words.
pub fn folder_path_ok(path: &str) -> Result<String, String> {
    let p = path.trim();
    let p = if p.len() > 1 {
        p.trim_end_matches('/')
    } else {
        p
    };
    if !p.starts_with('/') {
        return Err("a folder on the PS5 starts with /".into());
    }
    if p.contains("//")
        || p.contains('\\')
        || p.chars().any(char::is_control)
        || p.split('/')
            .skip(1)
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format!("{p} is not a plain folder path"));
    }
    let parts: Vec<&str> = p.split('/').skip(1).collect();
    let lower = p.to_ascii_lowercase();
    let root_ok = match parts.first().copied() {
        Some("data") | Some("user") => true,
        Some("mnt") => parts
            .get(1)
            .is_some_and(|d| crate::save_backup_console::drive_path_ok(&format!("/mnt/{d}"))),
        _ => false,
    };
    if !root_ok {
        return Err(
            "only folders under /data, /user or a PS5 M.2 or USB drive (/mnt/ext0, /mnt/usb0) can be backed up"
                .into(),
        );
    }
    // Something below the drive itself.
    let depth = if parts[0] == "mnt" { 3 } else { 2 };
    if parts.len() < depth {
        return Err(format!("{p} is a whole drive: choose a folder inside it"));
    }
    let under = |dir: &str| lower == dir || lower.starts_with(&format!("{dir}/"));
    // The helper's own folder (its keys, records and saved shares); /data is /user/data.
    // A folder that holds it counts too: /user/data is /data, a whole drive, and restoring it would
    // empty the helper's folder with the rest.
    let holds = |dir: &str| dir.starts_with(&format!("{lower}/"));
    if under("/data/ps5upload")
        || under("/user/data/ps5upload")
        || holds("/data/ps5upload")
        || holds("/user/data/ps5upload")
    {
        return Err("PS5Upload's own folder on the PS5 can't be backed up".into());
    }
    // The installed games, and the console's own folders under /user.
    let user_rest = if parts[0] == "user" {
        Some(&parts[1..])
    } else if parts[0] == "mnt" && parts.get(2) == Some(&"user") {
        Some(&parts[3..])
    } else {
        None
    };
    if let Some(rest) = user_rest {
        let top = rest.first().copied().unwrap_or_default();
        if matches!(
            top,
            "app" | "patch" | "addcont" | "appmeta" | "av_contents" | "system" | "license"
        ) || rest.is_empty()
        {
            return Err(format!(
                "{p} is the console's own folder: choose a folder of a game or app's saves"
            ));
        }
        if top == "home" && rest.len() < 3 {
            return Err(format!(
                "{p} holds every user's saves: choose a folder inside it"
            ));
        }
    }
    // Saves are backed up as saves (and put back through the console's save handling).
    if parts
        .iter()
        .any(|c| matches!(*c, "savedata" | "savedata_prospero"))
    {
        return Err("save data folders are backed up as saves already".into());
    }
    // The copies of the backups on a PS5 drive.
    // Any case: a USB drive is exFAT, where PS5Upload and ps5upload are the same folder.
    if parts[0] == "mnt"
        && parts
            .get(2)
            .is_some_and(|d| d.eq_ignore_ascii_case("ps5upload"))
    {
        return Err("the backups' own folder can't be backed up".into());
    }
    Ok(p.to_string())
}

/// The setting's list made plain, one of each, refusing any folder `folder_path_ok` refuses.
pub fn folders_ok(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for f in list {
        if f.trim().is_empty() {
            continue;
        }
        let p = folder_path_ok(f)?;
        // Exactly: /data is case-sensitive, and /data/Saves and /data/saves are two folders.
        if !out.contains(&p) {
            out.push(p);
        }
    }
    if out.len() > MAX_FOLDERS {
        return Err(format!("at most {MAX_FOLDERS} folders"));
    }
    Ok(out)
}

/// A short, stable hash (FNV-1a) of `s`.
fn fnv(s: &[u8]) -> u64 {
    s.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The folder a console folder's backups go in, inside `Folders`: its path made safe
/// (`data_prospero-win_…_My Games_Grim Dawn`), the end kept when it is long, and `-<8 hex>` of
/// the exact path, so two folders never share one.
pub fn dir_name(path: &str) -> String {
    let joined: Vec<String> = path
        .split('/')
        .filter(|c| !c.is_empty())
        .map(|c| layout::safe_component(c, NAME_CHARS))
        .filter(|c| !c.is_empty())
        .collect();
    let joined = joined.join("_");
    let chars: Vec<char> = joined.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(NAME_CHARS)..]
        .iter()
        .collect();
    let tail = tail.trim_matches(|c: char| c == '.' || c == '_' || c.is_whitespace());
    format!("{tail}-{:08x}", fnv(path.as_bytes()) as u32)
}

/// `Folders/<dir_name>`: where a console folder's backups went before they were kept per PS5, of
/// every PS5 together (still listed and restored from; never written to by a backup run).
pub fn folder_rel(path: &str) -> String {
    format!("{DIR}/{}", dir_name(path))
}

/// The folder of one PS5's folder backups inside `Folders`: `PS5-<its id>`, the id the PS5 gives
/// on the network (the same whatever its address, and on every computer that backs it up). None
/// for an id with nothing usable in it.
pub fn console_dir(id: &str) -> Option<String> {
    let clean: String = id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(32)
        .collect::<String>()
        .to_ascii_uppercase();
    (!clean.is_empty()).then(|| format!("PS5-{clean}"))
}

/// Why a folder backup can't be made or found for a PS5 whose id is not known.
pub const UNKNOWN_CONSOLE: &str =
    "couldn't tell which PS5 this is (it did not answer on the network): folder backups are kept per PS5, so this one waits for the next backup";

/// `Folders/PS5-<id>/<dir_name>`: where the backups of the console folder `path` made on the PS5
/// `console` (its id) go, so two PS5s with the same folder never share, prune or restore each
/// other's.
pub fn folder_rel_on(path: &str, console: &str) -> Result<String, String> {
    let c = console_dir(console).ok_or_else(|| UNKNOWN_CONSOLE.to_string())?;
    Ok(format!("{DIR}/{c}/{}", dir_name(path)))
}

/// The name of a backup of a folder made now: `folder_<YYYY-MM-DD_HHMMSS>[_<tag>].zip`.
pub fn backup_name(tag: Option<&str>) -> String {
    let st = local::stamp(local::now_ms());
    match tag {
        Some(t) => format!("folder_{st}_{t}.zip"),
        None => format!("folder_{st}.zip"),
    }
}

/// Whether `name` is a folder backup this app made (the only files it ever deletes there).
pub fn ours(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("folder_")
        .and_then(|r| r.strip_suffix(".zip"))
    else {
        return false;
    };
    let b = rest.as_bytes();
    let shape = b.len() >= 17
        && b[..17].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'_',
            _ => c.is_ascii_digit(),
        });
    shape
        && match &rest[17..] {
            "" => true,
            t => t
                .strip_prefix('_')
                .is_some_and(|t| !t.is_empty() && t.bytes().all(|c| c.is_ascii_lowercase())),
        }
}

/// The tag of a folder backup's name (`beforerestore`), None for one made by a backup run.
pub fn tag_of(name: &str) -> Option<&str> {
    ps5upload_core::save_archive::backup_tag(name)
}

/// Which of `names` (one folder's files) to delete so `keep` of the backups tagged `tag` remain:
/// the oldest, and only names this app gives them.
pub fn to_prune(names: &[String], tag: Option<&str>, keep: u32) -> Vec<String> {
    let mut mine: Vec<&String> = names
        .iter()
        .filter(|n| ours(n) && tag_of(n) == tag)
        .collect();
    mine.sort();
    let drop = mine.len().saturating_sub(keep.max(1) as usize);
    mine.into_iter().take(drop).cloned().collect()
}

/// The run record's id of a folder on `host` (`<host>|folder:<path>`), also its key in the
/// records of what each place holds.
pub fn item_path(path: &str) -> String {
    format!("folder:{path}")
}

/// A run's stand-in for a folder: `path` `folder:<path>`, `kind` `folder`, `mtime` what the folder
/// looked like (`signature`).
pub fn entry(path: &str, signature: i64) -> ps5upload_core::saves::SaveEntry {
    ps5upload_core::saves::SaveEntry {
        title_id: String::new(),
        user_id: 0,
        path: item_path(path),
        size: 0,
        mtime: signature,
        kind: "folder".into(),
    }
}

/// What a folder holds, as one number: changes when a file is added, removed, resized or
/// written. `entries`: `(path in the folder, size, mtime)`.
pub fn signature(entries: &[(String, u64, i64)]) -> i64 {
    let mut sorted: Vec<&(String, u64, i64)> = entries.iter().collect();
    sorted.sort();
    let mut text = String::new();
    for (p, s, m) in sorted {
        text.push_str(&format!("{p}\u{0}{s}\u{0}{m}\n"));
    }
    // Never 0: a record with no time is "never backed up". The top bit goes first: read signed,
    // half of all hashes are negative and `.max(1)` made each of them 1, so a change was missed
    // whenever the old and the new signature both came out negative.
    ((fnv(text.as_bytes()) >> 1) as i64).max(1)
}

/// The zip comment of a folder's backup: which folder, from which PS5.
pub fn note(path: &str, console: Option<&str>) -> String {
    let mut n = json!({ "ps5upload": "folder-backup", "path": path });
    if let Some(c) = console {
        n["console"] = json!(c);
    }
    n.to_string()
}

/// The folder a backup zip is of, from its comment; None for a zip that does not say.
pub fn noted_path(zip: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&zip_comment(zip).ok()?).ok()?;
    if v.get("ps5upload").and_then(Value::as_str) != Some("folder-backup") {
        return None;
    }
    v.get("path").and_then(Value::as_str).map(str::to_string)
}

/// The PS5 a folder backup zip was made on, from its comment; None when it does not say.
pub fn noted_console(zip: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&zip_comment(zip).ok()?).ok()?;
    if v.get("ps5upload").and_then(Value::as_str) != Some("folder-backup") {
        return None;
    }
    v.get("console")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|c| !c.is_empty())
}

// ── zipping, checking ───────────────────────────────────────────────────

pub(crate) fn sha256_file(path: &Path) -> Result<(u64, String), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let got = std::io::Read::read(&mut f, &mut buf).map_err(|e| format!("read: {e}"))?;
        if got == 0 {
            break;
        }
        h.update(&buf[..got]);
        n += got as u64;
    }
    Ok((n, h.finalize().iter().map(|b| format!("{b:02x}")).collect()))
}

pub(crate) fn blake3_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(h.finalize().to_hex().to_string())
}

/// Zips the folder `tmp/<leaf>` (a copy of the console folder `path`) into `zip`, noted.
pub(crate) fn zip_copy(
    tmp: &Path,
    leaf: &str,
    zip: &Path,
    path: &str,
    console: Option<&str>,
) -> Result<(), String> {
    if !tmp.join(leaf).is_dir() {
        std::fs::create_dir_all(tmp.join(leaf)).map_err(|e| e.to_string())?;
    }
    zip_folder_noted(tmp, leaf, zip, &note(path, console)).map_err(|e| format!("zip: {e:#}"))
}

// ── the places ──────────────────────────────────────────────────────────

/// Writes the folder backup `zip` to `<root>/<rel>/<name>` on the server, reads it back and checks
/// it (removed again when it does not), writes its `.sha256`, then removes the oldest of that
/// folder's backups of the same kind past `keep`. The backup's path on the server.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn store_on_server(
    fs: &dyn RemoteFs,
    zip: &Path,
    root: &str,
    rel: &str,
    name: &str,
    keep: u32,
    scratch: &Path,
    progress: &PutProgress,
    checking: &dyn Fn(),
) -> Result<String, String> {
    if !ours(name) || !rel.starts_with(&format!("{DIR}/")) || rel.contains("..") {
        return Err("unexpected backup name".into());
    }
    let (size, sum) = sha256_file(zip)?;
    let entries = crate::save_backup_server::check_zip(zip)
        .map_err(|e| format!("the backup made here: {e}"))?;
    let dir = layout::join(root, rel);
    crate::download_relay::make_dirs(fs, &dir)
        .await
        .map_err(|e| format!("make {dir} on the server: {e}"))?;
    let target = format!("{dir}/{name}");
    crate::download_relay::place(fs, zip, &target, progress)
        .await
        .map_err(|e| format!("write {target}: {e}"))?;
    checking();
    match crate::save_backup_server::verify_on_server(fs, &target, size, &sum, scratch).await {
        Ok(n) if n == entries => {}
        Ok(n) => {
            let _ = fs.remove_file(&target).await;
            return Err(format!(
                "verification failed: {n} files read back, {entries} were sent"
            ));
        }
        Err(e) => {
            let _ = fs.remove_file(&target).await;
            return Err(format!("verification failed: {e}"));
        }
    }
    let sum_file = scratch.with_extension("sha256");
    if std::fs::write(&sum_file, format!("{sum}  {name}\n")).is_ok() {
        let _ = fs
            .put_file(&format!("{target}.sha256"), &sum_file, &quiet())
            .await;
        let _ = std::fs::remove_file(&sum_file);
    }
    let names = server_names(fs, &dir).await;
    for old in to_prune(&names, tag_of(name), keep) {
        let p = format!("{dir}/{old}");
        if p != target {
            let _ = fs.remove_file(&p).await;
            let _ = fs.remove_file(&format!("{p}.sha256")).await;
        }
    }
    Ok(target)
}

fn quiet() -> PutProgress {
    PutProgress {
        add: Box::new(|_| {}),
        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

/// The file names in the server folder `dir` (none when it can't be read).
async fn server_names(fs: &dyn RemoteFs, dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let Ok(page) = fs.list(dir, cursor).await else {
            return out;
        };
        out.extend(
            page.entries
                .into_iter()
                .filter(|e| !e.is_dir)
                .map(|e| e.name),
        );
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
}

/// Copies the folder backup `zip` to `<root>/<rel>/<name>` on a PS5 drive, checks the copy there
/// (BLAKE3, removed when it differs), then prunes the folder's backups past `keep`.
pub(crate) fn store_on_drive(
    c: &dyn Drive,
    root: &str,
    rel: &str,
    name: &str,
    zip: &Path,
    keep: u32,
) -> Result<String, String> {
    if !ours(name) || !rel.starts_with(&format!("{DIR}/")) || rel.contains("..") {
        return Err("unexpected backup name".into());
    }
    crate::save_backup_server::check_zip(zip).map_err(|e| format!("{name}: {e}"))?;
    let want = blake3_file(zip)?;
    let dir = layout::join(root, rel);
    let dest = format!("{dir}/{name}");
    if let Err(e) = c.mkdir(&dir).and_then(|()| c.upload(zip, &dest)) {
        let _ = c.delete(&dest);
        return Err(format!("{name}: {e}"));
    }
    match c.hash(&dest) {
        Ok(got) if got == want => {}
        Ok(_) => {
            let _ = c.delete(&dest);
            return Err(format!("{name}: the copy on the PS5 does not match"));
        }
        Err(e) => {
            let _ = c.delete(&dest);
            return Err(format!("{name}: could not check the copy on the PS5: {e}"));
        }
    }
    let names: Vec<String> = c
        .list(&dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, d, _)| !d)
        .map(|(n, _, _)| n)
        .collect();
    for old in to_prune(&names, tag_of(name), keep) {
        let p = format!("{dir}/{old}");
        if p != dest {
            let _ = c.delete(&p);
        }
    }
    Ok(dest)
}

/// Copies the folder backup `zip` into the local folder `dest`'s place (`<dir>/<rel>/<name>`),
/// under a temporary name until it is whole and checked, then prunes past `keep`.
pub(crate) fn store_here(
    dir: &str,
    rel: &str,
    name: &str,
    zip: &Path,
    keep: u32,
) -> Result<String, String> {
    if !ours(name) || !rel.starts_with(&format!("{DIR}/")) || rel.contains("..") {
        return Err("unexpected backup name".into());
    }
    let folder = local::local_path(dir, rel);
    std::fs::create_dir_all(&folder).map_err(|e| format!("create {}: {e}", folder.display()))?;
    let dest = folder.join(name);
    let tmp = dest.with_extension("zip.ps5upload-tmp");
    if let Err(e) = std::fs::copy(zip, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {}: {e}", tmp.display()));
    }
    if let Err(e) = crate::save_backup_server::check_zip(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("the copy here does not check out: {e}"));
    }
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("write {}: {e}", dest.display())
    })?;
    let names: Vec<String> = std::fs::read_dir(&folder)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    for old in to_prune(&names, tag_of(name), keep) {
        if old != name {
            let _ = std::fs::remove_file(folder.join(old));
        }
    }
    Ok(dest.to_string_lossy().into_owned())
}

// ── the console ─────────────────────────────────────────────────────────

/// Every file under the console folder `path`, `(path in it, size, mtime)`, as far as
/// `WALK_DEPTH` folders down; Err when the folder can't be read (not there, or the console is
/// away).
pub(crate) fn walk(
    list: &dyn Fn(&str) -> Result<Vec<Value>, String>,
    path: &str,
) -> Result<Vec<(String, u64, i64)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(path.to_string(), String::new(), 0u32)];
    while let Some((dir, rel, depth)) = stack.pop() {
        for e in list(&dir)? {
            let name = e.get("name").and_then(Value::as_str).unwrap_or_default();
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }
            let here = format!("{rel}{name}");
            if e.get("kind").and_then(Value::as_str) == Some("dir") {
                if depth < WALK_DEPTH {
                    stack.push((format!("{dir}/{name}"), format!("{here}/"), depth + 1));
                }
            } else {
                out.push((
                    here,
                    e.get("size").and_then(Value::as_u64).unwrap_or(0),
                    e.get("mtime").and_then(Value::as_i64).unwrap_or(0),
                ));
                if out.len() >= WALK_ENTRIES {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

/// The name a console folder's copy gets on this computer (what a download writes).
pub(crate) fn leaf_of(path: &str) -> Result<String, String> {
    let leaf = path.rsplit('/').next().unwrap_or_default();
    ps5upload_core::host_names::escape_name(leaf)
}

// ── restore ─────────────────────────────────────────────────────────────

/// Where a folder backup is kept: the network share, the backup folder here, a PS5 drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum From {
    Share,
    Local,
    Drive,
}

/// One folder backup in a place.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FolderBackup {
    pub file: String,
    /// `YYYY-MM-DD_HHMMSS` (UTC).
    pub stamp: String,
    pub size: u64,
    /// `beforerestore` for the copy kept before a restore.
    pub tag: Option<String>,
    /// Made before folder backups were kept per PS5: it may be of any PS5 that has this folder.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
}

fn backups_of(files: Vec<(String, u64)>) -> Vec<FolderBackup> {
    let mut out: Vec<FolderBackup> = files
        .into_iter()
        .filter(|(n, _)| ours(n))
        .map(|(n, size)| FolderBackup {
            stamp: n
                .strip_prefix("folder_")
                .map(|r| r.chars().take(17).collect())
                .unwrap_or_default(),
            tag: tag_of(&n).map(str::to_string),
            size,
            file: n,
            legacy: false,
        })
        .collect();
    out.sort_by(|a, b| b.file.cmp(&a.file));
    out
}

/// A place's folder backups, behind what each of its kinds needs.
trait Place {
    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>, String>;
    /// Copies the backup `file` of `rel` into the local file `dest`.
    fn fetch(&self, rel: &str, file: &str, dest: &Path) -> Result<(), String>;
    /// Keeps the zip `zip` (named `name`) there, as a before-restore copy.
    fn keep(&self, rel: &str, name: &str, zip: &Path) -> Result<String, String>;
}

struct ShareSide {
    target: String,
}

impl ShareSide {
    /// The server and its backup folder, run on the engine's runtime.
    fn run<T>(
        &self,
        f: impl FnOnce(
            Arc<dyn RemoteFs>,
            String,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>>>>,
    ) -> Result<T, String> {
        let target = self.target.clone();
        let r = tokio::runtime::Handle::current().block_on(async {
            let (fs, root) = crate::save_backup_server::open_target(&target).await?;
            f(fs, root).await
        });
        if r.is_err() {
            crate::save_backup_server::drop_session(&target);
        }
        r
    }
}

impl Place for ShareSide {
    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>, String> {
        let rel = rel.to_string();
        self.run(move |fs, root| {
            Box::pin(async move {
                let dir = layout::join(&root, &rel);
                let mut out = Vec::new();
                let mut cursor = None;
                loop {
                    let page = match fs.list(&dir, cursor).await {
                        Ok(p) => p,
                        Err(crate::remote::RemoteError::NotFound(_)) => return Ok(out),
                        Err(e) => return Err(e.to_string()),
                    };
                    out.extend(
                        page.entries
                            .into_iter()
                            .filter(|e| !e.is_dir)
                            .map(|e| (e.name, e.size)),
                    );
                    match page.next_cursor {
                        Some(c) => cursor = Some(c),
                        None => return Ok(out),
                    }
                }
            })
        })
    }

    fn fetch(&self, rel: &str, file: &str, dest: &Path) -> Result<(), String> {
        let (rel, file, dest) = (rel.to_string(), file.to_string(), dest.to_path_buf());
        self.run(move |fs, root| {
            Box::pin(async move {
                let path = format!("{}/{file}", layout::join(&root, &rel));
                let (_, sum) =
                    crate::save_backup_server::fetch_file(fs.as_ref(), &path, &dest).await?;
                let sum_path = dest.with_extension("sha256");
                if crate::save_backup_server::fetch_file(
                    fs.as_ref(),
                    &format!("{path}.sha256"),
                    &sum_path,
                )
                .await
                .is_ok()
                {
                    let want = std::fs::read_to_string(&sum_path).unwrap_or_default();
                    let _ = std::fs::remove_file(&sum_path);
                    let want = want.split_whitespace().next().unwrap_or_default();
                    if want.len() == 64 && !want.eq_ignore_ascii_case(&sum) {
                        return Err(format!(
                            "{file} on the server does not match the checksum written with it"
                        ));
                    }
                }
                Ok(())
            })
        })
    }

    fn keep(&self, rel: &str, name: &str, zip: &Path) -> Result<String, String> {
        let (rel, name, zip) = (rel.to_string(), name.to_string(), zip.to_path_buf());
        let keep = crate::save_backup_server::settings().keep;
        self.run(move |fs, root| {
            Box::pin(async move {
                let scratch = zip.with_extension("readback");
                store_on_server(
                    fs.as_ref(),
                    &zip,
                    &root,
                    &rel,
                    &name,
                    keep,
                    &scratch,
                    &quiet(),
                    &|| {},
                )
                .await
            })
        })
    }
}

struct LocalSide {
    dir: String,
}

impl Place for LocalSide {
    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>, String> {
        let folder = local::local_path(&self.dir, rel);
        match std::fs::read_dir(&folder) {
            Ok(rd) => Ok(rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let md = e.metadata().ok()?;
                    md.is_file()
                        .then(|| e.file_name().into_string().ok().map(|n| (n, md.len())))
                        .flatten()
                })
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{}: {e}", folder.display())),
        }
    }

    fn fetch(&self, rel: &str, file: &str, dest: &Path) -> Result<(), String> {
        let src = local::local_path(&self.dir, rel).join(file);
        std::fs::copy(&src, dest)
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", src.display()))
    }

    fn keep(&self, rel: &str, name: &str, zip: &Path) -> Result<String, String> {
        store_here(&self.dir, rel, name, zip, local::settings().keep)
    }
}

struct DriveSide<'a> {
    c: &'a dyn Drive,
    root: String,
}

impl Place for DriveSide<'_> {
    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>, String> {
        Ok(self
            .c
            .list(&layout::join(&self.root, rel))
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, d, _)| !d)
            .map(|(n, _, s)| (n, s))
            .collect())
    }

    fn fetch(&self, rel: &str, file: &str, dest: &Path) -> Result<(), String> {
        let dir = dest.parent().ok_or("no folder to download into")?;
        self.c
            .download(&format!("{}/{file}", layout::join(&self.root, rel)), dir)?;
        let got = dir.join(file);
        if got != dest {
            std::fs::rename(&got, dest).map_err(|e| format!("{}: {e}", got.display()))?;
        }
        Ok(())
    }

    fn keep(&self, rel: &str, name: &str, zip: &Path) -> Result<String, String> {
        let _ = (rel, name, zip);
        // A copy kept on the drive itself would be lost with the drive: the folder here or the
        // share keeps it instead (see `restore`).
        Err("not kept on the drive".into())
    }
}

/// What a restore needs from the console. `Live` asks the engine's own routes.
pub(crate) trait Console {
    /// The entries of the folder `path` (Err when it is not there).
    fn entries(&self, path: &str) -> Result<Vec<Value>, String>;
    /// Copies the folder `path` into the local folder `dest` (as `dest/<escaped leaf>`).
    fn download(&self, path: &str, dest: &Path) -> Result<(), String>;
    fn delete(&self, path: &str) -> Result<(), String>;
    fn mkdir(&self, path: &str) -> Result<(), String>;
    /// Uploads the files of the local folder `src` into the console folder `dest`.
    fn upload(&self, src: &Path, dest: &str) -> Result<(), String>;
}

pub(crate) struct LiveConsole<'a>(pub &'a str);

impl Console for LiveConsole<'_> {
    fn entries(&self, path: &str) -> Result<Vec<Value>, String> {
        local::folder_entries(self.0, path)
    }
    fn download(&self, path: &str, dest: &Path) -> Result<(), String> {
        local::download(self.0, path, dest, "folder")
    }
    fn delete(&self, path: &str) -> Result<(), String> {
        local::post(
            "/api/ps5/fs/delete",
            json!({ "addr": self.0, "path": path }),
        )
        .map(|_| ())
    }
    fn mkdir(&self, path: &str) -> Result<(), String> {
        local::post("/api/ps5/fs/mkdir", json!({ "addr": self.0, "path": path })).map(|_| ())
    }
    fn upload(&self, src: &Path, dest: &str) -> Result<(), String> {
        local::upload_kept(self.0, src, dest)
    }
}

/// Empties the console folder `path` (keeping the folder) and uploads the files of `src` into it.
/// `there`: the folder is there (and was kept first); else nothing in it is removed.
fn put_back(c: &dyn Console, path: &str, src: &Path, there: bool) -> Result<(), String> {
    let names = if there { c.entries(path)? } else { Vec::new() };
    for e in names {
        let Some(name) = e.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        c.delete(&format!("{path}/{name}"))?;
    }
    let _ = c.mkdir(path);
    c.upload(src, path)
}

/// Puts the folder backup `file` (in `place`, of the console folder `path`) back into that
/// folder on the console: the zip is checked, what is in the folder now is kept first (in
/// `keep_in`, as a `beforerestore` backup), then the folder is emptied and filled from the
/// backup. When filling fails, what was there is put back.
fn restore_with(
    c: &dyn Console,
    place: &dyn Place,
    keep_in: &dyn Place,
    path: &str,
    file: &str,
    console_id: Option<&str>,
) -> Result<(), String> {
    let path = folder_path_ok(path)?;
    if !ours(file) {
        return Err("unexpected backup name".into());
    }
    // This PS5's own backups first; then one made before backups were kept per PS5 (of whichever
    // PS5 had the folder: the person picked it). Another PS5's own backups are never looked at.
    let own = console_id.map(|id| folder_rel_on(&path, id)).transpose()?;
    let old = folder_rel(&path);
    let holds = |rel: &str| place.list(rel).map(|l| l.iter().any(|(n, _)| n == file));
    let (rel, legacy) = match &own {
        Some(own) if holds(own)? => (own.clone(), false),
        _ if holds(&old)? => (old.clone(), true),
        Some(_) => return Err(format!("{file} is not one of this PS5's backups of {path}")),
        None => return Err(UNKNOWN_CONSOLE.into()),
    };
    // What is there now is kept with this PS5's own backups (or with the old ones when this PS5
    // can't be told).
    let keep_rel = own.clone().unwrap_or_else(|| old.clone());
    let tmp = make_temp_dir("folder-restore")?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let zip = tmp.join(file);
        place.fetch(&rel, file, &zip)?;
        crate::save_backup_server::check_zip(&zip)
            .map_err(|e| format!("{file} is damaged: {e}"))?;
        match noted_path(&zip) {
            Some(p) if p == path => {}
            Some(p) => return Err(format!("{file} is a backup of {p}, not {path}")),
            None => return Err(format!("{file} does not say which folder it is of")),
        }
        if let (false, Some(mine), Some(made_on)) = (legacy, console_id, noted_console(&zip)) {
            if !made_on.eq_ignore_ascii_case(mine) {
                return Err(format!("{file} was made on another PS5 ({made_on})"));
            }
        }
        let unpacked = tmp.join("unpacked");
        std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
        let inner = zip_root(&zip)?;
        ps5upload_core::save_archive::unzip_strict(&zip, &unpacked, &inner)
            .map_err(|e| format!("unzip: {e:#}"))?;
        let leaf = leaf_of(&path)?;
        // What is there now is kept first.
        let current = tmp.join("current");
        std::fs::create_dir_all(&current).map_err(|e| e.to_string())?;
        // Only "not there" counts as not there: a timeout or a busy helper read as that skipped
        // keeping what is there, and the folder was then emptied all the same.
        let there = match c.entries(&path) {
            Ok(_) => true,
            Err(e) if ps5upload_core::fs_ops::is_not_found(&e) => false,
            Err(e) => return Err(format!("could not look at {path} now: {e}")),
        };
        if there {
            c.download(&path, &current)
                .map_err(|e| format!("could not copy what is in {path} now: {e}"))?;
            let kept = tmp.join(backup_name(Some(BEFORE_RESTORE_TAG)));
            zip_copy(&current, &leaf, &kept, &path, console_id)?;
            let name = kept
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            keep_in
                .keep(&keep_rel, &name, &kept)
                .map_err(|e| format!("could not keep what is in {path} now first: {e}"))?;
        }
        match put_back(c, &path, &unpacked.join(&inner), there) {
            Ok(()) => Ok(()),
            Err(e) => {
                if there {
                    let _ = put_back(c, &path, &current.join(&leaf), true);
                }
                Err(format!("putting {file} back failed: {e}"))
            }
        }
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// The one folder at the top of a backup zip.
fn zip_root(zip: &Path) -> Result<String, String> {
    let f = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("not a readable zip: {e}"))?;
    for i in 0..z.len() {
        let e = z.by_index(i).map_err(|e| e.to_string())?;
        if let Some(first) = e
            .name()
            .ok()
            .and_then(|n| n.split('/').next().map(str::to_string))
        {
            if !first.is_empty() {
                return Ok(first);
            }
        }
    }
    Err("the backup is empty".into())
}

// ── HTTP ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListQuery {
    /// `share`, `local` or `drive`.
    pub from: From,
    /// The console (bare IP): needed for `drive`.
    #[serde(default)]
    pub addr: String,
}

/// The folders whose backups are listed from `from`: the share's or the automatic backups'
/// chosen folders (both for a PS5 drive, which either may copy to).
fn chosen(from: From) -> Vec<String> {
    let mut out = match from {
        From::Share => crate::save_backup_server::settings().folders,
        From::Local => local::settings().folders,
        From::Drive => {
            let mut v = crate::save_backup_server::settings().folders;
            v.extend(local::settings().folders);
            v
        }
    };
    out.sort();
    out.dedup();
    out
}

fn place_of<'a>(from: From, c: &'a dyn Drive) -> Result<Box<dyn Place + 'a>, String> {
    Ok(match from {
        From::Share => {
            let s = crate::save_backup_server::settings();
            if s.target.trim().is_empty() {
                return Err("no server is set for save backups".into());
            }
            Box::new(ShareSide { target: s.target })
        }
        From::Local => {
            let s = local::settings();
            if s.dir.trim().is_empty() {
                return Err("no backup folder is set".into());
            }
            Box::new(LocalSide { dir: s.dir })
        }
        From::Drive => {
            let drive = crate::save_backup_console::chosen_drives()
                .into_iter()
                .next()
                .ok_or("no PS5 drive is chosen for save backup copies")?;
            Box::new(DriveSide {
                c,
                root: crate::save_backup_console::root_of(&drive)?,
            })
        }
    })
}

/// The backups of the console folder `path` in `place` that the PS5 `console` (its id) may be
/// given back: its own, then those made before backups were kept per PS5 (`legacy`), each
/// newest first. Another PS5's own backups are never listed.
fn backups_for(
    place: &dyn Place,
    path: &str,
    console: Option<&str>,
) -> Result<Vec<FolderBackup>, String> {
    let mut out = match console {
        Some(id) => backups_of(place.list(&folder_rel_on(path, id)?)?),
        None => Vec::new(),
    };
    let mut old = backups_of(place.list(&folder_rel(path))?);
    for b in &mut old {
        b.legacy = true;
    }
    out.extend(old);
    Ok(out)
}

/// `GET /api/save-backup/folders?from=share|local|drive&addr=` — each chosen console folder with
/// the backups there that the PS5 at `addr` may be given back, newest first: `{console, folders:
/// [{path, backups: [{file, stamp, size, tag, legacy?}]}]}`. Its own backups, then (`legacy:
/// true`) those made before folder backups were kept per PS5, which may be of any PS5 with that
/// folder. Without `addr`, or when the PS5 can't be told (`console` null), only those.
pub async fn get_list(Query(q): Query<ListQuery>) -> Response {
    let r = tokio::task::spawn_blocking(move || {
        let host = q.addr.trim().to_string();
        let console = (!host.is_empty())
            .then(|| local::console_identity(&host))
            .flatten();
        let drive = crate::save_backup_console::Live(&host);
        let place = place_of(q.from, &drive)?;
        let mut out = Vec::new();
        for path in chosen(q.from) {
            let backups = backups_for(place.as_ref(), &path, console.as_deref())?;
            out.push(json!({ "path": path, "backups": backups }));
        }
        Ok::<_, String>((console, out))
    })
    .await;
    match r {
        Ok(Ok((console, folders))) => {
            Json(json!({ "console": console, "folders": folders })).into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct RestoreBody {
    pub from: From,
    /// The console (bare IP).
    pub addr: String,
    /// The console folder the backup is of (and goes back to).
    pub path: String,
    pub file: String,
}

/// `POST /api/save-backup/folders/restore` — `{from, addr, path, file}`: puts a folder backup
/// back where it came from, keeping what is there first (in the same place; the share or the
/// backup folder here for one on a PS5 drive). No other save operation on the console meanwhile.
/// Only this PS5's own backups, or one made before folder backups were kept per PS5: another
/// PS5's is refused.
pub async fn post_restore(Json(b): Json<RestoreBody>) -> Response {
    let r = tokio::task::spawn_blocking(move || {
        let host = b.addr.trim().to_string();
        if host.is_empty() {
            return Err("choose a console".to_string());
        }
        let _op = local::save_op(&[&host]);
        let drive = crate::save_backup_console::Live(&host);
        let place = place_of(b.from, &drive)?;
        // A copy kept before a restore from the drive goes to the share, else the folder here.
        let keep_in: Box<dyn Place> = match b.from {
            From::Drive => {
                place_of(From::Share, &drive).or_else(|_| place_of(From::Local, &drive))?
            }
            other => place_of(other, &drive)?,
        };
        let console = local::console_identity(&host);
        restore_with(
            &LiveConsole(&host),
            place.as_ref(),
            keep_in.as_ref(),
            b.path.trim(),
            b.file.trim(),
            console.as_deref(),
        )
    })
    .await;
    match r {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct CheckQuery {
    pub path: String,
}

/// `GET /api/save-backup/folders/check?path=` — `{ok, path}` (the path made plain) or `{ok:
/// false, error}`: whether a console folder may be chosen.
pub async fn get_check(Query(q): Query<CheckQuery>) -> Response {
    match folder_path_ok(&q.path) {
        Ok(p) => Json(json!({ "ok": true, "path": p })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::MemFs;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const GRIM: &str =
        "/data/prospero-win/prefixes/grim-dawn/drive_c/users/prospero/Documents/My Games/Grim Dawn";

    #[test]
    fn only_folders_the_helper_allows_and_not_its_own() {
        // The reporter's folders, and other ordinary ones.
        assert_eq!(folder_path_ok(&format!("{GRIM}/")).unwrap(), GRIM);
        for ok in [
            "/data/prospero-win/prefixes/warriors-orochi-4-ultimate-deluxe-edition/drive_c/users/prospero/Documents/KoeiTecmo/WARRIORS OROCHI 4",
            "/data/retro/saves",
            "/mnt/ext0/emulator/saves",
            "/mnt/usb1/homebrew/save",
            "/user/data/some-port",
        ] {
            assert!(folder_path_ok(ok).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("data/x", "starts with /"),
            ("/data", "whole drive"),
            ("/mnt/ext0", "whole drive"),
            ("/data/../system", "plain"),
            ("/data/./x", "plain"),
            ("/data//x", "plain"),
            ("/data/x\\y", "plain"),
            ("/system/priv", "only folders under"),
            ("/mnt/sandbox/x", "only folders under"),
            ("/mnt/ext9x/x", "only folders under"),
            ("/data/ps5upload", "own folder"),
            ("/data/PS5Upload/ava", "own folder"),
            ("/user/data/ps5upload/smb", "own folder"),
            ("/user/app/PPSA01234", "console's own"),
            ("/mnt/ext0/user/addcont/x", "console's own"),
            ("/user/home", "every user"),
            ("/user/home/10000000", "every user"),
            (
                "/user/home/10000000/savedata_prospero/PPSA01234",
                "save data",
            ),
            ("/mnt/ext0/ps5upload/save-backups", "backups' own"),
            ("/mnt/usb0/PS5Upload/save-backups", "backups' own"),
            // /user/data is /data: it holds the helper's own folder.
            ("/user/data", "own folder"),
            ("/user/data/", "own folder"),
        ] {
            let e = folder_path_ok(bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        // The list: plain, once each, every one checked.
        assert_eq!(
            folders_ok(&[format!("{GRIM}/"), GRIM.into(), String::new()]).unwrap(),
            vec![GRIM.to_string()]
        );
        assert!(folders_ok(&[GRIM.into(), "/data/ps5upload".into()]).is_err());
        // Two folders that differ in case are two folders on /data.
        assert_eq!(
            folders_ok(&["/data/Saves".into(), "/data/saves".into()])
                .unwrap()
                .len(),
            2
        );
    }

    /// Every rewrite of a file changes the signature (the hash's top bit used to fold half of
    /// all signatures into 1, so most rewrites went unseen).
    #[test]
    fn each_rewrite_changes_the_signature() {
        let mut seen = std::collections::HashSet::new();
        for mtime in 1000..1100 {
            let s = signature(&[("save.dat".to_string(), 4096, mtime)]);
            assert!(s >= 1);
            assert!(seen.insert(s), "mtime {mtime} gave a signature seen before");
        }
    }

    #[test]
    fn a_folder_gets_a_clear_name_of_its_own() {
        let n = dir_name(GRIM);
        assert!(
            n.starts_with("data_prospero-win_prefixes_grim-dawn_drive_c_users_prospero_Documents_My Games_Grim Dawn-"),
            "{n}"
        );
        assert_eq!(folder_rel(GRIM), format!("Folders/{n}"));
        // Stable, and two folders that read alike never share one.
        assert_eq!(dir_name(GRIM), n);
        assert_ne!(dir_name("/data/a_b"), dir_name("/data/a/b"));
        // Characters a NAS would mangle are gone; a long path keeps its end.
        let odd = dir_name("/data/x/Game: Part 2?/saves*");
        assert!(!odd.contains([':', '?', '*']), "{odd}");
        let long = format!("/data/{}/end", "deep/".repeat(60));
        let ln = dir_name(&long);
        assert!(ln.chars().count() <= 100 + 9, "{ln}");
        assert!(ln.contains("_end-"), "{ln}");
        // Its backups' names, and only those, are ours.
        let name = backup_name(None);
        assert!(ours(&name), "{name}");
        assert!(ours(&backup_name(Some(BEFORE_RESTORE_TAG))));
        for not in [
            "folder_.zip",
            "folder_2026-11-20_031200.zip.ps5upload-tmp",
            "my notes.zip",
            "PPSA01234_user10000000_2026-11-20_031200.zip",
            "folder_2026-11-20_031200_X.zip",
        ] {
            assert!(!ours(not), "{not}");
        }
    }

    #[test]
    fn versions_to_keep_count_each_kind_apart_and_spare_other_files() {
        let names: Vec<String> = [
            "folder_2026-11-01_000000.zip",
            "folder_2026-11-02_000000.zip",
            "folder_2026-11-03_000000.zip",
            "folder_2026-11-01_000000_beforerestore.zip",
            "folder_2026-11-03_000000.zip.sha256",
            "notes.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            to_prune(&names, None, 2),
            vec!["folder_2026-11-01_000000.zip".to_string()]
        );
        assert!(to_prune(&names, Some(BEFORE_RESTORE_TAG), 2).is_empty());
        assert_eq!(to_prune(&names, None, 0).len(), 2, "at least one is kept");
    }

    #[test]
    fn a_folder_changes_when_a_file_does() {
        let a = vec![
            ("a.sav".to_string(), 10, 100),
            ("x/b.sav".to_string(), 5, 50),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(signature(&a), signature(&b), "order does not matter");
        let mut c = a.clone();
        c[0].2 = 101;
        assert_ne!(signature(&a), signature(&c));
        let mut d = a.clone();
        d.push(("new.sav".into(), 1, 1));
        assert_ne!(signature(&a), signature(&d));
        assert!(signature(&[]) > 0);
        // The walk goes into folders and stops at the files.
        let tree: BTreeMap<&str, Vec<Value>> = [
            (
                "/data/g",
                vec![
                    json!({"name": "a.sav", "kind": "file", "size": 10, "mtime": 100}),
                    json!({"name": "x", "kind": "dir"}),
                ],
            ),
            (
                "/data/g/x",
                vec![json!({"name": "b.sav", "kind": "file", "size": 5, "mtime": 50})],
            ),
        ]
        .into();
        let list = |p: &str| {
            tree.get(p)
                .cloned()
                .ok_or_else(|| "no such folder".to_string())
        };
        let mut got = walk(&list, "/data/g").unwrap();
        got.sort();
        assert_eq!(got, a);
        assert!(walk(&list, "/data/none").is_err());
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5u-folders-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A zip of a folder copy, as a backup run makes one.
    fn folder_zip(dir: &Path, path: &str, files: &[(&str, &[u8])], tag: Option<&str>) -> PathBuf {
        let leaf = leaf_of(path).unwrap();
        let src = dir.join(format!("src-{}", files.len()));
        for (rel, b) in files {
            let p = src.join(&leaf).join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b).unwrap();
        }
        let zip = dir.join(backup_name(tag));
        zip_copy(&src, &leaf, &zip, path, Some("ABC")).unwrap();
        zip
    }

    #[test]
    fn a_folder_backup_on_the_share_is_checked_kept_and_pruned() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = scratch("share");
        let fs = MemFs::new(&[("/b/Folders/other/keep.me", b"x")]);
        let rel = folder_rel(GRIM);
        let zip = folder_zip(&dir, GRIM, &[("Save/game.gdc", b"progress")], None);
        let name = zip.file_name().unwrap().to_string_lossy().into_owned();
        let at = rt
            .block_on(store_on_server(
                &fs,
                &zip,
                "/b",
                &rel,
                &name,
                2,
                &dir.join("rb"),
                &quiet(),
                &|| {},
            ))
            .unwrap();
        assert_eq!(at, format!("/b/{rel}/{name}"));
        assert!(fs.bytes(&format!("{at}.sha256")).is_some());
        assert_eq!(noted_path(&zip).as_deref(), Some(GRIM));
        // Three older ones there: two remain in all.
        for old in [
            "folder_2020-01-01_000000.zip",
            "folder_2020-01-02_000000.zip",
        ] {
            fs.add(&[(format!("/b/{rel}/{old}").as_str(), b"old")]);
        }
        fs.add(&[(format!("/b/{rel}/mine.txt").as_str(), b"not ours")]);
        let zip2 = folder_zip(&dir, GRIM, &[("Save/game.gdc", b"more")], Some("x"));
        let name2 = "folder_2030-01-01_000000.zip";
        rt.block_on(store_on_server(
            &fs,
            &zip2,
            "/b",
            &rel,
            name2,
            2,
            &dir.join("rb"),
            &quiet(),
            &|| {},
        ))
        .unwrap();
        let left: Vec<String> = fs
            .paths()
            .into_iter()
            .filter(|p| p.starts_with(&format!("/b/{rel}/")) && p.ends_with(".zip"))
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(
            fs.bytes(&format!("/b/{rel}/mine.txt")).is_some(),
            "not ours: kept"
        );
        assert!(fs.bytes("/b/Folders/other/keep.me").is_some());
        // A server that garbles what it stores: caught, removed, not counted.
        fs.corrupt_writes_at(Some(40));
        let bad = rt.block_on(store_on_server(
            &fs,
            &zip,
            "/b",
            &rel,
            "folder_2031-01-01_000000.zip",
            2,
            &dir.join("rb"),
            &quiet(),
            &|| {},
        ));
        assert!(bad.unwrap_err().contains("verification failed"));
        assert!(fs
            .bytes(&format!("/b/{rel}/folder_2031-01-01_000000.zip"))
            .is_none());
        // Only our names go there.
        assert!(rt
            .block_on(store_on_server(
                &fs,
                &zip,
                "/b",
                "Saves",
                &name,
                2,
                &dir.join("rb"),
                &quiet(),
                &|| {}
            ))
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A console folder in memory, for restores.
    #[derive(Default)]
    struct FakeConsole {
        files: RefCell<BTreeMap<String, Vec<u8>>>,
        /// Uploads that fail (half done) before one goes through.
        fail_upload: std::cell::Cell<u32>,
        /// Listings that fail (the console busy) before one goes through.
        fail_entries: std::cell::Cell<u32>,
    }

    impl Console for FakeConsole {
        fn entries(&self, path: &str) -> Result<Vec<Value>, String> {
            if self.fail_entries.get() > 0 {
                self.fail_entries.set(self.fail_entries.get() - 1);
                return Err("timed out".into());
            }
            let prefix = format!("{path}/");
            let mut names: BTreeMap<String, bool> = BTreeMap::new();
            for p in self.files.borrow().keys() {
                if let Some(rest) = p.strip_prefix(&prefix) {
                    match rest.split_once('/') {
                        Some((d, _)) => names.insert(d.to_string(), true),
                        None => names.insert(rest.to_string(), false),
                    };
                }
            }
            if names.is_empty() {
                return Err("fs_list_dir_failed_errno_2".into());
            }
            Ok(names
                .into_iter()
                .map(|(n, d)| json!({"name": n, "kind": if d { "dir" } else { "file" }}))
                .collect())
        }
        fn download(&self, path: &str, dest: &Path) -> Result<(), String> {
            let leaf = leaf_of(path)?;
            let prefix = format!("{path}/");
            for (p, b) in self.files.borrow().iter() {
                if let Some(rest) = p.strip_prefix(&prefix) {
                    let to = dest.join(&leaf).join(rest);
                    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
                    std::fs::write(to, b).unwrap();
                }
            }
            Ok(())
        }
        fn delete(&self, path: &str) -> Result<(), String> {
            let prefix = format!("{path}/");
            self.files
                .borrow_mut()
                .retain(|p, _| p != path && !p.starts_with(&prefix));
            Ok(())
        }
        fn mkdir(&self, _path: &str) -> Result<(), String> {
            Ok(())
        }
        fn upload(&self, src: &Path, dest: &str) -> Result<(), String> {
            if self.fail_upload.get() > 0 {
                self.fail_upload.set(self.fail_upload.get() - 1);
                // Half of it lands, then the console drops.
                self.files
                    .borrow_mut()
                    .insert(format!("{dest}/half"), b"x".to_vec());
                return Err("The PS5 isn't answering".into());
            }
            fn walk_in(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
                for e in std::fs::read_dir(dir).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk_in(&p, base, out);
                    } else {
                        let rel = p
                            .strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        out.push((rel, std::fs::read(&p).unwrap()));
                    }
                }
            }
            let mut got = Vec::new();
            walk_in(src, src, &mut got);
            for (rel, b) in got {
                self.files.borrow_mut().insert(format!("{dest}/{rel}"), b);
            }
            Ok(())
        }
    }

    #[test]
    fn a_restore_keeps_what_is_there_first_and_puts_the_folder_back() {
        let dir = scratch("restore");
        let backups = dir.join("backups");
        let place = LocalSide {
            dir: backups.to_string_lossy().into_owned(),
        };
        let rel = folder_rel(GRIM);
        let zip = folder_zip(
            &dir,
            GRIM,
            &[("Save/game.gdc", b"old progress"), ("settings.txt", b"a")],
            None,
        );
        let name = zip.file_name().unwrap().to_string_lossy().into_owned();
        store_here(&place.dir, &rel, &name, &zip, 5).unwrap();
        let c = FakeConsole::default();
        c.files
            .borrow_mut()
            .insert(format!("{GRIM}/Save/game.gdc"), b"new progress".to_vec());
        c.files
            .borrow_mut()
            .insert(format!("{GRIM}/Save/extra.gdc"), b"made since".to_vec());
        restore_with(&c, &place, &place, GRIM, &name, None).unwrap();
        let files = c.files.borrow().clone();
        assert_eq!(files[&format!("{GRIM}/Save/game.gdc")], b"old progress");
        assert_eq!(files[&format!("{GRIM}/settings.txt")], b"a");
        assert!(
            !files.contains_key(&format!("{GRIM}/Save/extra.gdc")),
            "the folder is as backed up"
        );
        // What was there was kept first, beside the backups, and lists as such.
        let listed = backups_of(place.list(&rel).unwrap());
        let kept: Vec<&FolderBackup> = listed
            .iter()
            .filter(|b| b.tag.as_deref() == Some(BEFORE_RESTORE_TAG))
            .collect();
        assert_eq!(kept.len(), 1);
        assert!(listed.iter().any(|b| b.file == name && b.tag.is_none()));
        // That copy puts it back the way it was.
        restore_with(&c, &place, &place, GRIM, &kept[0].file.clone(), None).unwrap();
        assert_eq!(
            c.files.borrow()[&format!("{GRIM}/Save/extra.gdc")],
            b"made since"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restore_refuses_another_folders_backup_and_puts_back_what_was_there_on_failure() {
        let dir = scratch("restore-bad");
        let place = LocalSide {
            dir: dir.join("backups").to_string_lossy().into_owned(),
        };
        let other = "/data/retro/saves";
        // A backup of another folder, put in this folder's place by hand.
        let zip = folder_zip(&dir, other, &[("a.srm", b"x")], None);
        let name = zip.file_name().unwrap().to_string_lossy().into_owned();
        store_here(&place.dir, &folder_rel(GRIM), &name, &zip, 5).unwrap();
        let c = FakeConsole::default();
        c.files
            .borrow_mut()
            .insert(format!("{GRIM}/Save/game.gdc"), b"now".to_vec());
        let e = restore_with(&c, &place, &place, GRIM, &name, None).unwrap_err();
        assert!(e.contains("is a backup of /data/retro/saves"), "{e}");
        assert_eq!(c.files.borrow()[&format!("{GRIM}/Save/game.gdc")], b"now");
        // A console that drops while the backup goes in: what was there comes back.
        let good = folder_zip(&dir, GRIM, &[("Save/game.gdc", b"backup")], None);
        let gname = "folder_2026-01-01_000000.zip";
        store_here(&place.dir, &folder_rel(GRIM), gname, &good, 5).unwrap();
        c.fail_upload.set(1);
        let e = restore_with(&c, &place, &place, GRIM, gname, None).unwrap_err();
        assert!(e.contains("putting"), "{e}");
        let files = c.files.borrow().clone();
        assert_eq!(files[&format!("{GRIM}/Save/game.gdc")], b"now");
        assert!(
            !files.contains_key(&format!("{GRIM}/half")),
            "no half restore left"
        );
        // And the copy kept first is there either way.
        let listed = backups_of(place.list(&folder_rel(GRIM)).unwrap());
        assert!(listed
            .iter()
            .any(|b| b.tag.as_deref() == Some(BEFORE_RESTORE_TAG)));
        // Names that are not ours, or folders the rules refuse, are refused outright.
        assert!(restore_with(&c, &place, &place, GRIM, "x.zip", None).is_err());
        assert!(restore_with(&c, &place, &place, "/data/ps5upload/ava", gname, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Folder backups are kept per PS5: a PS5 lists and gets back its own and the ones made
    /// before that (marked legacy), never another PS5's of the same folder.
    #[test]
    fn a_restore_takes_this_ps5s_backups_and_the_old_ones_never_anothers() {
        let dir = scratch("restore-per-ps5");
        let place = LocalSide {
            dir: dir.join("backups").to_string_lossy().into_owned(),
        };
        // folder_zip notes the PS5 "ABC".
        let mine = folder_zip(&dir, GRIM, &[("Save/game.gdc", b"abc's")], None);
        let a = "folder_2026-01-02_000000.zip";
        store_here(
            &place.dir,
            &folder_rel_on(GRIM, "ABC").unwrap(),
            a,
            &mine,
            5,
        )
        .unwrap();
        let old = "folder_2025-12-01_000000.zip";
        store_here(&place.dir, &folder_rel(GRIM), old, &mine, 5).unwrap();
        // Two PS5s, two places; the old shared one apart from both.
        assert_ne!(
            folder_rel_on(GRIM, "abc").unwrap(),
            folder_rel_on(GRIM, "DEF").unwrap()
        );
        assert_eq!(
            folder_rel_on(GRIM, "a:b-c").unwrap(),
            folder_rel_on(GRIM, "ABC").unwrap()
        );
        assert!(folder_rel_on(GRIM, "::").is_err());

        // Listed for ABC: its own, then the old one marked.
        let listed = backups_for(&place, GRIM, Some("ABC")).unwrap();
        let names: Vec<(&str, bool)> = listed.iter().map(|b| (b.file.as_str(), b.legacy)).collect();
        assert_eq!(names, vec![(a, false), (old, true)]);
        // For DEF: only the old one; without an id: only the old one.
        let listed = backups_for(&place, GRIM, Some("DEF")).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].legacy);
        assert_eq!(backups_for(&place, GRIM, None).unwrap().len(), 1);

        let c = FakeConsole::default();
        c.files
            .borrow_mut()
            .insert(format!("{GRIM}/Save/game.gdc"), b"now".to_vec());
        // DEF can't have ABC's own backup.
        let e = restore_with(&c, &place, &place, GRIM, a, Some("DEF")).unwrap_err();
        assert!(e.contains("not one of this PS5's backups"), "{e}");
        assert_eq!(c.files.borrow()[&format!("{GRIM}/Save/game.gdc")], b"now");
        // An old one it may: the person picked it. What was there is kept as DEF's own.
        restore_with(&c, &place, &place, GRIM, old, Some("DEF")).unwrap();
        assert_eq!(c.files.borrow()[&format!("{GRIM}/Save/game.gdc")], b"abc's");
        let def = backups_for(&place, GRIM, Some("DEF")).unwrap();
        assert!(def
            .iter()
            .any(|b| !b.legacy && b.tag.as_deref() == Some(BEFORE_RESTORE_TAG)));
        // ABC gets its own back.
        restore_with(&c, &place, &place, GRIM, a, Some("ABC")).unwrap();
        // A zip of ABC's put by hand among DEF's own is refused by what it says inside.
        store_here(
            &place.dir,
            &folder_rel_on(GRIM, "DEF").unwrap(),
            a,
            &mine,
            5,
        )
        .unwrap();
        let e = restore_with(&c, &place, &place, GRIM, a, Some("DEF")).unwrap_err();
        assert!(e.contains("made on another PS5"), "{e}");
        // A PS5 that can't be told gets only the old ones.
        let e = restore_with(
            &c,
            &place,
            &place,
            GRIM,
            "folder_2030-01-01_000000.zip",
            None,
        )
        .unwrap_err();
        assert_eq!(e, UNKNOWN_CONSOLE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A console that could not be asked what the folder holds (busy, timed out) is not a folder
    /// that is not there: nothing is removed without a copy kept first.
    #[test]
    fn a_folder_that_could_not_be_looked_at_is_left_alone() {
        let dir = scratch("restore-unasked");
        let place = LocalSide {
            dir: dir.join("backups").to_string_lossy().into_owned(),
        };
        let good = folder_zip(&dir, GRIM, &[("Save/game.gdc", b"backup")], None);
        let gname = "folder_2026-01-01_000000.zip";
        store_here(&place.dir, &folder_rel(GRIM), gname, &good, 5).unwrap();
        let c = FakeConsole::default();
        c.files
            .borrow_mut()
            .insert(format!("{GRIM}/Save/game.gdc"), b"now".to_vec());
        c.files
            .borrow_mut()
            .insert(format!("{GRIM}/notes.txt"), b"mine".to_vec());
        c.fail_entries.set(1);
        let e = restore_with(&c, &place, &place, GRIM, gname, None).unwrap_err();
        assert!(e.contains("could not look at"), "{e}");
        let files = c.files.borrow().clone();
        assert_eq!(files[&format!("{GRIM}/Save/game.gdc")], b"now");
        assert_eq!(files[&format!("{GRIM}/notes.txt")], b"mine");
        // A folder that is not there at all is made from the backup.
        let fresh = "/data/retro/saves";
        let z = folder_zip(&dir, fresh, &[("a.srm", b"x")], None);
        store_here(&place.dir, &folder_rel(fresh), gname, &z, 5).unwrap();
        restore_with(&c, &place, &place, fresh, gname, None).unwrap();
        assert_eq!(c.files.borrow()[&format!("{fresh}/a.srm")], b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
