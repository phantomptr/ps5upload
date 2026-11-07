//! Backup & Restore (#707): the parts of a console worth having after it has been reset, in one
//! folder on this computer, and back onto the console.
//!
//! What is worth having is a short list on purpose ([`group_of`]). Saves, captures, payload settings
//! and homebrew come back and work. Installed games don't (copying `/user/app` back doesn't install
//! anything), and `/system_data` is the console's own state, which the helper refuses to write.
//!
//! Saves go through the Saves screen's own code, one `.zip` per game and user. A save can't be
//! copied back as plain files: it is encrypted for this console, and the account it belongs to is
//! in the save and in the console's save database. A reset console has new user numbers, so a save
//! usually goes back with [`save_copy::restore_to_user`], which needs that user's own save of the
//! game to put it in. A save with nowhere to go yet is reported, and restoring again later is safe.
//!
//! Everything else goes over the engine's own transfer routes, as `save_backup` does: one transfer
//! path to trust. One run at a time. A folder that fails is noted and the rest carry on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::save_backup::{self, back_up_one, enc, folder_entries, get, list_saves, post};
use crate::save_copy;

/// The manifest at the top of a backup folder.
const MANIFEST: &str = "backup.json";
const FORMAT: u32 = 1;
/// Under a backup folder: saves as zips, and everything else at its console path.
const SAVES_DIR: &str = "saves";
const FILES_DIR: &str = "files";
/// Never reached in practice; a save's backups in this folder are never thrown away.
const KEEP_ALL: u32 = 1000;

pub const GROUP_SAVES: &str = "saves";
pub const GROUP_CAPTURES: &str = "captures";
pub const GROUP_SETTINGS: &str = "settings";
pub const GROUP_HOMEBREW: &str = "homebrew";
pub const GROUP_OTHER: &str = "other";

const CAPTURES_DIR: &str = "/user/av_contents";

/// Payloads' own folders in `/data`, kept apart from what takes real room.
const SETTINGS_FOLDERS: &[&str] = &[
    "etaHEN",
    "shadowmount",
    "ps5_autoloader",
    "pldmgr",
    "GoldHEN",
];

/// Left out of a backup after it's copied: logs and temporary files help nobody on a fresh console.
const NOT_WORTH_KEEPING: &[&str] = &["*.log", "*.log.old", "*.log.1", "*.tmp", "*.bak"];

/// Which group a folder in `/data` belongs to. `ps5upload` is this app's own, and it comes back
/// with the helper: its trust store can't be read, and its temporary packages aren't wanted.
pub fn group_of(data_folder: &str) -> Option<&'static str> {
    if data_folder == "ps5upload" || data_folder == "." || data_folder == ".." {
        return None;
    }
    if data_folder == "homebrew" {
        return Some(GROUP_HOMEBREW);
    }
    if SETTINGS_FOLDERS.contains(&data_folder) {
        return Some(GROUP_SETTINGS);
    }
    Some(GROUP_OTHER)
}

/// A folder of a group, with what the scan measured.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub path: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub files: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    #[serde(default)]
    pub bytes: u64,
    /// Files, or saves for the saves group.
    #[serde(default)]
    pub items: u64,
    #[serde(default)]
    pub folders: Vec<Folder>,
}

/// One save in a backup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedGame {
    pub title_id: String,
    pub user_id: u32,
    /// The save folder on the console it came from.
    pub path: String,
    /// The zip, under `saves/<title_id>/`.
    pub file: String,
    #[serde(default)]
    pub bytes: u64,
}

/// A user on the console when the backup was made, for matching them to a user on a reset console.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupUser {
    pub uid: u32,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub made_at_ms: u64,
    #[serde(default)]
    pub host: String,
    /// The PS5 it was made on (its host-id), if it said: saves open only on that one.
    #[serde(default)]
    pub console: Option<String>,
    /// False until the last folder is done: a backup cut short says so.
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub saves: Vec<SavedGame>,
    #[serde(default)]
    pub users: Vec<BackupUser>,
    /// What couldn't be copied.
    #[serde(default)]
    pub failed: Vec<Outcome>,
}

/// What became of one folder or save.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub group: String,
    /// The console path, or a game's title id.
    pub item: String,
    /// `done`, `failed`, or `needs_save`: that user has no save of the game to put it in yet.
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn outcome(group: &str, item: &str, r: Result<(), String>) -> Outcome {
    let (result, detail) = match r {
        Ok(()) => ("done", None),
        Err(e) if needs_a_save(&e) => ("needs_save", Some(e)),
        Err(e) => ("failed", Some(e)),
    };
    Outcome {
        group: group.into(),
        item: item.into(),
        result: result.into(),
        detail,
    }
}

/// The refusal for a user with no save of the game yet (`save_copy::no_target_save`).
fn needs_a_save(e: &str) -> bool {
    e.contains("has no save of") || e.contains("no save of this user")
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    /// `idle`, `scanning`, `backing_up`, `restoring`, `done`, `cancelled` or `failed`.
    pub state: String,
    /// What this run is or was: `scanning`, `backing_up` or `restoring`.
    pub kind: String,
    pub host: String,
    /// What it's on now, and how far through.
    pub current: String,
    pub step: u32,
    pub steps: u32,
    /// The running transfer's bytes, when it says.
    pub bytes: u64,
    pub total_bytes: u64,
    /// The scan's answer.
    pub groups: Vec<Group>,
    /// The backup folder made or read.
    pub dir: String,
    pub outcomes: Vec<Outcome>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Shared {
    status: Status,
    job: Option<String>,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Shared {
            status: Status {
                state: "idle".into(),
                ..Status::default()
            },
            job: None,
        })
    })
}

static CANCEL: AtomicBool = AtomicBool::new(false);

fn update(f: impl FnOnce(&mut Status)) {
    f(&mut shared().lock().unwrap_or_else(|e| e.into_inner()).status);
}

fn stopped() -> Result<(), String> {
    if CANCEL.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

const CANCELLED: &str = "cancelled";

/// What ends a run with the backup left marked as cut short. Everything after a console goes to
/// rest would fail the same way, and a list of those hides the one thing to do.
const WENT_TO_REST: &str =
    "The PS5 went into rest mode, so this stopped. Wake it and run it again.";

/// Whether this failure ends the whole run rather than just this item.
fn run_over(result: &Result<(), String>) -> Option<String> {
    match result {
        Err(e) if e == CANCELLED => Some(CANCELLED.into()),
        Err(e) if e.to_ascii_lowercase().contains("rest mode") => Some(WENT_TO_REST.into()),
        _ => None,
    }
}

/// The console's own copies in a user's save folder, which it manages itself, aren't anyone's game.
pub fn is_a_game_save(title_id: &str) -> bool {
    !title_id.starts_with("sce_")
}

fn game_saves(host: &str) -> Result<Vec<SaveEntry>, String> {
    Ok(list_saves(host)?
        .into_iter()
        .filter(|s| is_a_game_save(&s.title_id))
        .collect())
}

/// Takes the one slot, or says what holds it. `work` runs on its own thread, and its result ends
/// the run however it went.
fn begin(
    state: &str,
    host: &str,
    work: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    {
        let mut g = shared().lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            g.status.state.as_str(),
            "scanning" | "backing_up" | "restoring"
        ) {
            return Err("A backup or restore is already running.".into());
        }
        g.status = Status {
            state: state.into(),
            kind: state.into(),
            host: host.into(),
            ..Status::default()
        };
        g.job = None;
    }
    CANCEL.store(false, Ordering::Relaxed);
    std::thread::spawn(move || {
        // A panic mustn't leave the slot taken until the engine restarts.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err("the run stopped unexpectedly".into()));
        update(|s| {
            s.current.clear();
            match r {
                Ok(()) => s.state = "done".into(),
                Err(e) if e == CANCELLED => s.state = "cancelled".into(),
                Err(e) => {
                    s.state = "failed".into();
                    s.error = Some(e);
                }
            }
        });
    });
    Ok(())
}

/// Waits for a transfer, showing its bytes and passing a cancel on. A job cancelled by someone else
/// ends the run the same way.
fn follow_job(job: &str) -> Result<(), String> {
    shared().lock().unwrap_or_else(|e| e.into_inner()).job = Some(job.to_string());
    loop {
        if CANCEL.load(Ordering::Relaxed) {
            let _ = post(&format!("/api/jobs/{job}/cancel"), json!({}));
            return Err(CANCELLED.into());
        }
        let v = get(&format!("/api/jobs/{job}"))?;
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        update(|s| {
            s.bytes = n("bytes_sent").max(n("bytes_received"));
            s.total_bytes = n("total_bytes");
        });
        match v.get("status").and_then(Value::as_str) {
            Some("done") => return Ok(()),
            Some("failed") => {
                return Err(v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("transfer failed")
                    .to_string())
            }
            Some("cancelled") => return Err(CANCELLED.into()),
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

// ── scan ────────────────────────────────────────────────────────────────

/// The size of a folder on the console: `(bytes, files)`, or None if it isn't there.
fn measure(host: &str, path: &str) -> Result<Option<(u64, u64)>, String> {
    let started = loop {
        match post("/api/ps5/disk-usage", json!({ "addr": host, "path": path })) {
            Ok(v) => break v,
            // Another screen is measuring this console: it finishes, or nobody watches it and it
            // is abandoned within a minute.
            Err(e) if e == "disk_usage_busy" => {
                stopped()?;
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => return Err(e),
        }
    };
    let id = started
        .get("id")
        .and_then(Value::as_str)
        .ok_or("the walk did not start")?
        .to_string();
    loop {
        if CANCEL.load(Ordering::Relaxed) {
            let _ = post(&format!("/api/ps5/disk-usage/{id}/cancel"), json!({}));
            return Err(CANCELLED.into());
        }
        let v = get(&format!("/api/ps5/disk-usage/{id}"))?;
        match v.get("state").and_then(Value::as_str) {
            Some("running") => std::thread::sleep(Duration::from_millis(400)),
            Some("done") => {
                let tree = v.get("tree").cloned().unwrap_or(Value::Null);
                let n = |k: &str| tree.get(k).and_then(Value::as_u64).unwrap_or(0);
                return Ok(Some((n("bytes"), n("files"))));
            }
            // A folder this console doesn't have, or can't list.
            _ => return Ok(None),
        }
    }
}

/// The folders of `/data` with the group each is in. Only folders: every payload keeps its things
/// in one, and a restore works on folders.
fn data_folders(host: &str) -> Vec<(String, &'static str)> {
    let Ok(entries) = folder_entries(host, "/data") else {
        return Vec::new();
    };
    let mut out: Vec<(String, &'static str)> = entries
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some("dir"))
        .filter_map(|e| e.get("name").and_then(Value::as_str))
        .filter_map(|name| group_of(name).map(|g| (format!("/data/{name}"), g)))
        .collect();
    out.sort();
    out
}

fn scan(host: &str) -> Result<(), String> {
    let mut wanted: Vec<(String, &'static str)> = vec![(CAPTURES_DIR.to_string(), GROUP_CAPTURES)];
    wanted.extend(data_folders(host));
    update(|s| s.steps = wanted.len() as u32 + 1);

    let saves = game_saves(host).unwrap_or_default();
    let mut groups: BTreeMap<&'static str, Group> = BTreeMap::new();
    for id in [
        GROUP_SAVES,
        GROUP_CAPTURES,
        GROUP_SETTINGS,
        GROUP_HOMEBREW,
        GROUP_OTHER,
    ] {
        groups.insert(
            id,
            Group {
                id: id.into(),
                ..Group::default()
            },
        );
    }
    if let Some(g) = groups.get_mut(GROUP_SAVES) {
        g.items = saves.len() as u64;
        g.bytes = saves.iter().map(|s| s.size.max(0) as u64).sum();
    }
    update(|s| {
        s.step = 1;
        s.groups = groups.values().cloned().collect();
    });
    for (i, (path, group)) in wanted.iter().enumerate() {
        stopped()?;
        update(|s| {
            s.step = i as u32 + 2;
            s.current = path.clone();
        });
        if let Some((bytes, files)) = measure(host, path)? {
            if let Some(g) = groups.get_mut(group) {
                g.bytes += bytes;
                g.items += files;
                g.folders.push(Folder {
                    path: path.clone(),
                    bytes,
                    files,
                });
            }
            update(|s| s.groups = groups.values().cloned().collect());
        }
    }
    Ok(())
}

// ── backup ──────────────────────────────────────────────────────────────

/// Where a console folder sits in a backup: its path under `files/`. None for a path that could
/// climb out of the backup, which a manifest edited by hand might hold.
pub fn local_path(root: &Path, console_path: &str) -> Option<PathBuf> {
    let rel = console_path.strip_prefix('/')?;
    if rel.is_empty()
        || rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return None;
    }
    Some(root.join(FILES_DIR).join(rel))
}

/// A path a backup may put things in on the console: only what a backup can hold.
pub fn restorable(console_path: &str) -> bool {
    if console_path.split('/').any(|p| p == "..") {
        return false;
    }
    if console_path == CAPTURES_DIR {
        return true;
    }
    console_path
        .strip_prefix("/data/")
        .is_some_and(|name| !name.contains('/') && group_of(name).is_some())
}

/// Removes what isn't worth keeping from a copied folder. Returns how many files went.
pub fn prune(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut gone = 0;
    for e in entries.flatten() {
        let p = e.path();
        // `file_type` doesn't follow a link, so nothing outside the backup is ever touched.
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            gone += prune(&p);
        } else if ps5upload_core::excludes::is_excluded(&p, NOT_WORTH_KEEPING)
            && std::fs::remove_file(&p).is_ok()
        {
            gone += 1;
        }
    }
    gone
}

fn write_manifest(root: &Path, m: &Manifest) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    let tmp = root.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, root.join(MANIFEST)))
        .map_err(|e| crate::state_io::describe("the backup folder", root, &e))
}

/// The users on the console, by what the app's own Accounts screen reads.
fn users_of(host: &str) -> Vec<BackupUser> {
    get(&format!("/api/profile/info?addr={}", enc(host)))
        .ok()
        .and_then(|v| v.get("users").cloned())
        .and_then(|u| u.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|u| {
            Some(BackupUser {
                uid: u.get("uid")?.as_u64()? as u32,
                name: u
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

fn back_up(host: &str, dir: &str, wanted: &[String]) -> Result<(), String> {
    save_backup::writable(dir)?;
    let at = save_backup::now_ms();
    let root = Path::new(dir).join(format!("ps5-backup-{}", save_backup::stamp(at)));
    std::fs::create_dir_all(&root)
        .map_err(|e| crate::state_io::describe("the backup folder", &root, &e))?;
    update(|s| s.dir = root.to_string_lossy().into_owned());

    let has = |g: &str| wanted.iter().any(|w| w == g);
    let saves: Vec<SaveEntry> = if has(GROUP_SAVES) {
        game_saves(host)?
    } else {
        Vec::new()
    };
    let mut folders: Vec<(String, &'static str)> = Vec::new();
    if has(GROUP_CAPTURES) {
        folders.push((CAPTURES_DIR.to_string(), GROUP_CAPTURES));
    }
    folders.extend(data_folders(host).into_iter().filter(|(_, g)| has(g)));

    let mut m = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        made_at_ms: at,
        host: host.to_string(),
        console: save_backup::console_identity(host),
        users: users_of(host),
        ..Manifest::default()
    };
    write_manifest(&root, &m)?;
    update(|s| s.steps = (saves.len() + folders.len()) as u32);

    let saves_dir = root.join(SAVES_DIR);
    let saves_dir_s = saves_dir.to_string_lossy().into_owned();
    let mut step = 0;
    for entry in &saves {
        stopped()?;
        step += 1;
        update(|s| {
            s.step = step;
            s.current = entry.title_id.clone();
            s.bytes = 0;
            s.total_bytes = 0;
        });
        let made = back_up_one(host, entry, &saves_dir_s, KEEP_ALL, None, &|| Ok(()));
        let result = match made {
            Ok(zip) => {
                m.saves.push(SavedGame {
                    title_id: entry.title_id.clone(),
                    user_id: entry.user_id as u32,
                    path: entry.path.clone(),
                    file: zip
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    bytes: entry.size.max(0) as u64,
                });
                Ok(())
            }
            Err(e) => Err(e),
        };
        if let Some(why) = run_over(&result) {
            return Err(why);
        }
        note(&mut m, GROUP_SAVES, &entry.title_id, result);
        write_manifest(&root, &m)?;
    }
    for (path, group) in &folders {
        stopped()?;
        step += 1;
        update(|s| {
            s.step = step;
            s.current = path.clone();
            s.bytes = 0;
            s.total_bytes = 0;
        });
        let result = copy_down(host, &root, path);
        if let Some(why) = run_over(&result) {
            return Err(why);
        }
        if result.is_ok() {
            add_folder(&mut m, group, path);
        }
        note(&mut m, group, path, result);
        write_manifest(&root, &m)?;
    }
    m.complete = true;
    write_manifest(&root, &m)
}

fn copy_down(host: &str, root: &Path, path: &str) -> Result<(), String> {
    let local = local_path(root, path).ok_or("not a path a backup can hold")?;
    let parent = local.parent().ok_or("not a path a backup can hold")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| crate::state_io::describe("the backup folder", parent, &e))?;
    let start = post(
        "/api/transfer/download",
        json!({ "addr": host, "src_path": path, "dest_dir": parent.to_string_lossy(), "kind": "folder" }),
    )?;
    let job = start
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or("download did not start")?;
    follow_job(job)?;
    prune(&local);
    Ok(())
}

fn add_folder(m: &mut Manifest, group: &str, path: &str) {
    let g = match m.groups.iter().position(|g| g.id == group) {
        Some(i) => &mut m.groups[i],
        None => {
            m.groups.push(Group {
                id: group.into(),
                ..Group::default()
            });
            m.groups.last_mut().expect("just pushed")
        }
    };
    g.folders.push(Folder {
        path: path.into(),
        ..Folder::default()
    });
}

fn note(m: &mut Manifest, group: &str, item: &str, result: Result<(), String>) {
    let o = outcome(group, item, result);
    if o.result != "done" {
        m.failed.push(o.clone());
    }
    update(|s| s.outcomes.push(o));
}

// ── restore ─────────────────────────────────────────────────────────────

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST);
    let text = std::fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "This folder isn't a backup: it has no backup.json.".to_string()
        } else {
            crate::state_io::describe("the backup", &path, &e)
        }
    })?;
    let m: Manifest =
        serde_json::from_slice(&text).map_err(|e| format!("backup.json can't be read: {e}"))?;
    if m.format != FORMAT {
        return Err(format!(
            "This backup was made by a newer version of the app (format {}). Update the app first.",
            m.format
        ));
    }
    Ok(m)
}

/// The user a save goes back to: the one the person chose, or the same user if they're here.
pub fn target_user(from: u32, chosen: &BTreeMap<String, u32>, here: &[u32]) -> Option<u32> {
    chosen
        .get(&from.to_string())
        .copied()
        .or_else(|| here.contains(&from).then_some(from))
}

fn restore_save(
    host: &str,
    root: &Path,
    s: &SavedGame,
    to: u32,
    live: &save_copy::Live<'_>,
) -> Result<(), String> {
    if !save_backup::plain(&s.title_id) || !save_backup::plain(&s.file) {
        return Err("unexpected backup name".into());
    }
    let zip = root.join(SAVES_DIR).join(&s.title_id).join(&s.file);
    if !zip.is_file() {
        return Err(format!("no backup {}", zip.display()));
    }
    if to == s.user_id {
        save_backup::restore_zip(host, &s.title_id, &zip, &s.path, to)
    } else {
        save_copy::restore_to_user(live, &s.title_id, &s.file, &zip, to)
            .map(|_| ())
            .map_err(|e| e.msg)
    }
}

fn restore(
    host: &str,
    dir: &str,
    wanted: &[String],
    chosen: &BTreeMap<String, u32>,
) -> Result<(), String> {
    let root = PathBuf::from(dir);
    let m = read_manifest(&root)?;
    update(|s| s.dir = dir.to_string());
    let has = |g: &str| wanted.iter().any(|w| w == g);

    let folders: Vec<(String, String)> = m
        .groups
        .iter()
        .filter(|g| has(&g.id))
        .flat_map(|g| g.folders.iter().map(|f| (g.id.clone(), f.path.clone())))
        .collect();
    let saves: &[SavedGame] = if has(GROUP_SAVES) { &m.saves } else { &[] };
    update(|s| s.steps = (folders.len() + saves.len()) as u32);

    // Settings and folders first: a payload's settings don't depend on anything, and a save may
    // need a game started first.
    let mut step = 0;
    for (group, path) in &folders {
        stopped()?;
        step += 1;
        update(|s| {
            s.step = step;
            s.current = path.clone();
            s.bytes = 0;
            s.total_bytes = 0;
        });
        let result = copy_up(host, &root, path);
        if let Some(why) = run_over(&result) {
            return Err(why);
        }
        let o = outcome(group, path, result);
        update(|s| s.outcomes.push(o));
    }
    if saves.is_empty() {
        return Ok(());
    }
    let here: Vec<u32> = users_of(host).iter().map(|u| u.uid).collect();
    let live = save_copy::Live::keeping_in(host, root.join("before-restore"));
    for s in saves {
        stopped()?;
        step += 1;
        update(|st| {
            st.step = step;
            st.current = s.title_id.clone();
            st.bytes = 0;
            st.total_bytes = 0;
        });
        let result = match target_user(s.user_id, chosen, &here) {
            Some(to) => restore_save(host, &root, s, to, &live),
            None => Err("No user was chosen for this save.".to_string()),
        };
        if let Some(why) = run_over(&result) {
            return Err(why);
        }
        let o = outcome(GROUP_SAVES, &s.title_id, result);
        update(|st| st.outcomes.push(o));
    }
    Ok(())
}

fn copy_up(host: &str, root: &Path, path: &str) -> Result<(), String> {
    if !restorable(path) {
        return Err("This backup names a folder the app won't write to.".into());
    }
    let local = local_path(root, path).ok_or("not a path a backup can hold")?;
    if !local.is_dir() {
        return Err(format!("{} isn't in this backup", path));
    }
    let start = post(
        "/api/transfer/dir",
        json!({
            "addr": host,
            "src_dir": local.to_string_lossy(),
            "dest_root": path,
            "excludes": ps5upload_core::excludes::DEFAULT_EXCLUDES,
        }),
    )?;
    let job = start
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or("upload did not start")?;
    follow_job(job)
}

// ── routes ──────────────────────────────────────────────────────────────

fn answer(r: Result<(), String>) -> Response {
    match r {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (StatusCode::CONFLICT, Json(json!({ "error": e }))).into_response(),
    }
}

fn refuse_while_busy(host: &str) -> Result<(), String> {
    if save_backup::console_busy(host) {
        return Err(
            "Something else is using this console. Wait for it to finish, then try again.".into(),
        );
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct ScanBody {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
}

/// `POST /api/console-backup/scan` `{addr}`: what could be backed up, and how big. Follow it on
/// `/status`.
pub async fn post_scan(Json(b): Json<ScanBody>) -> Response {
    let host = b.addr.trim().to_string();
    let h = host.clone();
    answer(begin("scanning", &host, move || scan(&h)))
}

#[derive(Deserialize)]
pub struct BackupBody {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
    /// A folder on the computer the engine runs on; the backup is a new folder inside it.
    pub dir: String,
    pub groups: Vec<String>,
}

/// `POST /api/console-backup/backup` `{addr, dir, groups}`.
pub async fn post_backup(Json(b): Json<BackupBody>) -> Response {
    let host = b.addr.trim().to_string();
    let h = host.clone();
    let r = tokio::task::spawn_blocking(move || {
        refuse_while_busy(&h)?;
        let host = h.clone();
        begin("backing_up", &h, move || {
            back_up(&host, b.dir.trim(), &b.groups)
        })
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    answer(r)
}

#[derive(Deserialize)]
pub struct InspectBody {
    pub dir: String,
}

/// `POST /api/console-backup/inspect` `{dir}`: what a backup folder holds.
pub async fn post_inspect(Json(b): Json<InspectBody>) -> Response {
    match read_manifest(Path::new(b.dir.trim())) {
        Ok(m) => Json(m).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))).into_response(),
    }
}

#[derive(Deserialize)]
pub struct RestoreBody {
    #[serde(alias = "console", alias = "host")]
    pub addr: String,
    pub dir: String,
    pub groups: Vec<String>,
    /// For saves: the user number in the backup to the user here it goes to.
    #[serde(default)]
    pub users: BTreeMap<String, u32>,
}

/// `POST /api/console-backup/restore` `{addr, dir, groups, users}`.
pub async fn post_restore(Json(b): Json<RestoreBody>) -> Response {
    let host = b.addr.trim().to_string();
    let h = host.clone();
    let r = tokio::task::spawn_blocking(move || {
        refuse_while_busy(&h)?;
        let host = h.clone();
        begin("restoring", &h, move || {
            restore(&host, b.dir.trim(), &b.groups, &b.users)
        })
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    answer(r)
}

/// `GET /api/console-backup/status`.
pub async fn get_status() -> Response {
    let s = shared()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .status
        .clone();
    Json(s).into_response()
}

/// `POST /api/console-backup/cancel`: stops after what's copying now is told to.
pub async fn post_cancel() -> Response {
    CANCEL.store(true, Ordering::Relaxed);
    Json(json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_folders_go_to_the_right_group_and_our_own_stays_out() {
        assert_eq!(group_of("homebrew"), Some(GROUP_HOMEBREW));
        assert_eq!(group_of("shadowmount"), Some(GROUP_SETTINGS));
        assert_eq!(group_of("etaHEN"), Some(GROUP_SETTINGS));
        assert_eq!(group_of("something_else"), Some(GROUP_OTHER));
        assert_eq!(group_of("ps5upload"), None);
    }

    #[test]
    fn a_console_path_stays_inside_the_backup() {
        let root = Path::new("/b");
        assert_eq!(
            local_path(root, "/data/homebrew"),
            Some(PathBuf::from("/b/files/data/homebrew"))
        );
        assert_eq!(local_path(root, "/data/../../x"), None);
        assert_eq!(local_path(root, "data/x"), None);
        assert_eq!(local_path(root, "/"), None);
    }

    #[test]
    fn a_restore_writes_only_what_a_backup_can_hold() {
        assert!(restorable("/user/av_contents"));
        assert!(restorable("/data/homebrew"));
        assert!(restorable("/data/shadowmount"));
        // A manifest edited by hand can't reach the console's own state or our helper's.
        for bad in [
            "/system_data/priv",
            "/data/ps5upload",
            "/data/../system_data",
            "/user/home/1/savedata",
            "/data",
            "/data/a/b",
        ] {
            assert!(!restorable(bad), "{bad}");
        }
    }

    #[test]
    fn logs_and_temporary_files_are_left_out_and_the_rest_kept() {
        let dir = std::env::temp_dir().join(format!("cb-prune-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["config.ini", "debug.log", "sub/x.tmp", "sub/keep.bin"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert_eq!(prune(&dir), 2);
        assert!(dir.join("config.ini").exists());
        assert!(dir.join("sub/keep.bin").exists());
        assert!(!dir.join("debug.log").exists());
        assert!(!dir.join("sub/x.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_save_goes_to_the_chosen_user_or_the_same_one() {
        let chosen: BTreeMap<String, u32> = [("5".to_string(), 9)].into();
        assert_eq!(target_user(5, &chosen, &[9]), Some(9));
        assert_eq!(target_user(7, &chosen, &[7, 9]), Some(7));
        // A reset console's new users: nowhere to go until someone says.
        assert_eq!(target_user(7, &chosen, &[9]), None);
    }

    #[test]
    fn a_user_with_no_save_of_the_game_is_told_apart_from_a_failure() {
        let o = outcome(
            GROUP_SAVES,
            "PPSA1",
            Err("The other user has no save of PPSA1 yet. Start the game once.".into()),
        );
        assert_eq!(o.result, "needs_save");
        assert_eq!(outcome(GROUP_SAVES, "PPSA1", Ok(())).result, "done");
        assert_eq!(
            outcome(GROUP_SAVES, "PPSA1", Err("x".into())).result,
            "failed"
        );
    }

    #[test]
    fn rest_mode_and_a_cancel_end_the_run_and_other_failures_do_not() {
        let rest =
            Err("The PS5 is in rest mode. Wake it, and the helper can be sent again.".into());
        assert_eq!(run_over(&rest).as_deref(), Some(WENT_TO_REST));
        assert_eq!(run_over(&Err(CANCELLED.into())).as_deref(), Some(CANCELLED));
        assert_eq!(run_over(&Err("no such file".into())), None);
        assert_eq!(run_over(&Ok(())), None);
    }

    #[test]
    fn the_consoles_own_save_copies_are_not_games() {
        assert!(!is_a_game_save("sce_backup2"));
        assert!(is_a_game_save("PPSA01650"));
        assert!(is_a_game_save("CUSA33878"));
    }

    #[test]
    fn a_manifest_comes_back_as_written_and_a_newer_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("cb-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = Manifest {
            format: FORMAT,
            complete: true,
            ..Manifest::default()
        };
        write_manifest(&dir, &m).unwrap();
        assert_eq!(read_manifest(&dir).unwrap(), m);
        let newer = Manifest {
            format: FORMAT + 1,
            ..Manifest::default()
        };
        write_manifest(&dir, &newer).unwrap();
        assert!(read_manifest(&dir).unwrap_err().contains("newer"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(read_manifest(&dir).unwrap_err().contains("isn't a backup"));
    }
}
