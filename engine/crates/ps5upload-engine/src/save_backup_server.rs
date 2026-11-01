//! Save backups to a network share (SMB, or any saved server the engine can write), run by the
//! engine on its own: every few hours, or each time a game is closed on the console (#592).
//!
//! A backup is made exactly as the automatic local one is (`save_backup::download_save`: the
//! save folder copied off the console and tidied, then zipped), then written to
//! `<folder>/<title_id>/<title>_user<id>_<stamp>.zip` on the server under a temporary name and
//! moved into place. It counts only once it is verified: the file is read back from the server,
//! its size and SHA-256 compared with the zip that was sent, and every entry of the read-back zip
//! unpacked against its CRC. A copy that fails is removed from the server; a good one gets a
//! `.sha256` file beside it, which a restore checks again before anything on the console changes.
//!
//! Restore copies a backup off the server and puts it back through the same path as a local one
//! (`save_backup::restore_zip`), which backs up the save that is there now first.
//!
//! "After a game closes" is seen by asking each console for its running app every minute: a
//! title that was running and no longer is was closed, and its saves that changed are backed up.
//! A console that does not answer (off, resting) tells nothing, so a game is never taken for
//! closed because the console went quiet.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::save_archive::{
    auto_backup_stem, backup_tag, cleanup_temp_dir, make_temp_dir, tagged_backups_to_prune,
    zip_folder,
};
use ps5upload_core::saves::SaveEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::emulator_saves;
use crate::remote::{self, PutProgress, RemoteFs};
use crate::save_backup::{
    self as local, done_key, json_error, now_ms, parse_backup_name, plain, stamp, BackupFile, Done,
    SETTLE_SECS,
};

/// How often the scheduler looks: the running game on each console, and whether a pass is due.
pub const TICK: Duration = Duration::from_secs(60);
/// After a pass that failed, the next try comes this soon rather than a whole interval later.
pub const RETRY_MS: u64 = 30 * 60 * 1000;
pub const DEFAULT_HOURS: u32 = 6;
/// The tag of the backup taken on the server of a save before a restore replaced it (when no
/// local backup folder is set): kept apart, so it never pushes the automatic history out.
pub const BEFORE_RESTORE_TAG: &str = "beforerestore";
/// Bytes read from the server at a time when checking or fetching a backup.
const READ_CHUNK: u64 = 4 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    /// Every `every_hours`.
    Interval,
    /// Each time a game is closed on a console.
    AfterClose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerSettings {
    #[serde(default)]
    pub mode: Mode,
    /// `remote://<connection-id>/<folder>`: the saved server and the folder the backups go in.
    #[serde(default)]
    pub target: String,
    #[serde(default = "default_hours")]
    pub every_hours: u32,
    #[serde(default = "default_keep")]
    pub keep: u32,
    /// The consoles to back up (bare IPs); empty: every console the app knows.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The console users to back up; empty: every user.
    #[serde(default)]
    pub users: Vec<u32>,
}

fn default_hours() -> u32 {
    DEFAULT_HOURS
}
fn default_keep() -> u32 {
    local::DEFAULT_KEEP
}

impl Default for ServerSettings {
    fn default() -> Self {
        ServerSettings {
            mode: Mode::Off,
            target: String::new(),
            every_hours: DEFAULT_HOURS,
            keep: local::DEFAULT_KEEP,
            hosts: Vec::new(),
            users: Vec::new(),
        }
    }
}

pub fn clamp_hours(n: u32) -> u32 {
    n.clamp(1, 24 * 7)
}

/// A backup written to the server and read back whole.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Verified {
    pub title_id: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    /// Files in the zip, each unpacked against its CRC when read back.
    pub entries: u32,
}

/// What the last pass did, for the Saves screen. Kept across restarts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerStatus {
    #[serde(skip_deserializing)]
    pub running: bool,
    /// Unix ms of the last finished pass that backed something up or failed; 0 when none.
    #[serde(default)]
    pub last_at: u64,
    /// What started it: `interval`, `closed` (a game was closed) or `manual`.
    #[serde(default)]
    pub last_trigger: String,
    #[serde(default)]
    pub backed_up: u32,
    /// Of `backed_up`: read back from the server and found whole (all of them, or it failed).
    #[serde(default)]
    pub verified: u32,
    #[serde(default)]
    pub failed: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    /// The newest backup that was verified.
    #[serde(default)]
    pub last_verified: Option<Verified>,
    /// Unix ms of the last time every chosen console was looked at (with nothing to do, too).
    #[serde(default)]
    pub last_check_at: u64,
}

/// When a console was last backed up in full, and last tried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostRun {
    #[serde(default)]
    pub ok_ms: u64,
    #[serde(default)]
    pub tried_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    status: ServerStatus,
    /// Console and save path → the save's time when it was last backed up to the server.
    #[serde(default)]
    done: Done,
    #[serde(default)]
    hosts: BTreeMap<String, HostRun>,
}

// ── when to back up (pure, tested) ──────────────────────────────────────

/// Whether an interval pass of a console is due: never backed up, or `hours` since the last full
/// one, and not tried in the last `RETRY_MS` (a server or console that keeps failing is not
/// hammered every minute).
pub fn host_due(run: &HostRun, now_ms: u64, hours: u32) -> bool {
    let every = clamp_hours(hours) as u64 * 3_600_000;
    let interval_up = run.ok_ms == 0 || now_ms.saturating_sub(run.ok_ms) >= every;
    let retry_up = run.tried_ms == 0 || now_ms.saturating_sub(run.tried_ms) >= RETRY_MS.min(every);
    interval_up && retry_up
}

/// The titles closed between two looks at a console: running in `before`, gone in `now`. No
/// earlier look (the engine just started, or the console did not answer) closes nothing.
pub fn closed_titles(before: Option<&BTreeSet<String>>, now: &BTreeSet<String>) -> Vec<String> {
    before
        .map(|b| b.difference(now).cloned().collect())
        .unwrap_or_default()
}

/// Which saves a pass backs up.
pub struct Pick<'a> {
    /// Only these users; empty: all.
    pub users: &'a [u32],
    /// Only this game (a game that was closed).
    pub title: Option<&'a str>,
    /// Leave a save written in the last few minutes for later (it may still be being written).
    pub settle: bool,
    /// Games running now: their saves are being written, so they wait.
    pub running: &'a BTreeSet<String>,
}

/// The saves of `host` that need a backup on the server: a game's, changed since its last one
/// there, of a chosen user, and (see [`Pick`]) not still being written. An emulator's save (the
/// Switch emulator's, a PlayStation or PlayStation 2 emulator's cards or states) belongs to none of
/// the console's users, and goes with the emulator: it is due when the emulator was closed, and
/// waits while it runs.
pub fn due_saves<'a>(
    host: &str,
    saves: &'a [SaveEntry],
    done: &Done,
    now_secs: i64,
    pick: &Pick,
) -> Vec<&'a SaveEntry> {
    saves
        .iter()
        .filter(|e| {
            let app = emulator_saves::app_of(e);
            emulator_saves::backs_up(e)
                && e.mtime > 0
                && e.mtime > done.get(&done_key(host, &e.path)).copied().unwrap_or(0)
                && (pick.users.is_empty()
                    || emulator_saves::is_emulator_save(e)
                    || pick.users.contains(&(e.user_id as u32)))
                && pick.title.is_none_or(|t| app.eq_ignore_ascii_case(t))
                && !pick.running.contains(&app.to_ascii_uppercase())
                && (!pick.settle || now_secs - e.mtime >= SETTLE_SECS)
        })
        .collect()
}

// ── settings and state ──────────────────────────────────────────────────

const SETTINGS_FILE: &str = "save-backup-server.json";
const STATE_FILE: &str = "save-backup-server-state.json";

struct Shared {
    settings: ServerSettings,
    state: Persisted,
    /// The titles seen running on each console at the last look.
    seen: BTreeMap<String, BTreeSet<String>>,
    /// Games closed and not backed up yet (a busy console, a failed pass): console → titles.
    pending: BTreeMap<String, BTreeSet<String>>,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Shared {
            settings: local::load(SETTINGS_FILE),
            state: local::load(STATE_FILE),
            seen: BTreeMap::new(),
            pending: BTreeMap::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    shared().lock().unwrap_or_else(|e| e.into_inner())
}

pub fn settings() -> ServerSettings {
    lock().settings.clone()
}

/// A server is set: restores from it can run, and a save replaced by one is backed up there.
pub fn configured() -> bool {
    !settings().target.trim().is_empty()
}

fn persist(g: &Shared) {
    if let Err(e) = local::save(STATE_FILE, &g.state) {
        crate::log_warn!("save backup to server: {e}");
    }
}

/// The consoles a pass covers: the chosen ones, or every console the app knows.
fn hosts_of(s: &ServerSettings) -> Vec<String> {
    if s.hosts.is_empty() {
        local::settings().hosts
    } else {
        s.hosts.clone()
    }
}

// ── the server ──────────────────────────────────────────────────────────

/// The session for `target` and the folder inside it.
async fn open_target(target: &str) -> Result<(Arc<dyn RemoteFs>, String), String> {
    let rp = remote::path::parse(target.trim()).map_err(|e| e.to_string())?;
    let r = remote::pool::global().map_err(|e| e.to_string())?;
    let fs = r
        .pool
        .fs(&r.store, &rp.connection_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok((fs, rp.path.trim_end_matches('/').to_string()))
}

/// Forget the pooled session for `target` (it may have broken), so the next use signs in again.
fn drop_session(target: &str) {
    if let (Ok(rp), Ok(r)) = (remote::path::parse(target.trim()), remote::pool::global()) {
        r.pool.invalidate(&rp.connection_id);
    }
}

fn rt() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

fn quiet_progress() -> PutProgress {
    PutProgress {
        add: Box::new(|_| {}),
        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

fn sha256_file(path: &Path) -> Result<(u64, String), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let got = f.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if got == 0 {
            break;
        }
        h.update(&buf[..got]);
        n += got as u64;
    }
    Ok((n, hex(&h.finalize())))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Unpacks every entry of the zip at `path` against its CRC (the zip reader checks it at the end
/// of each entry); the number of files in it.
pub fn check_zip(path: &Path) -> Result<u32, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("not a readable zip: {e}"))?;
    let mut n = 0u32;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        let name = e
            .name()
            .map(|n| n.to_string())
            .unwrap_or_else(|_| format!("entry {i}"));
        std::io::copy(&mut e, &mut std::io::sink())
            .map_err(|err| format!("{name} does not match its checksum: {err}"))?;
        if !e.is_dir() {
            n += 1;
        }
    }
    if z.is_empty() {
        return Err("the zip is empty".into());
    }
    Ok(n)
}

/// Copies the server file `path` to the local `dest`; its size and SHA-256.
pub async fn fetch_file(
    fs: &dyn RemoteFs,
    path: &str,
    dest: &Path,
) -> Result<(u64, String), String> {
    let f = fs.open(path).await.map_err(|e| e.to_string())?;
    let size = f.size();
    let mut out =
        std::fs::File::create(dest).map_err(|e| format!("write {}: {e}", dest.display()))?;
    let mut h = Sha256::new();
    let mut at = 0u64;
    while at < size {
        let want = READ_CHUNK.min(size - at);
        let bytes = f.read_at(at, want).await.map_err(|e| e.to_string())?;
        if bytes.is_empty() {
            return Err(format!("{path} ended early, at {at} of {size} bytes"));
        }
        h.update(&bytes);
        out.write_all(&bytes)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
        at += bytes.len() as u64;
    }
    Ok((at, hex(&h.finalize())))
}

/// Reads `path` back from the server into `scratch` and checks it is the zip that was sent:
/// the size the server lists, the bytes read (size and SHA-256), and every entry's CRC.
pub async fn verify_on_server(
    fs: &dyn RemoteFs,
    path: &str,
    size: u64,
    sha256: &str,
    scratch: &Path,
) -> Result<u32, String> {
    let listed = fs.stat(path).await.map_err(|e| e.to_string())?;
    if listed.size != size {
        return Err(format!(
            "the server lists {} bytes, {size} were sent",
            listed.size
        ));
    }
    let (got, sum) = fetch_file(fs, path, scratch).await?;
    if got != size {
        return Err(format!("{got} bytes read back, {size} were sent"));
    }
    if sum != sha256 {
        return Err("the copy read back differs from what was sent".into());
    }
    check_zip(scratch)
}

fn sidecar(path: &str) -> String {
    format!("{path}.sha256")
}

/// Every file name in the server folder `dir` (all pages); none when it is not there.
async fn names_in(fs: &dyn RemoteFs, dir: &str) -> Result<Vec<remote::Entry>, String> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = match fs.list(dir, cursor).await {
            Ok(p) => p,
            Err(remote::RemoteError::NotFound(_)) => return Ok(out),
            Err(e) => return Err(e.to_string()),
        };
        out.extend(page.entries.into_iter().filter(|e| !e.is_dir));
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return Ok(out),
        }
    }
}

/// Writes the local backup zip `zip` to `<folder>/<title_id>/<name>` on the server, verifies it
/// (removed again if it does not check out), writes its `.sha256`, then removes this save's
/// oldest backups of the same kind beyond `keep`. `scratch` is a local file for the read-back.
#[allow(clippy::too_many_arguments)]
pub async fn store_verified(
    fs: &dyn RemoteFs,
    zip: &Path,
    folder: &str,
    title_id: &str,
    user_id: u32,
    name: &str,
    keep: u32,
    scratch: &Path,
) -> Result<Verified, String> {
    if !plain(title_id) || !plain(name) || !name.ends_with(".zip") {
        return Err("unexpected backup name".into());
    }
    let (size, sum) = sha256_file(zip)?;
    // A zip that does not read back here would not on the server either.
    let entries = check_zip(zip).map_err(|e| format!("the backup made here: {e}"))?;
    let dir = format!("{folder}/{title_id}");
    crate::download_relay::make_dirs(fs, &dir)
        .await
        .map_err(|e| format!("make {dir} on the server: {e}"))?;
    let target = format!("{dir}/{name}");
    crate::download_relay::place(fs, zip, &target, &quiet_progress())
        .await
        .map_err(|e| format!("write {target}: {e}"))?;
    match verify_on_server(fs, &target, size, &sum, scratch).await {
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
    // The checksum beside it: a restore checks it again before the console is touched.
    let sum_file = scratch.with_extension("sha256");
    std::fs::write(&sum_file, format!("{sum}  {name}\n")).map_err(|e| format!("{e}"))?;
    if let Err(e) = fs
        .put_file(&sidecar(&target), &sum_file, &quiet_progress())
        .await
    {
        crate::log_warn!("save backup to server: no checksum file for {target}: {e}");
    }
    let _ = std::fs::remove_file(&sum_file);
    // Older backups go only once the new one is there and checked.
    let existing: Vec<String> = names_in(fs, &dir)
        .await?
        .into_iter()
        .map(|e| e.name)
        .collect();
    let tag = backup_tag(name);
    for old in tagged_backups_to_prune(&existing, &auto_backup_stem(title_id, user_id), tag, keep) {
        let p = format!("{dir}/{old}");
        let _ = fs.remove_file(&p).await;
        let _ = fs.remove_file(&sidecar(&p)).await;
    }
    Ok(Verified {
        title_id: title_id.to_string(),
        file: name.to_string(),
        size,
        sha256: sum,
        entries,
    })
}

/// The backups of `title_id` in `folder` on the server, newest first.
pub async fn list_on_server(
    fs: &dyn RemoteFs,
    folder: &str,
    title_id: &str,
) -> Result<Vec<BackupFile>, String> {
    if !plain(title_id) {
        return Ok(Vec::new());
    }
    let mut out: Vec<BackupFile> = names_in(fs, &format!("{folder}/{title_id}"))
        .await?
        .into_iter()
        .filter_map(|e| {
            let (user_id, stamp) = parse_backup_name(title_id, &e.name)?;
            Some(BackupFile {
                tag: backup_tag(&e.name).map(String::from),
                file: e.name,
                title_id: title_id.to_string(),
                user_id,
                stamp,
                size: e.size,
            })
        })
        .collect();
    out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    Ok(out)
}

/// Copies the backup `file` of `title_id` off the server into `dest`, checked against its
/// `.sha256` (when it has one) and its zip CRCs.
pub async fn fetch_backup(
    fs: &dyn RemoteFs,
    folder: &str,
    title_id: &str,
    file: &str,
    dest: &Path,
) -> Result<(), String> {
    if !plain(title_id) || !plain(file) || !file.ends_with(".zip") {
        return Err("unexpected backup name".into());
    }
    let path = format!("{folder}/{title_id}/{file}");
    let (_, sum) = fetch_file(fs, &path, dest).await?;
    let sum_path = dest.with_extension("sha256");
    if fetch_file(fs, &sidecar(&path), &sum_path).await.is_ok() {
        let want = std::fs::read_to_string(&sum_path).unwrap_or_default();
        let _ = std::fs::remove_file(&sum_path);
        let want = want.split_whitespace().next().unwrap_or_default();
        if !want.eq_ignore_ascii_case(&sum) {
            return Err(format!(
                "{file} on the server does not match the checksum written with it: it was changed or damaged"
            ));
        }
    }
    check_zip(dest)
        .map(|_| ())
        .map_err(|e| format!("{file} on the server is damaged: {e}"))
}

// ── backing up ──────────────────────────────────────────────────────────

/// Backs up one save to the server: off the console, zipped, written and verified.
fn back_up_one(
    s: &ServerSettings,
    host: &str,
    entry: &SaveEntry,
    tag: Option<&str>,
) -> Result<Verified, String> {
    let tmp = make_temp_dir(&entry.title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        local::download_save(host, entry, &tmp, &|| Ok(()))?;
        let zip = tmp.join("backup.zip");
        zip_folder(&tmp, &entry.title_id, &zip).map_err(|e| format!("zip: {e:#}"))?;
        let user = entry.user_id as u32;
        let st = match tag {
            Some(t) => format!("{}_{t}", stamp(now_ms())),
            None => stamp(now_ms()),
        };
        let name = format!("{}{st}.zip", auto_backup_stem(&entry.title_id, user));
        let scratch = tmp.join("readback.zip");
        rt().block_on(async {
            let (fs, folder) = open_target(&s.target).await?;
            store_verified(
                fs.as_ref(),
                &zip,
                &folder,
                &entry.title_id,
                user,
                &name,
                s.keep,
                &scratch,
            )
            .await
        })
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    if result.is_err() {
        drop_session(&s.target);
    }
    result
}

/// Called by a restore with no local backup folder: the save there now goes to the server first.
pub fn back_up_before_restore(host: &str, entry: &SaveEntry) -> Result<(), String> {
    back_up_one(&settings(), host, entry, Some(BEFORE_RESTORE_TAG)).map(|_| ())
}

/// The titles running on `host`, upper-case; Err when the console does not answer.
fn running_titles(host: &str) -> Result<BTreeSet<String>, String> {
    let list = ps5upload_core::process_mgr::process_list(host).map_err(|e| format!("{e:#}"))?;
    Ok(list
        .processes
        .iter()
        .filter(|p| p.kind == "app" && crate::looks_like_title_id(&p.title_id))
        .map(|p| p.title_id.to_ascii_uppercase())
        .collect())
}

#[derive(Default)]
struct Tally {
    backed_up: u32,
    failed: u32,
    last_error: Option<String>,
    last_verified: Option<Verified>,
}

/// Backs up `host`'s due saves (see [`due_saves`]). Err when the console did not answer.
fn pass_host(
    s: &ServerSettings,
    host: &str,
    title: Option<&str>,
    settle: bool,
    tally: &mut Tally,
) -> Result<(), String> {
    let saves = local::list_saves(host)?;
    // A game running now is still writing its save; one closed is not.
    let running = running_titles(host).unwrap_or_default();
    let now = (now_ms() / 1000) as i64;
    let done = lock().state.done.clone();
    let pick = Pick {
        users: &s.users,
        title,
        settle,
        running: &running,
    };
    for entry in due_saves(host, &saves, &done, now, &pick) {
        // The server may be changed part-way through a long pass.
        if settings().target != s.target {
            break;
        }
        match back_up_one(s, host, entry, None) {
            Ok(v) => {
                crate::log_info!(
                    "save backup to server: {} on {host} → {} ({} bytes, {} files, verified)",
                    entry.title_id,
                    v.file,
                    v.size,
                    v.entries
                );
                let mut g = lock();
                g.state
                    .done
                    .insert(done_key(host, &entry.path), entry.mtime);
                persist(&g);
                drop(g);
                tally.backed_up += 1;
                tally.last_verified = Some(v);
            }
            Err(e) => {
                crate::log_warn!("save backup to server: {} on {host}: {e}", entry.title_id);
                tally.failed += 1;
                tally.last_error = Some(format!("{}: {e}", entry.title_id));
            }
        }
    }
    Ok(())
}

/// Marks a pass running; false when one already is.
fn begin() -> bool {
    let mut g = lock();
    if g.state.status.running {
        return false;
    }
    g.state.status.running = true;
    true
}

fn finish(trigger: &str, tally: Tally, checked: bool) {
    let mut g = lock();
    let st = &mut g.state.status;
    st.running = false;
    let now = now_ms();
    if checked {
        st.last_check_at = now;
    }
    if tally.backed_up > 0 || tally.failed > 0 {
        st.last_at = now;
        st.last_trigger = trigger.to_string();
        st.backed_up = tally.backed_up;
        st.verified = tally.backed_up;
        st.failed = tally.failed;
        st.last_error = tally.last_error;
        if tally.last_verified.is_some() {
            st.last_verified = tally.last_verified;
        }
    }
    persist(&g);
}

/// One look: the running games (after-close mode) and any pass that is due.
pub fn tick() {
    let s = settings();
    if s.mode == Mode::Off || s.target.trim().is_empty() {
        return;
    }
    if !begin() {
        return;
    }
    let mut tally = Tally::default();
    let now = now_ms();
    let mut checked = false;
    for host in hosts_of(&s) {
        match s.mode {
            Mode::Off => {}
            Mode::AfterClose => {
                let Ok(titles) = running_titles(&host) else {
                    continue;
                };
                {
                    let mut g = lock();
                    let closed = closed_titles(g.seen.get(&host), &titles);
                    if !closed.is_empty() {
                        crate::log_info!(
                            "save backup to server: {} closed on {host}",
                            closed.join(", ")
                        );
                    }
                    g.pending.entry(host.clone()).or_default().extend(closed);
                    g.seen.insert(host.clone(), titles);
                }
                if local::console_busy(&host) {
                    continue;
                }
                let pending: Vec<String> = lock()
                    .pending
                    .get(&host)
                    .map(|p| p.iter().cloned().collect())
                    .unwrap_or_default();
                for title in pending {
                    let before = tally.failed;
                    if pass_host(&s, &host, Some(&title), false, &mut tally).is_ok()
                        && tally.failed == before
                    {
                        if let Some(p) = lock().pending.get_mut(&host) {
                            p.remove(&title);
                        }
                    }
                }
                checked = true;
            }
            Mode::Interval => {
                let run = lock().state.hosts.get(&host).copied().unwrap_or_default();
                if !host_due(&run, now, s.every_hours) || local::console_busy(&host) {
                    continue;
                }
                let before = tally.failed;
                let answered = pass_host(&s, &host, None, true, &mut tally).is_ok();
                let mut g = lock();
                let r = g.state.hosts.entry(host.clone()).or_default();
                if answered {
                    r.tried_ms = now;
                    if tally.failed == before {
                        r.ok_ms = now;
                    }
                    checked = true;
                }
                persist(&g);
            }
        }
    }
    let trigger = if s.mode == Mode::AfterClose {
        "closed"
    } else {
        "interval"
    };
    finish(trigger, tally, checked);
}

/// "Back up now": every chosen console's changed saves, whatever the mode.
pub fn run_now() -> Result<(), String> {
    let s = settings();
    if s.target.trim().is_empty() {
        return Err("choose a server folder for the backups first".into());
    }
    if !begin() {
        return Ok(());
    }
    let mut tally = Tally::default();
    let mut checked = false;
    for host in hosts_of(&s) {
        if pass_host(&s, &host, None, false, &mut tally).is_ok() {
            checked = true;
        }
    }
    finish("manual", tally, checked);
    Ok(())
}

pub fn spawn() {
    crate::ava1_api::spawn_periodic(TICK, tick);
}

// ── restore ─────────────────────────────────────────────────────────────

fn restore(
    host: &str,
    title_id: &str,
    file: &str,
    save_path: &str,
    user_id: u32,
) -> Result<(), String> {
    let s = settings();
    if s.target.trim().is_empty() {
        return Err("no server is set for save backups".into());
    }
    let tmp = make_temp_dir(title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let zip: PathBuf = tmp.join("restore.zip");
        rt().block_on(async {
            let (fs, folder) = open_target(&s.target).await?;
            fetch_backup(fs.as_ref(), &folder, title_id, file, &zip).await
        })?;
        local::restore_zip(host, title_id, &zip, save_path, user_id)
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    if result.is_ok() {
        crate::log_info!("save backup to server: {file} restored on {host} ({save_path})");
    }
    result
}

// ── HTTP ────────────────────────────────────────────────────────────────

fn view() -> Value {
    let g = lock();
    let s = &g.settings;
    // When the next interval pass is due, the soonest of the chosen consoles.
    let next_at = (s.mode == Mode::Interval && !s.target.trim().is_empty()).then(|| {
        let every = clamp_hours(s.every_hours) as u64 * 3_600_000;
        hosts_of(s)
            .iter()
            .map(|h| {
                let r = g.state.hosts.get(h).copied().unwrap_or_default();
                if r.ok_ms == 0 {
                    0
                } else {
                    r.ok_ms + every
                }
            })
            .min()
            .unwrap_or(0)
    });
    json!({ "settings": s, "status": g.state.status, "next_at": next_at })
}

/// `GET /api/save-backup/server` — `{settings, status, next_at}`.
pub async fn get_view() -> Response {
    Json(view()).into_response()
}

#[derive(Deserialize)]
pub struct Patch {
    pub mode: Option<Mode>,
    pub target: Option<String>,
    pub every_hours: Option<u32>,
    pub keep: Option<u32>,
    pub hosts: Option<Vec<String>>,
    pub users: Option<Vec<u32>>,
}

/// Whether the engine can write in `target`: the folder is made, a small file written there
/// and removed.
async fn writable(target: &str) -> Result<(), String> {
    let (fs, folder) = open_target(target).await?;
    if folder.is_empty() || folder == "/" {
        // The top of the share is allowed; nothing to make.
    } else {
        crate::download_relay::make_dirs(fs.as_ref(), &folder)
            .await
            .map_err(|e| format!("make {folder}: {e}"))?;
    }
    let probe = std::env::temp_dir().join(format!("ps5upload-probe-{}", std::process::id()));
    std::fs::write(&probe, b"ok").map_err(|e| e.to_string())?;
    let at = format!("{folder}/.ps5upload-write-test");
    let r = fs.put_file(&at, &probe, &quiet_progress()).await;
    let _ = std::fs::remove_file(&probe);
    r.map_err(|e| format!("the engine cannot write in that folder: {e}"))?;
    let _ = fs.remove_file(&at).await;
    Ok(())
}

/// `PUT /api/save-backup/server` — change any of the settings.
pub async fn put_settings(Json(p): Json<Patch>) -> Response {
    let mut next = settings();
    let before = next.target.clone();
    if let Some(t) = p.target {
        next.target = t.trim().to_string();
    }
    if let Some(m) = p.mode {
        next.mode = m;
    }
    if let Some(h) = p.every_hours {
        next.every_hours = clamp_hours(h);
    }
    if let Some(k) = p.keep {
        next.keep = local::clamp_keep(k);
    }
    if let Some(h) = p.hosts {
        let mut h: Vec<String> = h
            .into_iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        h.sort();
        h.dedup();
        next.hosts = h;
    }
    if let Some(mut u) = p.users {
        u.sort_unstable();
        u.dedup();
        next.users = u;
    }
    if next.mode != Mode::Off && next.target.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "choose a server folder for the backups first",
        );
    }
    if !next.target.is_empty() {
        if !remote::path::is_remote(&next.target) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "choose a folder on a saved server (Connections)",
            );
        }
        if next.target != before {
            if let Err(e) = writable(&next.target).await {
                drop_session(&next.target);
                return json_error(StatusCode::BAD_REQUEST, e);
            }
        }
    }
    {
        let mut g = lock();
        g.settings = next.clone();
        // A new place starts its own history.
        if next.target != before {
            g.state.done.clear();
            g.state.hosts.clear();
            persist(&g);
        }
    }
    if let Err(e) = local::save(SETTINGS_FILE, &next) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(view()).into_response()
}

/// `POST /api/save-backup/server/run` — back up now (in the background).
pub async fn post_run() -> Response {
    if !configured() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "choose a server folder for the backups first",
        );
    }
    tokio::task::spawn_blocking(|| {
        if let Err(e) = run_now() {
            crate::log_warn!("save backup to server: {e}");
        }
    });
    let mut v = view();
    v["status"]["running"] = json!(true);
    (StatusCode::ACCEPTED, Json(v)).into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub title_id: String,
}

/// `GET /api/save-backup/server/list?title_id=` — that game's backups on the server.
pub async fn get_list(Query(q): Query<ListQuery>) -> Response {
    let s = settings();
    if s.target.trim().is_empty() {
        return Json(json!({ "backups": [] })).into_response();
    }
    let title = q.title_id.trim().to_ascii_uppercase();
    let r = async {
        let (fs, folder) = open_target(&s.target).await?;
        list_on_server(fs.as_ref(), &folder, &title).await
    }
    .await;
    match r {
        Ok(list) => Json(json!({ "backups": list })).into_response(),
        Err(e) => {
            drop_session(&s.target);
            json_error(StatusCode::BAD_GATEWAY, e)
        }
    }
}

/// `POST /api/save-backup/server/restore` — `{addr, title_id, file, save_path, user_id}`, as the
/// local restore. The game must not be running.
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
    use crate::remote::MemFs;

    fn save(path: &str, title: &str, user: u32, mtime: i64) -> SaveEntry {
        SaveEntry {
            title_id: title.into(),
            user_id: user as i32,
            path: path.into(),
            size: 1,
            mtime,
            kind: "ps5".into(),
        }
    }

    #[test]
    fn an_interval_pass_is_due_after_its_hours_and_retries_sooner() {
        let h = 3_600_000u64;
        // Never backed up: due at once.
        assert!(host_due(&HostRun::default(), 10 * h, 6));
        // Backed up 5 h ago, every 6 h: not yet; 6 h ago: due.
        let run = HostRun {
            ok_ms: 10 * h,
            tried_ms: 10 * h,
        };
        assert!(!host_due(&run, 15 * h, 6));
        assert!(host_due(&run, 16 * h, 6));
        // A failed try 10 min ago waits; 30 min ago tries again, not a whole interval later.
        let failing = HostRun {
            ok_ms: 0,
            tried_ms: 10 * h,
        };
        assert!(!host_due(&failing, 10 * h + 10 * 60_000, 6));
        assert!(host_due(&failing, 10 * h + RETRY_MS, 6));
        // Hours are clamped: 0 means 1.
        assert!(host_due(&run, 11 * h, 0));
    }

    #[test]
    fn a_game_is_closed_when_it_was_running_and_is_gone() {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        // The first look closes nothing: the engine cannot know what ran before it.
        assert!(closed_titles(None, &set(&["PPSA01234"])).is_empty());
        assert_eq!(
            closed_titles(
                Some(&set(&["PPSA01234", "PPSA05678"])),
                &set(&["PPSA05678"])
            ),
            vec!["PPSA01234".to_string()]
        );
        // A game that started is not a close; one still running is not either.
        assert!(closed_titles(Some(&set(&[])), &set(&["PPSA01234"])).is_empty());
        assert!(closed_titles(Some(&set(&["PPSA01234"])), &set(&["PPSA01234"])).is_empty());
    }

    #[test]
    fn due_saves_follow_users_titles_running_games_and_settling() {
        let saves = [
            save("/u1/A", "PPSA00001", 1, 1000),
            save("/u2/A", "PPSA00001", 2, 1000),
            save("/u1/B", "PPSA00002", 1, 1900),
            save("/u1/sys", "sce_backup", 1, 1000),
        ];
        let none = BTreeSet::new();
        let paths = |v: Vec<&SaveEntry>| v.into_iter().map(|e| e.path.clone()).collect::<Vec<_>>();
        let all = Pick {
            users: &[],
            title: None,
            settle: false,
            running: &none,
        };
        assert_eq!(
            paths(due_saves("h", &saves, &Done::new(), 2000, &all)),
            ["/u1/A", "/u2/A", "/u1/B"]
        );
        // Settling: B was written 100 s ago.
        let settle = Pick {
            settle: true,
            ..all
        };
        assert_eq!(
            paths(due_saves("h", &saves, &Done::new(), 2000, &settle)),
            ["/u1/A", "/u2/A"]
        );
        // One user; one game (closed).
        let user = Pick { users: &[2], ..all };
        assert_eq!(
            paths(due_saves("h", &saves, &Done::new(), 2000, &user)),
            ["/u2/A"]
        );
        let title = Pick {
            title: Some("ppsa00002"),
            ..all
        };
        assert_eq!(
            paths(due_saves("h", &saves, &Done::new(), 2000, &title)),
            ["/u1/B"]
        );
        // A running game waits.
        let running: BTreeSet<String> = ["PPSA00001".to_string()].into();
        let busy = Pick {
            running: &running,
            ..all
        };
        assert_eq!(
            paths(due_saves("h", &saves, &Done::new(), 2000, &busy)),
            ["/u1/B"]
        );
        // Already on the server at this time: not again.
        let mut done = Done::new();
        done.insert(done_key("h", "/u1/A"), 1000);
        assert_eq!(
            paths(due_saves("h", &saves, &done, 2000, &all)),
            ["/u2/A", "/u1/B"]
        );
    }

    #[test]
    fn switch_saves_go_with_the_emulator() {
        let mut sw = save(
            "/data/prosperoeden/user/nand/user/save/0000000000000000/P/0100F2C0115B6000",
            "0100F2C0115B6000",
            0x1F1E_1D1C,
            1000,
        );
        sw.kind = emulator_saves::KIND.into();
        let saves = [save("/u1/A", "PPSA00001", 1, 1000), sw];
        let none = BTreeSet::new();
        let ids = |v: Vec<&SaveEntry>| v.iter().map(|e| e.title_id.clone()).collect::<Vec<_>>();
        let all = Pick {
            users: &[],
            title: None,
            settle: false,
            running: &none,
        };
        // The emulator was closed: its games' saves are due, not the PS5 game's.
        let closed = Pick {
            title: Some(emulator_saves::APP),
            ..all
        };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &closed)),
            ["0100F2C0115B6000"]
        );
        // They belong to none of the console's users, so a chosen user does not leave them out.
        let user = Pick { users: &[1], ..all };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &user)),
            ["PPSA00001", "0100F2C0115B6000"]
        );
        // While the emulator runs, they wait.
        let running: BTreeSet<String> = [emulator_saves::APP.to_string()].into();
        let busy = Pick {
            running: &running,
            ..all
        };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &busy)),
            ["PPSA00001"]
        );
        // A Switch title ID on a save that is not the emulator's is not a game.
        let mut odd = saves[1].clone();
        odd.kind = "ps5".into();
        assert!(due_saves("h", &[odd], &Done::new(), 2000, &all).is_empty());
    }

    #[test]
    fn ps2_saves_go_with_their_emulator() {
        let ps2 = |path: &str, user| SaveEntry {
            kind: crate::ps2_saves::KIND.into(),
            ..save(path, crate::ps2_saves::CRUX2.app, user, 1000)
        };
        let saves = [
            save("/u1/A", "PPSA00001", 1, 1000),
            ps2("/data/crux2/memcards", 0),
            ps2("/data/crux2/sstates", 1),
        ];
        let none = BTreeSet::new();
        let ids = |v: Vec<&SaveEntry>| v.iter().map(|e| e.title_id.clone()).collect::<Vec<_>>();
        let all = Pick {
            users: &[],
            title: None,
            settle: false,
            running: &none,
        };
        // Crux2 was closed: its cards and its saved states are due.
        let closed = Pick {
            title: Some(crate::ps2_saves::CRUX2.app),
            ..all
        };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &closed)),
            [crate::ps2_saves::CRUX2.app; 2]
        );
        // They belong to no user of the console (their user number only tells them apart).
        let user = Pick { users: &[1], ..all };
        assert_eq!(due_saves("h", &saves, &Done::new(), 2000, &user).len(), 3);
        // While it runs, they wait.
        let running: BTreeSet<String> = [crate::ps2_saves::CRUX2.app.to_string()].into();
        let busy = Pick {
            running: &running,
            ..all
        };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &busy)),
            ["PPSA00001"]
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5u-srvbk-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A real backup zip of `title` with one file.
    fn make_zip(dir: &Path, title: &str, body: &[u8]) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join(title)).unwrap();
        std::fs::write(src.join(title).join("save.dat"), body).unwrap();
        let zip = dir.join("backup.zip");
        zip_folder(&src, title, &zip).unwrap();
        zip
    }

    #[tokio::test]
    async fn a_backup_is_written_verified_listed_fetched_and_pruned() {
        let dir = scratch("roundtrip");
        let fs = MemFs::new(&[]);
        let zip = make_zip(&dir, "PPSA01234", b"progress");
        let stem = auto_backup_stem("PPSA01234", 0x1d);
        let mut names = Vec::new();
        for (i, s) in [
            "2026-10-01_000000",
            "2026-10-02_000000",
            "2026-10-03_000000",
        ]
        .iter()
        .enumerate()
        {
            let name = format!("{stem}{s}.zip");
            let v = store_verified(
                &fs,
                &zip,
                "/backups",
                "PPSA01234",
                0x1d,
                &name,
                2,
                &dir.join(format!("rb{i}")),
            )
            .await
            .unwrap();
            assert_eq!(v.entries, 1);
            assert_eq!(v.size, std::fs::metadata(&zip).unwrap().len());
            names.push(name);
        }
        // Kept 2: the oldest went, with its checksum file.
        let p = |n: &str| format!("/backups/PPSA01234/{n}");
        assert!(fs.bytes(&p(&names[0])).is_none());
        assert!(fs.bytes(&format!("{}.sha256", p(&names[0]))).is_none());
        assert!(fs.bytes(&format!("{}.sha256", p(&names[2]))).is_some());
        let listed = list_on_server(&fs, "/backups", "PPSA01234").await.unwrap();
        assert_eq!(
            listed.iter().map(|b| b.file.clone()).collect::<Vec<_>>(),
            [names[2].clone(), names[1].clone()]
        );
        assert_eq!(listed[0].user_id, 0x1d);
        // Nothing for another game, or an odd name.
        assert!(list_on_server(&fs, "/backups", "PPSA09999")
            .await
            .unwrap()
            .is_empty());
        assert!(list_on_server(&fs, "/backups", "../x")
            .await
            .unwrap()
            .is_empty());
        // Fetched whole and checked.
        let out = dir.join("restore.zip");
        fetch_backup(&fs, "/backups", "PPSA01234", &names[2], &out)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&zip).unwrap());
        assert!(
            fetch_backup(&fs, "/backups", "PPSA01234", "../../etc.zip", &out)
                .await
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_damaged_copy_on_the_server_is_caught() {
        let dir = scratch("damaged");
        let fs = MemFs::new(&[]);
        let zip = make_zip(&dir, "PPSA01234", b"progress");
        let name = format!("{}2026-10-01_000000.zip", auto_backup_stem("PPSA01234", 1));
        store_verified(&fs, &zip, "/b", "PPSA01234", 1, &name, 5, &dir.join("rb"))
            .await
            .unwrap();
        let path = format!("/b/PPSA01234/{name}");
        let (size, sum) = sha256_file(&zip).unwrap();
        // The same size, one byte changed: the hash catches it.
        let mut bytes = fs.bytes(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        fs.add(&[(&path, &bytes)]);
        let e = verify_on_server(&fs, &path, size, &sum, &dir.join("rb2"))
            .await
            .unwrap_err();
        assert!(e.contains("differs"), "{e}");
        // Short: the size catches it.
        fs.add(&[(&path, &bytes[..10])]);
        assert!(verify_on_server(&fs, &path, size, &sum, &dir.join("rb3"))
            .await
            .unwrap_err()
            .contains("bytes"));
        // A restore refuses it too, against the checksum written beside it.
        fs.add(&[(&path, &bytes)]);
        let e = fetch_backup(&fs, "/b", "PPSA01234", &name, &dir.join("r.zip"))
            .await
            .unwrap_err();
        assert!(e.contains("checksum"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zip_with_a_bad_crc_does_not_check_out() {
        let dir = scratch("crc");
        let zip = make_zip(&dir, "PPSA01234", &[7u8; 4096]);
        assert_eq!(check_zip(&zip).unwrap(), 1);
        // Stored? deflated: flip a byte inside the compressed data, keeping the headers.
        let mut bytes = std::fs::read(&zip).unwrap();
        // The local header is 30 bytes + the name; the data follows it.
        let name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
        let extra = u16::from_le_bytes([bytes[28], bytes[29]]) as usize;
        let data = 30 + name_len + extra;
        bytes[data + 1] ^= 0x55;
        let bad = dir.join("bad.zip");
        std::fs::write(&bad, &bytes).unwrap();
        assert!(check_zip(&bad).is_err());
        std::fs::write(&bad, b"not a zip").unwrap();
        assert!(check_zip(&bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_default_off_and_read_old_files() {
        let s: ServerSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, ServerSettings::default());
        let s: ServerSettings =
            serde_json::from_str(r#"{"mode":"after_close","target":"remote://a/b"}"#).unwrap();
        assert_eq!(s.mode, Mode::AfterClose);
        assert_eq!(s.every_hours, DEFAULT_HOURS);
        assert_eq!(clamp_hours(0), 1);
        assert_eq!(clamp_hours(10_000), 168);
    }
}
