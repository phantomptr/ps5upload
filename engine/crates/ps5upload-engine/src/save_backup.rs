//! Automatic save backups, run by the engine: every few minutes, each saved console's saves that
//! changed since their last backup are copied into a folder the engine can write, a few versions
//! of each kept. Because the engine runs them, they carry on with no window open — in the Docker
//! image (always on) they run around the clock; in the desktop app, while the app runs.
//!
//! A backup is the same as the Saves screen's Backup button: download the save folder, tidy it
//! (`ps5upload_core::save_archive::backup_finalize`), zip it as `<title_id>/…`. The engine's own
//! routes do the console work (download, list, delete, upload) over loopback, exactly as the app
//! drives them, so there is one transfer path to trust.
//!
//! Restore (`POST /api/save-backup/restore`) puts one backup back: it unzips and checks the
//! backup first, backs up the save that is there now (so a restore can itself be undone), then
//! empties the save folder and uploads the backup into it.
//!
//! Settings, and what was last backed up, are kept in `state/` under the data folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::save_archive::{
    auto_backup_slot, backup_finalize, cleanup_temp_dir, make_temp_dir, restore_prepare,
    unzip_strict, zip_folder,
};
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How often the consoles are looked at.
pub const EVERY: Duration = Duration::from_secs(10 * 60);
/// A save written to in the last few minutes is probably still being written: wait.
pub const SETTLE_SECS: i64 = 180;
pub const DEFAULT_KEEP: u32 = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    /// A folder the engine can write (in Docker, one inside a mounted volume).
    #[serde(default)]
    pub dir: String,
    #[serde(default = "default_keep")]
    pub keep: u32,
    /// The consoles to back up (bare IPs), as the app's console list has them.
    #[serde(default)]
    pub hosts: Vec<String>,
}

fn default_keep() -> u32 {
    DEFAULT_KEEP
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            dir: String::new(),
            keep: DEFAULT_KEEP,
            hosts: Vec::new(),
        }
    }
}

pub fn clamp_keep(n: u32) -> u32 {
    n.clamp(1, 50)
}

/// What the last pass did, for the Saves screen.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub running: bool,
    /// Unix ms of the last finished pass, 0 when none.
    pub last_at: u64,
    pub backed_up: u32,
    pub failed: u32,
    pub last_error: Option<String>,
}

/// Console and save path → the save's modification time when it was last backed up.
pub type Done = BTreeMap<String, i64>;

pub(crate) fn done_key(host: &str, path: &str) -> String {
    format!("{host}|{path}")
}

/// The saves that need a backup: a game's (a PS5 game's or one in the Switch emulator, not the
/// console's own `sce_backup*` folders, which it lists among the saves, empty), changed since the
/// last one, and not written to in the last few minutes. A save never backed up counts as changed.
pub fn needing_backup<'a>(
    host: &str,
    saves: &'a [SaveEntry],
    done: &Done,
    now_secs: i64,
) -> Vec<&'a SaveEntry> {
    saves
        .iter()
        .filter(|e| {
            crate::emulator_saves::backs_up(e)
                && e.mtime > 0
                && now_secs - e.mtime >= SETTLE_SECS
                && e.mtime > done.get(&done_key(host, &e.path)).copied().unwrap_or(0)
        })
        .collect()
}

/// `YYYY-MM-DD_HHMMSS` in UTC: sorts in time order, and only plain characters.
pub fn stamp(ms: u64) -> String {
    let (y, m, d) = crate::event_journal::ymd_from_ms(ms);
    let secs = (ms / 1000) % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}{:02}{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── persistence ─────────────────────────────────────────────────────────

fn state_dir() -> Option<PathBuf> {
    let dir = match std::env::var("PS5UPLOAD_STATE_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ if cfg!(test) => return None,
        _ => crate::remote::store::data_dir()?.join("state"),
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub(crate) fn load<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    state_dir()
        .and_then(|d| std::fs::read(d.join(name)).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn save<T: Serialize>(name: &str, value: &T) -> std::io::Result<()> {
    let Some(dir) = state_dir() else {
        return Ok(());
    };
    let path = dir.join(name);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| crate::state_io::io_error("save backup settings", &path, e))
}

const SETTINGS_FILE: &str = "save-backup.json";
const DONE_FILE: &str = "save-backup-done.json";

struct Shared {
    settings: Settings,
    status: Status,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Shared {
            settings: load(SETTINGS_FILE),
            status: Status::default(),
        })
    })
}

pub(crate) fn settings() -> Settings {
    shared()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .settings
        .clone()
}

// ── the engine's own routes, over loopback ──────────────────────────────

static BASE: OnceLock<String> = OnceLock::new();

/// Starts the periodic pass. `bind` is the engine's listen address (its port is reached on
/// 127.0.0.1), `base_path` the optional URL prefix (`PS5UPLOAD_BASE_URL`).
pub fn spawn(bind: &str, base_path: &str) {
    let port = bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(19113);
    let base = base_path.trim_matches('/');
    let prefix = if base.is_empty() {
        String::new()
    } else {
        format!("/{base}")
    };
    let _ = BASE.set(format!("http://127.0.0.1:{port}{prefix}"));
    crate::ava1_api::spawn_periodic(EVERY, || {
        if let Err(e) = run_pass() {
            crate::log_warn!("save backup: {e}");
        }
    });
}

fn api(path: &str) -> Result<String, String> {
    BASE.get()
        .map(|b| format!("{b}{path}"))
        .ok_or_else(|| "save backup is not running".to_string())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn read_json(mut r: ureq::http::Response<ureq::Body>) -> Result<Value, String> {
    let status = r.status();
    let text = r.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        Ok(v)
    } else {
        Err(v
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(format!("HTTP {status}")))
    }
}

pub(crate) fn post(path: &str, body: Value) -> Result<Value, String> {
    read_json(
        agent()
            .post(&api(path)?)
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|e| e.to_string())?,
    )
}

pub(crate) fn get(path: &str) -> Result<Value, String> {
    read_json(agent().get(&api(path)?).call().map_err(|e| e.to_string())?)
}

pub(crate) fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Waits for a transfer job to finish.
pub(crate) fn wait_job(job_id: &str) -> Result<(), String> {
    loop {
        let v = get(&format!("/api/jobs/{job_id}"))?;
        match v.get("status").and_then(Value::as_str) {
            Some("done") => return Ok(()),
            Some("failed") => {
                return Err(v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("transfer failed")
                    .to_string())
            }
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// A transfer to or from `host` is running: a backup would only compete with it.
pub(crate) fn console_busy(host: &str) -> bool {
    let Ok(Value::Array(jobs)) = get("/api/jobs") else {
        return false;
    };
    jobs.iter().any(|j| {
        j.get("status").and_then(Value::as_str) == Some("running")
            && j.get("job")
                .map(|job| mentions_host(&job.to_string(), host))
                .unwrap_or(false)
    })
}

/// `text` names `host` as a whole address: `1.2.3.4` is not found in `1.2.3.40`.
pub fn mentions_host(text: &str, host: &str) -> bool {
    !host.is_empty()
        && text.match_indices(host).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + host.len()..].chars().next();
            let edge = |c: Option<char>| c.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '.'));
            edge(before) && edge(after)
        })
}

/// The console's saves, and the Switch emulator's (files of its own, which the console's list
/// leaves out).
pub(crate) fn list_saves(host: &str) -> Result<Vec<SaveEntry>, String> {
    let v = get(&format!("/api/ps5/saves/list?addr={}", enc(host)))?;
    let mut saves: Vec<SaveEntry> =
        serde_json::from_value(v.get("saves").cloned().unwrap_or(Value::Array(vec![])))
            .map_err(|e| format!("saves list: {e}"))?;
    saves.extend(crate::emulator_saves::list(host));
    Ok(saves)
}

// ── backup ──────────────────────────────────────────────────────────────

/// Backs up one save into `dir`; returns the zip's path. `tag` marks the kind (`snap`: taken
/// while the game ran; None: automatic), and each kind keeps its own `keep`. `verify` runs once
/// the copy is on this computer, before it is kept: an Err throws the copy away.
fn back_up_one(
    host: &str,
    entry: &SaveEntry,
    dir: &str,
    keep: u32,
    tag: Option<&str>,
    verify: &dyn Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    let user = entry.user_id as u32;
    let tmp = make_temp_dir(&entry.title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        download_save(host, entry, &tmp, verify)?;
        let st = match tag {
            Some(t) => format!("{}_{t}", stamp(now_ms())),
            None => stamp(now_ms()),
        };
        let dest = auto_backup_slot(dir, &entry.title_id, user, &st, keep, false)?;
        if let Err(e) = zip_folder(&tmp, &entry.title_id, &dest) {
            let _ = std::fs::remove_file(&dest);
            return Err(format!("zip: {e:#}"));
        }
        // Older backups go only once the new one is there, so a failed zip never costs one.
        auto_backup_slot(dir, &entry.title_id, user, &st, keep, true)?;
        Ok(dest)
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// Copies the save folder `entry` off `host` into `tmp/<title_id>` and tidies it for a backup
/// (`backup_finalize`). `verify` runs once the copy is here: an Err stops there.
pub(crate) fn download_save(
    host: &str,
    entry: &SaveEntry,
    tmp: &Path,
    verify: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let start = post(
        "/api/transfer/download",
        json!({ "addr": host, "src_path": entry.path, "dest_dir": tmp.to_string_lossy(), "kind": "folder" }),
    )?;
    let job = start
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or("download did not start")?;
    wait_job(job)?;
    verify()?;
    // The emulator's saves are plain files: kept as they are.
    if !crate::emulator_saves::is_emulator_save(entry) {
        backup_finalize(&tmp.join(&entry.title_id)).map_err(|e| format!("{e:#}"))?;
    }
    Ok(())
}

fn set_status(f: impl FnOnce(&mut Status)) {
    f(&mut shared().lock().unwrap_or_else(|e| e.into_inner()).status);
}

/// One pass over every console in the settings. Does nothing when the feature is off, no folder
/// is set, or a pass is already running.
pub fn run_pass() -> Result<(), String> {
    let s = settings();
    if !s.enabled || s.dir.trim().is_empty() {
        return Ok(());
    }
    {
        let mut g = shared().lock().unwrap_or_else(|e| e.into_inner());
        if g.status.running {
            return Ok(());
        }
        g.status.running = true;
    }
    let mut done: Done = load(DONE_FILE);
    let (mut backed_up, mut failed, mut last_error) = (0u32, 0u32, None::<String>);
    for host in &s.hosts {
        if console_busy(host) {
            continue;
        }
        // A console that does not answer (off, asleep, no helper) is simply not due now.
        let Ok(list) = list_saves(host) else { continue };
        let now = (now_ms() / 1000) as i64;
        for entry in needing_backup(host, &list, &done, now) {
            // The switch may be turned off part-way through a long pass.
            if !settings().enabled {
                break;
            }
            match back_up_one(host, entry, &s.dir, s.keep, None, &|| Ok(())) {
                Ok(_) => {
                    done.insert(done_key(host, &entry.path), entry.mtime);
                    let _ = save(DONE_FILE, &done);
                    backed_up += 1;
                }
                Err(e) => {
                    crate::log_warn!("save backup: {} on {host}: {e}", entry.title_id);
                    last_error = Some(format!("{}: {e}", entry.title_id));
                    failed += 1;
                }
            }
        }
    }
    if backed_up > 0 || failed > 0 {
        crate::log_info!(
            "save backup: {backed_up} saved, {failed} failed ({})",
            s.dir
        );
    }
    set_status(|st| {
        st.running = false;
        st.last_at = now_ms();
        st.backed_up = backed_up;
        st.failed = failed;
        st.last_error = last_error;
    });
    Ok(())
}

// ── snapshots (while the game runs) ─────────────────────────────────────

/// Whether `title_id` runs on `host` (an app process carrying it).
pub(crate) fn title_running(host: &str, title_id: &str) -> Result<bool, String> {
    let list = ps5upload_core::process_mgr::process_list(host)
        .map_err(|e| format!("could not check the running game: {e:#}"))?;
    Ok(list
        .processes
        .iter()
        .any(|p| p.kind == "app" && p.title_id.eq_ignore_ascii_case(title_id)))
}

/// Every file of a save folder (two levels, as the saves list measures it) with its size and
/// time, sorted: two equal fingerprints mean nothing was written between them. The console's
/// own times, compared with each other only, so the two machines' clocks never matter.
fn fingerprint(host: &str, path: &str) -> Result<Vec<(String, u64, i64)>, String> {
    let mut out = Vec::new();
    let list = |p: &str| -> Result<Vec<Value>, String> {
        let v = get(&format!(
            "/api/ps5/list-dir?addr={}&path={}&offset=0&limit=512",
            enc(host),
            enc(p)
        ))?;
        Ok(v.get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    };
    for e in list(path)? {
        let name = e
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let size = e.get("size").and_then(Value::as_u64).unwrap_or(0);
        let mtime = e.get("mtime").and_then(Value::as_i64).unwrap_or(0);
        if e.get("kind").and_then(Value::as_str) == Some("dir") {
            for c in list(&format!("{path}/{name}"))? {
                let cn = c.get("name").and_then(Value::as_str).unwrap_or_default();
                if cn.is_empty() || cn == "." || cn == ".." {
                    continue;
                }
                out.push((
                    format!("{name}/{cn}"),
                    c.get("size").and_then(Value::as_u64).unwrap_or(0),
                    c.get("mtime").and_then(Value::as_i64).unwrap_or(0),
                ));
            }
        } else {
            out.push((name, size, mtime));
        }
    }
    out.sort();
    Ok(out)
}

/// How long a save must stay unwritten before a snapshot copies it, and how often to try.
const SNAP_QUIET: Duration = Duration::from_secs(3);
const SNAP_TRIES: u32 = 3;
pub const SNAP_TAG: &str = "snap";

/// Back up the saves of `title_id` on `host` now, while its game may run: each copy is taken
/// only while the save sat still, and kept only if it still had not changed once copied (a game
/// saving mid-copy would leave a torn backup). Tagged `snap`, kept apart from the automatic ones.
pub fn snapshot(host: &str, title_id: &str) -> Result<Vec<PathBuf>, String> {
    let s = settings();
    if s.dir.trim().is_empty() {
        return Err("Choose a folder for save backups first (Saves › Automatic backups).".into());
    }
    let saves: Vec<SaveEntry> = list_saves(host)?
        .into_iter()
        .filter(|e| e.title_id.eq_ignore_ascii_case(title_id))
        .collect();
    if saves.is_empty() {
        return Err(format!("{title_id} has no save on this PS5 yet."));
    }
    let mut made = Vec::new();
    for entry in &saves {
        let mut last = String::new();
        let mut done = false;
        for _ in 0..SNAP_TRIES {
            let before = fingerprint(host, &entry.path)?;
            std::thread::sleep(SNAP_QUIET);
            if fingerprint(host, &entry.path)? != before {
                last = "the game was saving".into();
                continue;
            }
            let check = || match fingerprint(host, &entry.path) {
                Ok(after) if after == before => Ok(()),
                Ok(_) => Err("the game saved while it was copied".to_string()),
                Err(e) => Err(e),
            };
            match back_up_one(host, entry, &s.dir, s.keep, Some(SNAP_TAG), &check) {
                Ok(p) => {
                    made.push(p);
                    done = true;
                    break;
                }
                Err(e) => last = e,
            }
        }
        if !done {
            return Err(format!(
                "Could not take a clean copy of {title_id}'s save ({last}). Try again in a moment."
            ));
        }
    }
    crate::log_info!(
        "save backup: snapshot of {title_id} on {host} ({} file(s))",
        made.len()
    );
    Ok(made)
}

#[derive(Deserialize)]
pub struct SnapshotBody {
    pub addr: String,
    pub title_id: String,
}

/// `POST /api/save-backup/snapshot` — back up a game's save now, while it may be running.
pub async fn post_snapshot(Json(b): Json<SnapshotBody>) -> Response {
    let title = b.title_id.trim().to_ascii_uppercase();
    if !plain(&title) {
        return json_error(StatusCode::BAD_REQUEST, "unexpected title id");
    }
    let host = b.addr.trim().to_string();
    match tokio::task::spawn_blocking(move || snapshot(&host, &title)).await {
        Ok(Ok(files)) => Json(json!({
            "ok": true,
            "files": files.iter().filter_map(|p| p.file_name()?.to_str().map(String::from)).collect::<Vec<_>>(),
        }))
        .into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── listing and restore ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackupFile {
    /// File name inside `<dir>/<title_id>/`.
    pub file: String,
    pub title_id: String,
    pub user_id: u32,
    /// `YYYY-MM-DD_HHMMSS` (UTC for backups the engine made), `_snap` after it for a snapshot.
    pub stamp: String,
    pub size: u64,
    /// `snap`: taken while the game ran. None: an automatic (or before-restore) backup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Parses `<title>_user<8 hex>_<stamp>.zip`.
pub fn parse_backup_name(title_id: &str, file: &str) -> Option<(u32, String)> {
    let rest = file.strip_prefix(title_id)?.strip_prefix("_user")?;
    let (hex, rest) = rest.split_at_checked(8)?;
    let user = u32::from_str_radix(hex, 16).ok()?;
    let stamp = rest.strip_prefix('_')?.strip_suffix(".zip")?;
    (!stamp.is_empty()).then(|| (user, stamp.to_string()))
}

pub(crate) fn plain(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !s.contains("..")
}

/// The backups of `title_id` in `dir`, newest first.
pub fn list_backups(dir: &str, title_id: &str) -> Vec<BackupFile> {
    if !plain(title_id) || dir.trim().is_empty() {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(Path::new(dir).join(title_id)) else {
        return Vec::new();
    };
    let mut out: Vec<BackupFile> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            let (user_id, stamp) = parse_backup_name(title_id, &file)?;
            let size = e.metadata().ok()?.len();
            Some(BackupFile {
                tag: ps5upload_core::save_archive::backup_tag(&file).map(String::from),
                file,
                title_id: title_id.to_string(),
                user_id,
                stamp,
                size,
            })
        })
        .collect();
    out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    out
}

/// Puts the backup `file` of `title_id` back into the save folder `save_path` on `host`.
fn restore(
    host: &str,
    title_id: &str,
    file: &str,
    save_path: &str,
    user_id: u32,
) -> Result<(), String> {
    let s = settings();
    if !plain(title_id) || !plain(file) || !file.ends_with(".zip") {
        return Err("unexpected backup name".into());
    }
    let zip = Path::new(&s.dir).join(title_id).join(file);
    if !zip.is_file() {
        return Err(format!("no backup {}", zip.display()));
    }
    restore_zip(host, title_id, &zip, save_path, user_id)
}

/// Puts the backup zip `zip` (on this computer) of `title_id` back into the save folder
/// `save_path` on `host`: checked and unpacked first, the save there now backed up, then
/// replaced (see `put_in_place`).
pub(crate) fn restore_zip(
    host: &str,
    title_id: &str,
    zip: &Path,
    save_path: &str,
    user_id: u32,
) -> Result<(), String> {
    let s = settings();
    if !save_path.ends_with(&format!("/{title_id}")) {
        return Err(format!("{save_path} is not the save folder of {title_id}"));
    }
    // A save of the Switch emulator is written by the emulator, and its files go back as they are.
    let emulator = crate::emulator_saves::is_save_path(save_path);
    let app = if emulator {
        crate::emulator_saves::APP
    } else {
        title_id
    };
    // The game writes its save while it runs: one put back underneath it is overwritten, or
    // worse, mixed with what the game writes next.
    if title_running(host, app)? {
        return Err(format!(
            "{app} is running on the PS5. Close the game, then restore."
        ));
    }
    let tmp = make_temp_dir(title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        // Check and unpack the backup before anything on the console changes.
        unzip_strict(zip, &tmp, title_id).map_err(|e| format!("unzip: {e:#}"))?;
        if !emulator {
            restore_prepare(&tmp.join(title_id)).map_err(|e| format!("prepare: {e:#}"))?;
        }
        put_in_place(
            host,
            title_id,
            &tmp.join(title_id),
            save_path,
            user_id,
            &s,
            None,
        )
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// The most entries one `list-dir` page carries (the payload's own ceiling).
const LIST_PAGE: u64 = 256;

/// Every entry of a folder, page by page: `page(offset)` is one `list-dir` answer. A full page
/// (or one cut short because its buffer filled) means there may be more; a short or empty one is
/// the end.
pub(crate) fn all_entries(
    page: &dyn Fn(u64) -> Result<Value, String>,
) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    loop {
        let v = page(out.len() as u64)?;
        let entries = v
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let truncated = v.get("truncated").and_then(Value::as_bool).unwrap_or(false);
        let n = entries.len() as u64;
        out.extend(entries);
        if n == 0 || (!truncated && n < LIST_PAGE) {
            return Ok(out);
        }
    }
}

/// Replace what is in the console's save folder `save_path` with the files of the local folder
/// `src`: the save there now is backed up first (tagged `tag`, so it can be put back), then the
/// folder is emptied (kept itself: the console refuses a fresh mkdir under its save root) and
/// `src`'s files are uploaded into it.
fn put_in_place(
    host: &str,
    title_id: &str,
    src: &Path,
    save_path: &str,
    user_id: u32,
    s: &Settings,
    tag: Option<&str>,
) -> Result<(), String> {
    // Keep what is there now, so the change can be undone: in the backup folder, or (with none
    // set) on the server the save backups go to.
    let current = SaveEntry {
        title_id: title_id.to_string(),
        user_id: user_id as i32,
        path: save_path.to_string(),
        size: 0,
        mtime: 0,
        kind: if crate::emulator_saves::is_save_path(save_path) {
            crate::emulator_saves::KIND.into()
        } else {
            String::new()
        },
    };
    if !s.dir.trim().is_empty() {
        back_up_one(host, &current, &s.dir, s.keep, tag, &|| Ok(()))
            .map_err(|e| format!("could not back up the current save first: {e}"))?;
    } else if crate::save_backup_server::configured() {
        crate::save_backup_server::back_up_before_restore(host, &current)
            .map_err(|e| format!("could not back up the current save first: {e}"))?;
    }
    // Every name first, then the deletes: a page asked for after a delete would skip some.
    let names = all_entries(&|offset| {
        get(&format!(
            "/api/ps5/list-dir?addr={}&path={}&offset={offset}&limit={LIST_PAGE}",
            enc(host),
            enc(save_path)
        ))
    })?;
    for e in names {
        let Some(name) = e.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name == "." || name == ".." {
            continue;
        }
        post(
            "/api/ps5/fs/delete",
            json!({ "addr": host, "path": format!("{save_path}/{name}") }),
        )?;
    }
    let start = post(
        "/api/transfer/dir",
        json!({
            "addr": host,
            "src_dir": src.to_string_lossy(),
            "dest_root": save_path,
            "excludes": [],
        }),
    )?;
    let job = start
        .get("job_id")
        .and_then(Value::as_str)
        .ok_or("upload did not start")?;
    wait_job(job)
}

// ── PS4 saves onto the PS5 ──────────────────────────────────────────────

/// The tag of the backup taken of a PS5 save before a PS4 save replaced it.
pub const BEFORE_PS4_TAG: &str = "beforeps";

/// A PS4 save folder as the PS4 keeps it: one or more `sdimg_<name>` images, each with its
/// sealed key `<name>.bin` beside it, and nothing that is a folder. Err says what is off.
pub fn check_ps4_save_folder(names: &[(String, bool)]) -> Result<(), String> {
    if names.iter().any(|(_, dir)| *dir) {
        return Err("it holds a folder, and a PS4 save folder holds only files".into());
    }
    let images: Vec<&str> = names
        .iter()
        .filter_map(|(n, _)| n.strip_prefix("sdimg_"))
        .collect();
    if images.is_empty() {
        return Err("it has no save image (sdimg_…)".into());
    }
    for img in &images {
        let key = format!("{img}.bin");
        if !names.iter().any(|(n, _)| *n == key) {
            return Err(format!("the save image sdimg_{img} has no key ({key})"));
        }
    }
    Ok(())
}

/// Whether `save_path` is a PS4 save folder of `title_id` on the console
/// (`/user/home/<8 hex>/savedata/<TITLE>`, not a PS5 one under savedata_prospero).
pub fn is_ps4_save_path(save_path: &str, title_id: &str) -> bool {
    let parts: Vec<&str> = save_path.trim_end_matches('/').split('/').collect();
    matches!(
        parts.as_slice(),
        ["", "user", "home", uid, "savedata", t]
            if uid.len() == 8 && uid.chars().all(|c| c.is_ascii_hexdigit()) && *t == title_id
    )
}

/// `src` as a real path inside the save-backup folder `root` (where the wizard copies the PS4's
/// files), or why not. The engine's API has no sign-in, so a request must not be able to name any
/// other folder on this computer and have its files copied onto the PS5. Both are resolved
/// first (`..`, symlinks), so neither can step outside.
pub fn src_in_backups(root: &str, src: &str) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root.trim())
        .map_err(|e| format!("the save backup folder {root}: {e}"))?;
    let real = std::fs::canonicalize(src.trim()).map_err(|e| format!("{src}: {e}"))?;
    if real == root || !real.starts_with(&root) {
        return Err(format!(
            "{src} is not inside the save backup folder ({})",
            root.display()
        ));
    }
    Ok(real)
}

/// Put a PS4 save (the files a PS4 keeps for `title_id`, copied to the local folder `src`) into
/// the PS5 user's PS4 save folder of that game. Only into a folder the console already made
/// (the game was played and saved there once), never while the game runs, and only after the
/// save there now is backed up.
fn import_ps4(
    host: &str,
    title_id: &str,
    src: &str,
    save_path: &str,
    user_id: u32,
) -> Result<(), String> {
    let s = settings();
    if s.dir.trim().is_empty() {
        return Err("Choose a folder for save backups first (Saves › Automatic backups): the PS5's own save is kept there before it is replaced.".into());
    }
    let src = src_in_backups(&s.dir, src)?;
    let src = src.as_path();
    if !plain(title_id) || !is_ps4_save_path(save_path, title_id) {
        return Err(format!(
            "{save_path} is not a PS4 save folder of {title_id} on the PS5"
        ));
    }
    let names: Vec<(String, bool)> = std::fs::read_dir(src)
        .map_err(|e| format!("read {}: {e}", src.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let dir = e.file_type().ok()?.is_dir();
            Some((e.file_name().into_string().ok()?, dir))
        })
        .collect();
    check_ps4_save_folder(&names)
        .map_err(|e| format!("{} does not look like a PS4 save: {e}", src.display()))?;
    if title_running(host, title_id)? {
        return Err(format!(
            "{title_id} is running on the PS5. Close the game, then try again."
        ));
    }
    // The folder must be there already: the console refuses to make one under its save root.
    let there = get(&format!(
        "/api/ps5/list-dir?addr={}&path={}&offset=0&limit=1",
        enc(host),
        enc(save_path)
    ));
    if there.is_err() {
        return Err(format!("{title_id} has no save on this PS5 user yet: start the game on the PS5 once and save, then try again."));
    }
    put_in_place(
        host,
        title_id,
        src,
        save_path,
        user_id,
        &s,
        Some(BEFORE_PS4_TAG),
    )?;
    crate::log_info!("save backup: PS4 save of {title_id} put on {host} ({save_path})");
    Ok(())
}

#[derive(Deserialize)]
pub struct ImportPs4Body {
    pub addr: String,
    pub title_id: String,
    /// The PS4 save folder of the game on the PS5 (from the saves list, kind "ps4").
    pub save_path: String,
    pub user_id: u32,
    /// The local folder with the PS4's files of that game.
    pub src_dir: String,
}

/// `POST /api/save-backup/import-ps4` — put a PS4 save onto the PS5 (see `import_ps4`).
pub async fn post_import_ps4(Json(b): Json<ImportPs4Body>) -> Response {
    let title = b.title_id.trim().to_ascii_uppercase();
    let r = tokio::task::spawn_blocking(move || {
        import_ps4(
            b.addr.trim(),
            &title,
            &b.src_dir,
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

// ── Sharing a save with another account ─────────────────────────────────
//
// A PS5 save is encrypted for the account and console that made it, so a friend's save copied
// over as it is (even with the account ID changed to theirs) does not load. What does carry over
// is what is inside it: the game's own files. So a save is shared as those files (opened
// decrypted on the friend's console, its sce_sys left out: that is the owner's account
// information), and used by opening your own save of the same game, putting those files in it
// and closing it, which the console encrypts again as yours. Your save is backed up first and
// put back if anything fails. PS5 saves only.

/// The tag of the backup taken of a save before a shared one replaced its files.
pub const BEFORE_SHARE_TAG: &str = "beforeshare";
/// What a shared save says about itself, inside its one folder.
pub const SHARE_MANIFEST: &str = ".ps5upload-share.json";
/// A save's own system folder (its owner's account and the console's view of it): kept.
const SYS_DIR: &str = "sce_sys";

/// A PS5 save image, `/user/home/<8 hex>/savedata_prospero/<TITLE>/<name>`: (title, name).
pub fn ps5_save_image(image: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = image.split('/').collect();
    match parts.as_slice() {
        ["", "user", "home", uid, "savedata_prospero", title, name]
            if uid.len() == 8
                && uid.chars().all(|c| c.is_ascii_hexdigit())
                && plain(title)
                && !name.is_empty()
                && !name.starts_with('.')
                && !name.ends_with(".bin") =>
        {
            Some((title.to_string(), name.to_string()))
        }
        _ => None,
    }
}

/// The save folder holding `image`.
fn folder_of(image: &str) -> &str {
    image.rsplit_once('/').map(|(d, _)| d).unwrap_or(image)
}

/// Opens `image` decrypted on the console; its mount point.
fn open_save(host: &str, image: &str) -> Result<String, String> {
    let v = post(
        "/api/ps5/saves/open",
        json!({ "addr": host, "image": image }),
    )?;
    match (
        v.get("mounted").and_then(Value::as_bool),
        v.get("mount_point").and_then(Value::as_str),
    ) {
        (Some(true), Some(mp)) if !mp.is_empty() => Ok(mp.to_string()),
        _ => Err("the save did not open".into()),
    }
}

/// Closes the open save (written back when it changed).
fn close_save(host: &str) -> Result<bool, String> {
    let v = post("/api/ps5/saves/close", json!({ "addr": host }))?;
    Ok(v.get("written_back")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

/// A save is already open on the console (one at a time).
fn a_save_is_open(host: &str) -> bool {
    post("/api/ps5/saves/open", json!({ "addr": host, "image": "" }))
        .ok()
        .and_then(|v| v.get("mounted").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// The files of the save `image`, as a zip to give to someone else: one folder named after the
/// game with the game's files and a small manifest, the save's sce_sys left out.
pub fn share_save(host: &str, image: &str) -> Result<(String, Vec<u8>), String> {
    let (title_id, name) =
        ps5_save_image(image).ok_or_else(|| format!("{image} is not a PS5 save image"))?;
    if title_running(host, &title_id)? {
        return Err(format!(
            "{title_id} is running on the PS5. Close the game, then share its save."
        ));
    }
    if a_save_is_open(host) {
        return Err("A save is open in Save Data. Close it first.".into());
    }
    let tmp = make_temp_dir(&title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let mount = open_save(host, image)?;
        let copied = (|| {
            let start = post(
                "/api/transfer/download",
                json!({ "addr": host, "src_path": mount, "dest_dir": tmp_s, "kind": "folder" }),
            )?;
            let job = start
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or("download did not start")?;
            wait_job(job)
        })();
        // Read only: closing it changes nothing on the console.
        let closed = close_save(host);
        copied?;
        closed?;
        let leaf = mount.rsplit('/').next().unwrap_or("save_mount");
        let got = tmp.join(leaf);
        let dir = tmp.join(&title_id);
        std::fs::rename(&got, &dir).map_err(|e| format!("copy: {e}"))?;
        let _ = std::fs::remove_dir_all(dir.join(SYS_DIR));
        let manifest = json!({
            "kind": "ps5",
            "title_id": title_id,
            "image": name,
            "shared_at_ms": now_ms(),
            "app": env!("CARGO_PKG_VERSION"),
        });
        std::fs::write(dir.join(SHARE_MANIFEST), manifest.to_string())
            .map_err(|e| format!("manifest: {e}"))?;
        let zip = tmp.join("share.zip");
        zip_folder(&tmp, &title_id, &zip).map_err(|e| format!("zip: {e:#}"))?;
        let bytes = std::fs::read(&zip).map_err(|e| format!("zip: {e}"))?;
        let file = format!("{title_id} {name} shared {}.zip", stamp(now_ms()));
        Ok((file, bytes))
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// What a shared save holds, checked before anything on the console changes: the folder of
/// `title_id`'s files, its sce_sys and manifest removed. Err says why it can't be used.
pub fn shared_save_contents(unpacked: &Path, title_id: &str) -> Result<PathBuf, String> {
    let dir = unpacked.join(title_id);
    let manifest: Value = std::fs::read(dir.join(SHARE_MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(
            "this is not a shared save (no manifest): share it from Save Data on the other console",
        )?;
    if manifest.get("title_id").and_then(Value::as_str) != Some(title_id) {
        return Err(format!("this shared save is not for {title_id}"));
    }
    if manifest.get("kind").and_then(Value::as_str) != Some("ps5") {
        return Err("only PS5 saves can be shared".into());
    }
    let _ = std::fs::remove_file(dir.join(SHARE_MANIFEST));
    let _ = std::fs::remove_dir_all(dir.join(SYS_DIR));
    let any = std::fs::read_dir(&dir)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if !any {
        return Err("the shared save has no files".into());
    }
    Ok(dir)
}

/// Put a shared save's files into your own save `image` of the same game, which the console then
/// keeps as yours. Your save is backed up first (tagged `beforeshare`) and put back if a step
/// fails.
pub fn use_shared_save(
    host: &str,
    image: &str,
    user_id: u32,
    zip_bytes: &[u8],
) -> Result<(), String> {
    let s = settings();
    if s.dir.trim().is_empty() {
        return Err("Choose a folder for save backups first (Save Data › Automatic backups): your own save is kept there before its files are replaced.".into());
    }
    let (title_id, _) =
        ps5_save_image(image).ok_or_else(|| format!("{image} is not a PS5 save image"))?;
    let tmp = make_temp_dir(&title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let zip = tmp.join("shared.zip");
        std::fs::write(&zip, zip_bytes).map_err(|e| format!("save the file: {e}"))?;
        let unpacked = tmp.join("x");
        std::fs::create_dir_all(&unpacked).map_err(|e| format!("unpack: {e}"))?;
        unzip_strict(&zip, &unpacked, &title_id)
            .map_err(|e| format!("this file is not a shared save of {title_id}: {e:#}"))?;
        let src = shared_save_contents(&unpacked, &title_id)?;
        if title_running(host, &title_id)? {
            return Err(format!(
                "{title_id} is running on the PS5. Close the game, then try again."
            ));
        }
        if a_save_is_open(host) {
            return Err("A save is open in Save Data. Close it first.".into());
        }
        // Yours first, so this can be undone.
        let folder = folder_of(image).to_string();
        let mine = SaveEntry {
            title_id: title_id.clone(),
            user_id: user_id as i32,
            path: folder.clone(),
            size: 0,
            mtime: 0,
            kind: "ps5".into(),
        };
        let backup = back_up_one(host, &mine, &s.dir, s.keep, Some(BEFORE_SHARE_TAG), &|| {
            Ok(())
        })
        .map_err(|e| format!("could not back up your save first: {e}"))?;
        let mount = open_save(host, image)?;
        let replaced = (|| {
            let listing = get(&format!(
                "/api/ps5/list-dir?addr={}&path={}&offset=0&limit=4096",
                enc(host),
                enc(&mount)
            ))?;
            for e in listing
                .get("entries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let Some(name) = e.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if name == "." || name == ".." || name == SYS_DIR {
                    continue;
                }
                post(
                    "/api/ps5/fs/delete",
                    json!({ "addr": host, "path": format!("{mount}/{name}") }),
                )?;
            }
            let start = post(
                "/api/transfer/dir",
                json!({ "addr": host, "src_dir": src.to_string_lossy(), "dest_root": mount, "excludes": [] }),
            )?;
            let job = start
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or("upload did not start")?;
            wait_job(job)
        })();
        let closed = close_save(host);
        match (replaced, closed) {
            (Ok(()), Ok(_)) => {
                crate::log_info!("save backup: shared save of {title_id} put on {host} ({image})");
                Ok(())
            }
            (r, c) => {
                let why = r.err().or_else(|| c.err()).unwrap_or_default();
                let file = backup
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match restore(host, &title_id, &file, &folder, user_id) {
                    Ok(()) => Err(format!("{why}. Your own save was put back.")),
                    Err(e) => Err(format!(
                        "{why}. Putting your save back failed too ({e}); restore {file} from Save Data."
                    )),
                }
            }
        }
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

#[derive(Deserialize)]
pub struct ShareBody {
    pub addr: String,
    pub image: String,
}

/// `POST /api/save-backup/share` `{addr, image}`: the save's files as a zip (see `share_save`).
pub async fn post_share(Json(b): Json<ShareBody>) -> Response {
    let r = tokio::task::spawn_blocking(move || share_save(b.addr.trim(), b.image.trim())).await;
    match r {
        Ok(Ok((file, bytes))) => (
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    "application/zip".to_string(),
                ),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", file.replace('"', "")),
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct UseSharedQuery {
    pub addr: String,
    pub image: String,
    pub user_id: u32,
}

/// `POST /api/save-backup/use-shared?addr=&image=&user_id=` with the shared zip as the body.
pub async fn post_use_shared(Query(q): Query<UseSharedQuery>, body: axum::body::Bytes) -> Response {
    let r = tokio::task::spawn_blocking(move || {
        use_shared_save(q.addr.trim(), q.image.trim(), q.user_id, &body)
    })
    .await;
    match r {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── HTTP ────────────────────────────────────────────────────────────────

pub(crate) fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn view() -> Value {
    let g = shared().lock().unwrap_or_else(|e| e.into_inner());
    json!({ "settings": g.settings, "status": g.status })
}

/// `GET /api/save-backup` — `{settings, status}`.
pub async fn get_view() -> Response {
    Json(view()).into_response()
}

#[derive(Deserialize)]
pub struct SettingsPatch {
    pub enabled: Option<bool>,
    pub dir: Option<String>,
    pub keep: Option<u32>,
    pub hosts: Option<Vec<String>>,
}

/// Whether the engine can write `dir` (made if missing).
fn writable(dir: &str) -> Result<(), String> {
    let p = Path::new(dir);
    if !p.is_absolute() {
        return Err(format!("{dir} is not a full path"));
    }
    std::fs::create_dir_all(p)
        .map_err(|e| crate::state_io::describe("the backup folder", p, &e))?;
    let probe = p.join(".ps5upload-write-test");
    std::fs::write(&probe, b"ok")
        .map_err(|e| crate::state_io::describe("the backup folder", p, &e))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// `PUT /api/save-backup` — change any of `{enabled, dir, keep, hosts}`.
pub async fn put_settings(Json(patch): Json<SettingsPatch>) -> Response {
    let mut next = settings();
    if let Some(d) = patch.dir {
        next.dir = d.trim().to_string();
    }
    if let Some(e) = patch.enabled {
        next.enabled = e;
    }
    if let Some(k) = patch.keep {
        next.keep = clamp_keep(k);
    }
    if let Some(h) = patch.hosts {
        let mut h: Vec<String> = h
            .into_iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        h.sort();
        h.dedup();
        next.hosts = h;
    }
    if next.enabled && next.dir.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "choose a folder for the backups first",
        );
    }
    if !next.dir.is_empty() {
        let dir = next.dir.clone();
        match tokio::task::spawn_blocking(move || writable(&dir)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return json_error(StatusCode::BAD_REQUEST, e),
            Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    }
    shared().lock().unwrap_or_else(|e| e.into_inner()).settings = next.clone();
    if let Err(e) = save(SETTINGS_FILE, &next) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(view()).into_response()
}

/// `POST /api/save-backup/run` — a pass now (in the background).
pub async fn post_run() -> Response {
    tokio::task::spawn_blocking(|| {
        if let Err(e) = run_pass() {
            crate::log_warn!("save backup: {e}");
        }
    });
    (StatusCode::ACCEPTED, Json(view())).into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub title_id: String,
}

/// `GET /api/save-backup/list?title_id=` — that game's backups, newest first.
pub async fn get_list(Query(q): Query<ListQuery>) -> Response {
    let dir = settings().dir;
    let title = q.title_id.trim().to_ascii_uppercase();
    match tokio::task::spawn_blocking(move || list_backups(&dir, &title)).await {
        Ok(list) => Json(json!({ "backups": list })).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct RestoreBody {
    /// The console (bare IP).
    pub addr: String,
    pub title_id: String,
    pub file: String,
    /// The save folder on the console (from the saves list).
    pub save_path: String,
    pub user_id: u32,
}

/// `POST /api/save-backup/restore` — puts one backup back. The game must not be running.
pub async fn post_restore(Json(b): Json<RestoreBody>) -> Response {
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

    /// Sharing works on PS5 save images only, named as the console keeps them.
    #[test]
    fn a_shared_save_is_a_ps5_save_image() {
        assert_eq!(
            ps5_save_image("/user/home/1a2b3c4d/savedata_prospero/PPSA01234/sdimg_SAVEDATA00"),
            Some(("PPSA01234".into(), "sdimg_SAVEDATA00".into()))
        );
        // PS4 saves, keys, folders, odd users and paths that step out are not.
        assert_eq!(
            ps5_save_image("/user/home/1a2b3c4d/savedata/CUSA01234/sdimg_SAVEDATA00"),
            None
        );
        assert_eq!(
            ps5_save_image("/user/home/1a2b3c4d/savedata_prospero/PPSA01234/SAVEDATA00.bin"),
            None
        );
        assert_eq!(
            ps5_save_image("/user/home/1a2b3c4d/savedata_prospero/PPSA01234"),
            None
        );
        assert_eq!(
            ps5_save_image("/user/home/xyz/savedata_prospero/PPSA01234/a"),
            None
        );
        assert_eq!(
            ps5_save_image("/user/home/1a2b3c4d/savedata_prospero/../x/a"),
            None
        );
        assert_eq!(
            folder_of("/user/home/1a2b3c4d/savedata_prospero/PPSA01234/a"),
            "/user/home/1a2b3c4d/savedata_prospero/PPSA01234"
        );
    }

    /// A shared save is checked before the console is touched: its manifest, its game, and that
    /// it carries files; the sender's sce_sys never goes in.
    #[test]
    fn a_shared_save_is_checked_and_its_sce_sys_left_out() {
        let base = std::env::temp_dir().join(format!("ps5u-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("PPSA01234");
        std::fs::create_dir_all(dir.join(SYS_DIR)).unwrap();
        std::fs::write(dir.join(SYS_DIR).join("param.sfo"), b"owner").unwrap();
        std::fs::write(dir.join("savedata.dat"), b"progress").unwrap();
        // No manifest: not a shared save.
        assert!(shared_save_contents(&base, "PPSA01234").is_err());
        let manifest = |t: &str, k: &str| json!({ "kind": k, "title_id": t }).to_string();
        std::fs::write(dir.join(SHARE_MANIFEST), manifest("PPSA09999", "ps5")).unwrap();
        assert!(shared_save_contents(&base, "PPSA01234")
            .unwrap_err()
            .contains("not for PPSA01234"));
        std::fs::write(dir.join(SHARE_MANIFEST), manifest("PPSA01234", "ps4")).unwrap();
        assert!(shared_save_contents(&base, "PPSA01234").is_err());
        std::fs::write(dir.join(SHARE_MANIFEST), manifest("PPSA01234", "ps5")).unwrap();
        let src = shared_save_contents(&base, "PPSA01234").unwrap();
        assert!(src.join("savedata.dat").is_file());
        assert!(!src.join(SYS_DIR).exists());
        assert!(!src.join(SHARE_MANIFEST).exists());
        // Only a manifest and a sce_sys: nothing to put in.
        std::fs::remove_file(src.join("savedata.dat")).unwrap();
        std::fs::write(dir.join(SHARE_MANIFEST), manifest("PPSA01234", "ps5")).unwrap();
        assert!(shared_save_contents(&base, "PPSA01234")
            .unwrap_err()
            .contains("no files"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The import takes only a folder inside the save backup folder (code scanning alert #234:
    /// the API has no sign-in, so a path from a request must not reach the rest of the disk).
    #[test]
    fn a_ps4_import_reads_only_inside_the_backup_folder() {
        let base = std::env::temp_dir().join(format!("ps5u-src-{}", std::process::id()));
        let root = base.join("backups");
        let inside = root
            .join("PS4 10.0.0.2 1 2026-10-30 1200")
            .join("CUSA00001");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = base.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let r = root.to_str().unwrap();
        assert_eq!(
            src_in_backups(r, inside.to_str().unwrap()).unwrap(),
            std::fs::canonicalize(&inside).unwrap()
        );
        assert!(src_in_backups(r, outside.to_str().unwrap()).is_err());
        assert!(src_in_backups(r, r).is_err());
        // Stepping out with .. is caught once resolved.
        let dotdot = format!("{}/../elsewhere", r);
        assert!(src_in_backups(r, &dotdot).is_err());
        // A link inside that points out is caught too.
        #[cfg(unix)]
        {
            let link = root.join("link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(src_in_backups(r, link.to_str().unwrap()).is_err());
        }
        assert!(src_in_backups(r, "/no/such/folder").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn only_a_ps4_save_folder_is_put_on_the_ps5() {
        let f = |v: &[(&str, bool)]| {
            v.iter()
                .map(|(n, d)| (n.to_string(), *d))
                .collect::<Vec<_>>()
        };
        assert!(check_ps4_save_folder(&f(&[
            ("sdimg_SAVEDATA00", false),
            ("SAVEDATA00.bin", false)
        ]))
        .is_ok());
        // An image without its key, no image, or a folder inside: refused.
        assert!(check_ps4_save_folder(&f(&[("sdimg_SAVEDATA00", false)])).is_err());
        assert!(check_ps4_save_folder(&f(&[("notes.txt", false)])).is_err());
        assert!(check_ps4_save_folder(&f(&[
            ("sdimg_A", false),
            ("A.bin", false),
            ("sce_sys", true)
        ]))
        .is_err());
    }

    #[test]
    fn only_a_ps4_save_path_of_that_game() {
        assert!(is_ps4_save_path(
            "/user/home/1de82450/savedata/CUSA00001",
            "CUSA00001"
        ));
        assert!(!is_ps4_save_path(
            "/user/home/1de82450/savedata_prospero/CUSA00001",
            "CUSA00001"
        ));
        assert!(!is_ps4_save_path(
            "/user/home/1de82450/savedata/CUSA00002",
            "CUSA00001"
        ));
        assert!(!is_ps4_save_path(
            "/user/home/xyz/savedata/CUSA00001",
            "CUSA00001"
        ));
        assert!(!is_ps4_save_path(
            "/system_data/savedata/CUSA00001",
            "CUSA00001"
        ));
    }
    use ps5upload_core::save_archive::auto_backup_stem;

    fn save(path: &str, mtime: i64) -> SaveEntry {
        SaveEntry {
            title_id: "PPSA01234".into(),
            user_id: 0x1234_5678,
            path: path.into(),
            size: 1,
            mtime,
            kind: "ps5".into(),
        }
    }

    #[test]
    fn only_settled_changed_saves_are_due() {
        let saves = [
            save("/a", 1000),
            save("/b", 1000),
            save("/c", 1900),
            save("/d", 0),
        ];
        let mut done = Done::new();
        done.insert(done_key("h", "/a"), 1000); // already backed up at this mtime
        let due: Vec<&str> = needing_backup("h", &saves, &done, 2000)
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        // /c changed 100 s ago: still settling. /d has no time: skipped.
        assert_eq!(due, vec!["/b"]);
        // Another console's record does not count for this one.
        assert_eq!(needing_backup("other", &saves, &done, 2000).len(), 2);
    }

    #[test]
    fn the_consoles_own_backup_folders_are_not_games() {
        // The PS5 lists its empty sce_backup folders (PS4 and PS5 both) among the saves.
        let mut sys = save("/user/home/1/savedata/sce_backup2", 1000);
        sys.title_id = "sce_backup2".into();
        let saves = [sys, save("/user/home/1/savedata_prospero/PPSA01234", 1000)];
        let due = needing_backup("h", &saves, &Done::new(), 2000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].title_id, "PPSA01234");
    }

    /// A save folder with more entries than one page holds is listed whole, so a restore
    /// empties all of it, not only the first page.
    #[test]
    fn a_folder_is_listed_page_by_page() {
        let names = |n: usize| -> Vec<Value> {
            (0..n)
                .map(|i| json!({ "name": format!("f{i}"), "kind": "file" }))
                .collect()
        };
        let pager = |all: Vec<Value>, cut: Option<u64>| {
            let asked = std::cell::RefCell::new(Vec::new());
            let got = all_entries(&|offset| {
                asked.borrow_mut().push(offset);
                let size = cut.unwrap_or(LIST_PAGE);
                let page: Vec<Value> = all
                    .iter()
                    .skip(offset as usize)
                    .take(size as usize)
                    .cloned()
                    .collect();
                let truncated = cut.is_some() && (offset as usize + page.len()) < all.len();
                Ok(json!({ "entries": page, "truncated": truncated }))
            })
            .unwrap();
            (got.len(), asked.into_inner())
        };
        // Fewer than a page: one look, as before.
        assert_eq!(pager(names(3), None), (3, vec![0]));
        // More than a page.
        assert_eq!(pager(names(300), None), (300, vec![0, 256]));
        // Exactly a page: the empty one after it ends the listing.
        assert_eq!(pager(names(256), None), (256, vec![0, 256]));
        // A page cut short by its buffer still asks for the rest.
        assert_eq!(pager(names(10), Some(4)), (10, vec![0, 4, 8]));
        // A failing page fails the whole listing.
        assert!(all_entries(&|_| Err("gone".to_string())).is_err());
    }

    #[test]
    fn the_switch_emulators_saves_are_backed_up_too() {
        let mut sw = save(
            "/data/prosperoeden/user/nand/user/save/0000000000000000/P/0100F2C0115B6000",
            1000,
        );
        sw.title_id = "0100F2C0115B6000".into();
        sw.kind = crate::emulator_saves::KIND.into();
        let due = needing_backup("h", std::slice::from_ref(&sw), &Done::new(), 2000);
        assert_eq!(due.len(), 1);
        // Its backup is named, kept and listed as a PS5 save's is.
        let name = format!(
            "{}{}.zip",
            auto_backup_stem(&sw.title_id, sw.user_id as u32),
            stamp(0)
        );
        assert_eq!(
            parse_backup_name(&sw.title_id, &name),
            Some((sw.user_id as u32, "1970-01-01_000000".to_string()))
        );
    }

    #[test]
    fn stamps_sort_in_time_order_and_names_round_trip() {
        assert_eq!(stamp(0), "1970-01-01_000000");
        assert_eq!(stamp(1_761_300_000_000), "2025-10-24_100000");
        assert!(stamp(1_761_300_000_000) < stamp(1_761_300_001_000));
        let name = format!(
            "{}{}.zip",
            auto_backup_stem("PPSA01234", 0x1234_5678),
            stamp(0)
        );
        assert_eq!(
            parse_backup_name("PPSA01234", &name),
            Some((0x1234_5678, "1970-01-01_000000".to_string()))
        );
        assert_eq!(parse_backup_name("PPSA01234", "PPSA01234.zip"), None);
        assert_eq!(
            parse_backup_name("PPSA01234", "PPSA09999_user12345678_x.zip"),
            None
        );
    }

    #[test]
    fn backups_are_listed_newest_first_and_names_cannot_escape() {
        let dir = std::env::temp_dir().join(format!("ps5upload-savebk-{}", std::process::id()));
        let title = dir.join("PPSA01234");
        std::fs::create_dir_all(&title).unwrap();
        for s in [
            "2026-10-01_090000",
            "2026-10-03_090000",
            "2026-10-02_090000",
        ] {
            std::fs::write(title.join(format!("PPSA01234_user0000abcd_{s}.zip")), b"z").unwrap();
        }
        std::fs::write(title.join("notes.txt"), b"x").unwrap();
        let d = dir.to_string_lossy();
        let stamps: Vec<String> = list_backups(&d, "PPSA01234")
            .into_iter()
            .map(|b| b.stamp)
            .collect();
        assert_eq!(
            stamps,
            [
                "2026-10-03_090000",
                "2026-10-02_090000",
                "2026-10-01_090000"
            ]
        );
        assert!(list_backups(&d, "../PPSA01234").is_empty());
        assert!(list_backups("", "PPSA01234").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_host_is_matched_as_a_whole_address() {
        assert!(mentions_host(
            r#"{"addr":"192.168.1.1","x":1}"#,
            "192.168.1.1"
        ));
        assert!(mentions_host("to 192.168.1.1:9021", "192.168.1.1"));
        assert!(!mentions_host(r#"{"addr":"192.168.1.10"}"#, "192.168.1.1"));
        assert!(!mentions_host(
            r#"{"addr":"10.192.168.1.1"}"#,
            "192.168.1.1"
        ));
        assert!(!mentions_host("anything", ""));
    }

    #[test]
    fn keep_is_clamped() {
        assert_eq!(clamp_keep(0), 1);
        assert_eq!(clamp_keep(500), 50);
        assert_eq!(clamp_keep(7), 7);
    }
}
