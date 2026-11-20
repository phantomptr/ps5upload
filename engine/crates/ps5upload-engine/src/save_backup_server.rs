//! Save backups to a network share (SMB, or any saved server the engine can write), run by the
//! engine on its own: every few hours, or each time a game is closed on the console (#592).
//!
//! A backup is made exactly as the automatic local one is (`save_backup::download_save`: the
//! save folder copied off the console and tidied, then zipped), then written to
//! `<folder>/<title_id>/<title>_user<id>_<stamp>.zip` on the server (arranged by user,
//! `<folder>/<name>_<id>/Encrypted/<PS5|PS4>/<title_id>/…`, #886) under a temporary name and
//! moved into place. It counts only once it is verified: the file is read back from the server,
//! its size and SHA-256 compared with the zip that was sent, and every entry of the read-back zip
//! unpacked against its CRC. A copy that fails is removed from the server; a good one gets a
//! `.sha256` file beside it, which a restore checks again before anything on the console changes.
//!
//! With a PS5 drive chosen (`console_drive`, #885), each backup also goes to
//! `<drive>/ps5upload/save-backups` on the console it came from, checked as the automatic
//! backups' copies are (`save_backup_console`), keeping `console_keep` versions there. The two
//! places are reached one after the other and on their own: a server that can't be reached still
//! leaves the drive with the copy (the server is tried again at the next pass), and a drive that
//! is out never fails the server's backup. What each place holds is recorded apart.
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
    auto_backup_stem, backup_tag, cleanup_temp_dir, make_temp_dir, zip_folder_noted,
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
use crate::save_backup_console::{Copies, Drive, Outcome};
use crate::save_layout::{self as layout, Arrange, DecryptedStatus, Kind, Layout, Owner};

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
    /// A drive in the PS5 itself (`/mnt/ext<N>` M.2, `/mnt/usb<N>` USB) that also gets a copy of
    /// each backup this makes, in `ps5upload/save-backups` (#885); "" for none. The copy is made
    /// even when the server can't be reached, and the server is tried again at the next pass.
    #[serde(default)]
    pub console_drive: String,
    /// Versions of each save kept on that drive; 0: as many as `keep`.
    #[serde(default)]
    pub console_keep: u32,
    /// How backups are arranged in the server folder and on the PS5 drive (#886): by game, as
    /// always, or by user (`save_layout`). Backups made before stay where they are.
    #[serde(default)]
    pub layout: Layout,
    /// By user: a game's new folder is `<TITLE> - <game>` when the console names the game.
    #[serde(default)]
    pub game_names: bool,
    /// By user: each backup of a PS5 save also leaves a decrypted copy (one that opens on any
    /// PS5) in the user's Decrypted folder on the server, `keep` of them.
    #[serde(default)]
    pub decrypted_copy: bool,
}

impl ServerSettings {
    /// The layout of the server folder (and its PS5 drive copies).
    pub fn arrange(&self) -> Arrange {
        Arrange {
            layout: self.layout,
            game_names: self.game_names,
        }
    }

    /// Versions of each save kept on the PS5 drive.
    pub fn drive_keep(&self) -> u32 {
        if self.console_keep == 0 {
            self.keep
        } else {
            local::clamp_keep(self.console_keep)
        }
    }
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
            console_drive: String::new(),
            console_keep: 0,
            layout: Layout::ByGame,
            game_names: false,
            decrypted_copy: false,
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
    /// The last copies to the PS5 drive (`ServerSettings::console_drive`); None before any.
    #[serde(default)]
    pub console_copy: Option<crate::save_backup_console::CopyStatus>,
    /// The last decrypted copies made with the backups (`ServerSettings::decrypted_copy`); None
    /// before any.
    #[serde(default)]
    pub decrypted: Option<DecryptedStatus>,
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
    /// Console and save path → the save's time when it was last copied to the PS5 drive: kept
    /// apart from `done`, so a server that was away is tried again without the drive getting
    /// the same save twice, and a drive that was away gets it without the server.
    #[serde(default)]
    console_done: Done,
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

/// Whether a console's closed games that failed to back up are tried again now: not within
/// `RETRY_MS` of the last failed try. Each try copies the save off the console before it reaches
/// the server, so a server that is off for the night must not have the console read every minute.
pub fn pending_due(tried_ms: Option<u64>, now_ms: u64) -> bool {
    tried_ms.is_none_or(|t| now_ms.saturating_sub(t) >= RETRY_MS)
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
    /// With a PS5 drive chosen, what is on it already: a save changed since its last copy there
    /// is due as well, though the server has it (#885).
    pub drive_done: Option<&'a Done>,
}

/// Whether `e` changed since its time in `done` (never there: changed).
fn changed_since(host: &str, e: &SaveEntry, done: &Done) -> bool {
    e.mtime > done.get(&done_key(host, &e.path)).copied().unwrap_or(0)
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
            let apps = emulator_saves::titles_of(emulator_saves::app_of(e));
            emulator_saves::backs_up(e)
                && e.mtime > 0
                && (changed_since(host, e, done)
                    || pick.drive_done.is_some_and(|d| changed_since(host, e, d)))
                && (pick.users.is_empty()
                    || emulator_saves::is_emulator_save(e)
                    || pick.users.contains(&(e.user_id as u32)))
                && pick
                    .title
                    .is_none_or(|t| apps.iter().any(|a| a.eq_ignore_ascii_case(t)))
                && !apps
                    .iter()
                    .any(|a| pick.running.contains(&a.to_ascii_uppercase()))
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
    /// When a console's pending games were last tried and one failed (Unix ms): console → time.
    pending_tried: BTreeMap<String, u64>,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Shared {
            settings: local::load(SETTINGS_FILE),
            state: local::load(STATE_FILE),
            seen: BTreeMap::new(),
            pending: BTreeMap::new(),
            pending_tried: BTreeMap::new(),
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

/// Every entry of the server folder `dir` (all pages): `(name, is_dir, size)`; none when it is
/// not there.
async fn entries_in(fs: &dyn RemoteFs, dir: &str) -> Result<layout::Entries, String> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = match fs.list(dir, cursor).await {
            Ok(p) => p,
            Err(remote::RemoteError::NotFound(_)) => return Ok(out),
            Err(e) => return Err(e.to_string()),
        };
        out.extend(
            page.entries
                .into_iter()
                .filter(|e| e.name != "." && e.name != ".." && !e.name.contains('/'))
                .map(|e| (e.name, e.is_dir, e.size)),
        );
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return Ok(out),
        }
    }
}

/// Runs `f` (which looks at folders of the server folder `root` through `save_layout`'s `list`)
/// with each folder it asks for listed from the server: a round lists what the round before asked
/// for, until it asks for nothing new (the layouts are a few folders deep).
async fn with_listings<T>(fs: &dyn RemoteFs, root: &str, f: impl Fn(&layout::List) -> T) -> T {
    let mut seen: BTreeMap<String, Result<layout::Entries, String>> = BTreeMap::new();
    for _ in 0..8 {
        let (out, missing) = {
            let missing = std::cell::RefCell::new(BTreeSet::new());
            let out = f(&|rel: &str| match seen.get(rel) {
                Some(r) => r.clone(),
                None => {
                    missing.borrow_mut().insert(rel.to_string());
                    Ok(Vec::new())
                }
            });
            (out, missing.into_inner())
        };
        if missing.is_empty() {
            return out;
        }
        drop(out);
        for rel in missing {
            let listed = entries_in(fs, &layout::join(root, &rel)).await;
            seen.insert(rel, listed);
        }
    }
    f(&|rel: &str| seen.get(rel).cloned().unwrap_or(Ok(Vec::new())))
}

/// The folder (relative to `root` on the server) a new `kind` file of `owner` goes in, arranged
/// `how`. Err when the server folder can't be read: a user folder made then could be a second one.
pub async fn remote_place(
    fs: &dyn RemoteFs,
    root: &str,
    how: Layout,
    kind: Kind,
    owner: &Owner,
) -> Result<String, String> {
    if how == Layout::ByUser {
        entries_in(fs, root).await?;
    }
    with_listings(fs, root, |list| layout::place(list, how, kind, owner)).await
}

/// Writes the local backup zip `zip` to `<folder>/<rel>/<name>` on the server (`rel`: the game's
/// folder, `remote_place`), verifies it (removed again if it does not check out), writes its
/// `.sha256`, then removes this save's oldest backups of the same kind beyond `keep`, counted in
/// both layouts. `scratch` is a local file for the read-back.
#[allow(clippy::too_many_arguments)]
pub async fn store_verified(
    fs: &dyn RemoteFs,
    zip: &Path,
    folder: &str,
    rel: &str,
    title_id: &str,
    user_id: u32,
    name: &str,
    keep: u32,
    scratch: &Path,
) -> Result<Verified, String> {
    if !plain(title_id) || !plain(name) || !name.ends_with(".zip") {
        return Err("unexpected backup name".into());
    }
    if rel.is_empty()
        || rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err("unexpected backup folder".into());
    }
    let (size, sum) = sha256_file(zip)?;
    // A zip that does not read back here would not on the server either.
    let entries = check_zip(zip).map_err(|e| format!("the backup made here: {e}"))?;
    let dir = layout::join(folder, rel);
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
    // Older backups go only once the new one is there and checked, counted in both layouts.
    // The backup is made by now: a listing that fails prunes nothing there this time, and does
    // not fail the backup (it would be sent again at the next look, a second copy beside this).
    let files = with_listings(fs, folder, |list| layout::backup_files(list, title_id)).await;
    let tag = backup_tag(name);
    for old in layout::to_prune(&files, title_id, user_id, tag, keep) {
        let p = layout::join(folder, &old.rel());
        if p == target {
            continue;
        }
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

/// The backups of `title_id` in `folder` on the server (by game and by user), newest first.
pub async fn list_on_server(
    fs: &dyn RemoteFs,
    folder: &str,
    title_id: &str,
) -> Result<Vec<BackupFile>, String> {
    if !plain(title_id) {
        return Ok(Vec::new());
    }
    // A server that can't be read says so, rather than listing nothing.
    entries_in(fs, folder).await?;
    let files = with_listings(fs, folder, |list| layout::backup_files(list, title_id)).await;
    let mut out: Vec<BackupFile> = files
        .into_iter()
        .filter_map(|f| {
            let (user_id, stamp) = parse_backup_name(title_id, &f.name)?;
            Some(BackupFile {
                tag: backup_tag(&f.name).map(String::from),
                file: f.name,
                title_id: title_id.to_string(),
                user_id,
                stamp,
                size: f.size,
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
    // Where it is, in either layout (by game when it can't be found: the error names it).
    let path = with_listings(fs, folder, |list| layout::find(list, title_id, file))
        .await
        .map(|f| layout::join(folder, &f.rel()))
        .unwrap_or_else(|| format!("{folder}/{title_id}/{file}"));
    let (_, sum) = fetch_file(fs, &path, dest).await?;
    let sum_path = dest.with_extension("sha256");
    if fetch_file(fs, &sidecar(&path), &sum_path).await.is_ok() {
        let want = std::fs::read_to_string(&sum_path).unwrap_or_default();
        let _ = std::fs::remove_file(&sum_path);
        let want = want.split_whitespace().next().unwrap_or_default();
        // A checksum file cut short (the connection dropped while it was written: it is empty,
        // or part of a sum) says nothing about the backup, as if it were not there; the zip's
        // own check below still runs. Only a whole sum that differs is a changed backup.
        let whole = want.len() == 64 && want.bytes().all(|b| b.is_ascii_hexdigit());
        if whole && !want.eq_ignore_ascii_case(&sum) {
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

/// The name of a backup of `entry` made now: `<title>_user<8 hex>_<stamp>[_<tag>].zip`.
fn backup_name(entry: &SaveEntry, tag: Option<&str>) -> String {
    let st = match tag {
        Some(t) => format!("{}_{t}", stamp(now_ms())),
        None => stamp(now_ms()),
    };
    format!(
        "{}{st}.zip",
        auto_backup_stem(&entry.title_id, entry.user_id as u32)
    )
}

/// Which places one save's backup goes to this time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sides {
    /// The server (the save changed since its last backup there).
    pub share: bool,
    /// The PS5 drive (changed since its last copy there, and the drive is in).
    pub drive: bool,
}

/// What one save's backup did in each place; None: that place was not due.
#[derive(Debug, Default)]
pub(crate) struct Sent {
    pub share: Option<Result<Verified, String>>,
    pub drive: Option<Outcome>,
}

/// Writes a backup zip to the server and verifies it there.
pub(crate) type SendToShare<'a> = dyn Fn(&Path) -> Result<Verified, String> + 'a;

/// Sends the backup zip `zip` of `owner`'s save, made from `host`, to the server (`share`) and to the
/// PS5 drive (`drive`), each when given. Each on its own: one that fails (a server that can't be
/// reached, a drive taken out) never stops or fails the other, and both outcomes are answered.
pub(crate) fn deliver(
    zip: &Path,
    host: &str,
    owner: &Owner,
    share: Option<&SendToShare>,
    drive: Option<(&mut Copies, &dyn Drive)>,
) -> Sent {
    let share = share.map(|send| send(zip));
    let drive = drive.map(|(copies, console)| {
        // A write on the console: no other save operation runs there meanwhile.
        let _op = local::save_op(&[host]);
        copies.copy(console, host, owner, zip)
    });
    Sent { share, drive }
}

/// Backs up one save: off the console and zipped (no other save operation on the console
/// meanwhile), then sent where it is due (see [`deliver`]). Err only when no backup was made.
fn back_up_one(
    s: &ServerSettings,
    host: &str,
    entry: &SaveEntry,
    sides: Sides,
    copies: Option<&mut Copies>,
) -> Result<Sent, String> {
    let owner = local::owner_of(host, entry, s.arrange());
    let tmp = make_temp_dir(&entry.title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let name = backup_name(entry, None);
        let zip = tmp.join(&name);
        {
            let _op = local::save_op(&[host]);
            local::download_save(host, entry, &tmp, &|| Ok(()))?;
            let note = local::backup_note(
                local::console_identity(host).as_deref(),
                Some(entry.user_id as u32),
            );
            zip_folder_noted(&tmp, &entry.title_id, &zip, &note)
                .map_err(|e| format!("zip: {e:#}"))?;
        }
        let scratch = tmp.join("readback.zip");
        let share = |z: &Path| store_zip(s, &owner, &name, z, &scratch);
        let console = crate::save_backup_console::Live(host);
        Ok(deliver(
            &zip,
            host,
            &owner,
            sides.share.then_some(&share as &SendToShare),
            copies
                .filter(|_| sides.drive)
                .map(|c| (c, &console as &dyn Drive)),
        ))
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// The backup zip `zip` of `owner`'s save, written to the server as `name` (where the server
/// folder's layout puts it) and verified. `scratch` is a file to read it back into.
fn store_zip(
    s: &ServerSettings,
    owner: &Owner,
    name: &str,
    zip: &Path,
    scratch: &Path,
) -> Result<Verified, String> {
    let result = rt().block_on(async {
        let (fs, folder) = open_target(&s.target).await?;
        let rel = remote_place(fs.as_ref(), &folder, s.layout, Kind::Encrypted, owner).await?;
        store_verified(
            fs.as_ref(),
            zip,
            &folder,
            &rel,
            &owner.title_id,
            owner.user,
            name,
            s.keep,
            scratch,
        )
        .await
    });
    if result.is_err() {
        drop_session(&s.target);
    }
    result
}

/// Called by a change to a save with no local backup folder: the zip just made of the save there
/// now goes to the server, so it outlives the change.
pub fn keep_on_server(host: &str, entry: &SaveEntry, zip: &Path, tag: &str) -> Result<(), String> {
    let s = settings();
    let owner = local::owner_of(host, entry, s.arrange());
    let tmp = make_temp_dir(&entry.title_id)?;
    let result = store_zip(
        &s,
        &owner,
        &backup_name(entry, Some(tag)),
        zip,
        &tmp.join("readback.zip"),
    );
    let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    result.map(|_| ())
}

/// A decrypted copy of the PS5 save `entry` on `host` in the user's Decrypted folder on the
/// server (#886); `auto_keep`: made with an automatic backup (that many kept), else one the person
/// asked for. Read back and checked like a backup. The file's path on the server.
pub(crate) fn export_decrypted(
    host: &str,
    entry: &SaveEntry,
    auto_keep: Option<u32>,
) -> Result<String, String> {
    let s = settings();
    if s.target.trim().is_empty() {
        return Err(
            "Choose a server folder for the backups first (Saves › Backups to a network share)."
                .into(),
        );
    }
    let shared = crate::save_copy::share_file(&crate::save_copy::Live::new(host), &entry.path)?;
    let arrange = Arrange {
        layout: Layout::ByUser,
        game_names: s.game_names,
    };
    let owner = local::owner_of(host, entry, arrange);
    let name = local::decrypted_name(entry, auto_keep.is_some(), &shared.name);
    let scratch = shared.zip.with_file_name("readback.zip");
    let result = rt().block_on(async {
        let (fs, folder) = open_target(&s.target).await?;
        let fs = fs.as_ref();
        let rel = remote_place(fs, &folder, Layout::ByUser, Kind::Decrypted, &owner).await?;
        let dir = layout::join(&folder, &rel);
        crate::download_relay::make_dirs(fs, &dir)
            .await
            .map_err(|e| format!("make {dir} on the server: {e}"))?;
        let target = format!("{dir}/{name}");
        crate::download_relay::place(fs, &shared.zip, &target, &quiet_progress())
            .await
            .map_err(|e| format!("write {target}: {e}"))?;
        let (size, sum) = sha256_file(&shared.zip)?;
        if let Err(e) = verify_on_server(fs, &target, size, &sum, &scratch).await {
            let _ = fs.remove_file(&target).await;
            return Err(format!("verification failed: {e}"));
        }
        if let Some(keep) = auto_keep {
            let files = with_listings(fs, &folder, |list| {
                layout::decrypted_files(list, &entry.title_id)
            })
            .await;
            let tag = Some(layout::DECRYPTED_TAG);
            for old in layout::to_prune(&files, &entry.title_id, owner.user, tag, keep) {
                let p = layout::join(&folder, &old.rel());
                if p != target {
                    let _ = fs.remove_file(&p).await;
                }
            }
        }
        Ok(target)
    });
    if result.is_err() {
        drop_session(&s.target);
    }
    result
}

/// The titles running on `host`, upper-case; Err when the console does not answer.
pub(crate) fn running_titles(host: &str) -> Result<BTreeSet<String>, String> {
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
    /// Copies to the PS5 drive that failed (a drive that is away is not a failure).
    drive_failed: u32,
    last_error: Option<String>,
    last_verified: Option<Verified>,
    /// The decrypted copies made with the backups (`ServerSettings::decrypted_copy`).
    decrypted: DecryptedStatus,
}

impl Tally {
    /// Everything that failed, in either place: what makes a pass try again soon.
    fn failures(&self) -> u32 {
        self.failed + self.drive_failed
    }

    /// Counts what one save's backup did, and records each place it reached.
    fn count(&mut self, host: &str, entry: &SaveEntry, sent: Sent) {
        if let Some(r) = sent.share {
            match r {
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
                    self.backed_up += 1;
                    self.last_verified = Some(v);
                }
                Err(e) => {
                    crate::log_warn!("save backup to server: {} on {host}: {e}", entry.title_id);
                    self.failed += 1;
                    self.last_error = Some(format!("{}: {e}", entry.title_id));
                }
            }
        }
        match sent.drive {
            Some(Outcome::Copied(_)) => {
                let mut g = lock();
                g.state
                    .console_done
                    .insert(done_key(host, &entry.path), entry.mtime);
                persist(&g);
            }
            Some(Outcome::Failed(_)) => self.drive_failed += 1,
            Some(Outcome::Skipped(_)) | None => {}
        }
    }
}

/// Backs up `host`'s due saves (see [`due_saves`]): to the server, and to the PS5 drive when one
/// is chosen (`copies`). Err when the console did not answer.
fn pass_host(
    s: &ServerSettings,
    host: &str,
    title: Option<&str>,
    settle: bool,
    tally: &mut Tally,
    copies: &mut Option<Copies>,
) -> Result<(), String> {
    let saves = local::list_saves(host)?;
    // A game running now is still writing its save; one closed is not.
    let running = running_titles(host).unwrap_or_default();
    let now = (now_ms() / 1000) as i64;
    let (done, drive_done) = {
        let g = lock();
        (g.state.done.clone(), g.state.console_done.clone())
    };
    let pick = Pick {
        users: &s.users,
        title,
        settle,
        running: &running,
        drive_done: copies.is_some().then_some(&drive_done),
    };
    let console = crate::save_backup_console::Live(host);
    for entry in due_saves(host, &saves, &done, now, &pick) {
        // The server may be changed part-way through a long pass.
        if settings().target != s.target {
            break;
        }
        let share = changed_since(host, entry, &done);
        let mut drive = false;
        if let Some(c) = copies.as_mut() {
            if changed_since(host, entry, &drive_done) {
                // A drive that is away is noted and tried at the next pass; a save due only for
                // its copy is not read off the console for nothing.
                drive = c.ready(&console, host);
                if !drive {
                    c.note_skipped(&console, host);
                }
            }
        }
        if !share && !drive {
            continue;
        }
        match back_up_one(s, host, entry, Sides { share, drive }, copies.as_mut()) {
            Ok(sent) => {
                let on_share = matches!(sent.share, Some(Ok(_)));
                tally.count(host, entry, sent);
                // A decrypted copy beside it, when asked for: never a reason the backup failed.
                if on_share && local::wants_decrypted(s.decrypted_copy, s.arrange(), entry) {
                    let r = export_decrypted(host, entry, Some(s.keep));
                    if let Err(e) = &r {
                        crate::log_warn!(
                            "save backup to server: decrypted copy of {} on {host}: {e}",
                            entry.title_id
                        );
                    }
                    tally.decrypted.record(&r);
                }
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

/// Holds a pass running and clears that however the pass ends: a panic used to leave it set, and
/// every later tick then returned at once, with no backup and no error until a restart.
struct PassRunning;

impl Drop for PassRunning {
    fn drop(&mut self) {
        let mut g = lock();
        g.state.status.running = false;
        if std::thread::panicking() {
            g.state.status.last_error = Some("the backup pass stopped unexpectedly".to_string());
        }
    }
}

/// Marks a pass running; `None` when one already is.
fn begin() -> Option<PassRunning> {
    let mut g = lock();
    if g.state.status.running {
        return None;
    }
    g.state.status.running = true;
    Some(PassRunning)
}

fn finish(trigger: &str, tally: Tally, checked: bool, copies: Option<Copies>) {
    let mut g = lock();
    let st = &mut g.state.status;
    st.running = false;
    if let Some(c) = copies.filter(|c| c.status.at > 0) {
        st.console_copy = Some(c.status);
    }
    if tally.decrypted.at > 0 {
        st.decrypted = Some(tally.decrypted.clone());
    }
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
    let Some(_running) = begin() else {
        return;
    };
    let mut tally = Tally::default();
    let mut copies = Copies::new(&s.console_drive, s.drive_keep(), s.arrange());
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
                    // A game just closed is tried at once, whatever failed before.
                    if !closed.is_empty() {
                        g.pending_tried.remove(&host);
                    }
                    g.pending.entry(host.clone()).or_default().extend(closed);
                    g.seen.insert(host.clone(), titles);
                }
                if local::console_busy(&host) {
                    continue;
                }
                let (pending, tried): (Vec<String>, Option<u64>) = {
                    let g = lock();
                    (
                        g.pending
                            .get(&host)
                            .map(|p| p.iter().cloned().collect())
                            .unwrap_or_default(),
                        g.pending_tried.get(&host).copied(),
                    )
                };
                if pending.is_empty() || !pending_due(tried, now) {
                    checked = true;
                    continue;
                }
                let mut any_failed = false;
                for title in pending {
                    let before = tally.failures();
                    if pass_host(&s, &host, Some(&title), false, &mut tally, &mut copies).is_ok()
                        && tally.failures() == before
                    {
                        if let Some(p) = lock().pending.get_mut(&host) {
                            p.remove(&title);
                        }
                    } else {
                        any_failed = true;
                    }
                }
                {
                    let mut g = lock();
                    if any_failed {
                        g.pending_tried.insert(host.clone(), now);
                    } else {
                        g.pending_tried.remove(&host);
                    }
                }
                checked = true;
            }
            Mode::Interval => {
                let run = lock().state.hosts.get(&host).copied().unwrap_or_default();
                if !host_due(&run, now, s.every_hours) || local::console_busy(&host) {
                    continue;
                }
                let before = tally.failures();
                let answered = pass_host(&s, &host, None, true, &mut tally, &mut copies).is_ok();
                let mut g = lock();
                let r = g.state.hosts.entry(host.clone()).or_default();
                if answered {
                    r.tried_ms = now;
                    if tally.failures() == before {
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
    finish(trigger, tally, checked, copies);
}

/// "Back up now": every chosen console's changed saves, whatever the mode.
pub fn run_now() -> Result<(), String> {
    let s = settings();
    if s.target.trim().is_empty() {
        return Err("choose a server folder for the backups first".into());
    }
    let Some(_running) = begin() else {
        return Ok(());
    };
    let mut tally = Tally::default();
    let mut copies = Copies::new(&s.console_drive, s.drive_keep(), s.arrange());
    let mut checked = false;
    for host in hosts_of(&s) {
        if pass_host(&s, &host, None, false, &mut tally, &mut copies).is_ok() {
            checked = true;
        }
    }
    finish("manual", tally, checked, copies);
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
        local::restore_zip_any_user(host, title_id, &zip, save_path, user_id, None)
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    if result.is_ok() {
        crate::log_info!("save backup to server: {file} restored on {host} ({save_path})");
        mark_restored(host, save_path);
    }
    result
}

/// A save just put back from the server is already on it: the next pass need not store the same
/// content again. Best effort.
fn mark_restored(host: &str, save_path: &str) {
    let Ok(list) = local::list_saves(host) else {
        return;
    };
    let Some(entry) = list.iter().find(|e| e.path == save_path) else {
        return;
    };
    let mut g = lock();
    g.state.done.insert(done_key(host, save_path), entry.mtime);
    persist(&g);
}

/// Copies the backup `file` of `title_id` off the server into the local file `dest` (restoring
/// one to another user, `save_copy`).
pub(crate) fn fetch_to(title_id: &str, file: &str, dest: &Path) -> Result<(), String> {
    let s = settings();
    if s.target.trim().is_empty() {
        return Err("no server is set for save backups".into());
    }
    rt().block_on(async {
        let (fs, folder) = open_target(&s.target).await?;
        fetch_backup(fs.as_ref(), &folder, title_id, file, dest).await
    })
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
    /// The PS5 drive that also gets a copy of each backup (`/mnt/ext<N>`, `/mnt/usb<N>`); "" for
    /// none (#885).
    pub console_drive: Option<String>,
    /// Versions of each save kept on that drive, 1 to 50; 0: as many as `keep`.
    pub console_keep: Option<u32>,
    /// `by_game` or `by_user` (#886).
    pub layout: Option<Layout>,
    /// By user: name a game's new folder after the game too.
    pub game_names: Option<bool>,
    /// By user: a decrypted copy with each backup of a PS5 save.
    pub decrypted_copy: Option<bool>,
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
    let drive_before = next.console_drive.clone();
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
    if let Some(d) = p.console_drive {
        let d = d.trim().trim_end_matches('/').to_string();
        if !d.is_empty() && !crate::save_backup_console::drive_path_ok(&d) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "console_drive must be a PS5 M.2 or USB drive (/mnt/ext0, /mnt/usb0, …) or empty",
            );
        }
        next.console_drive = d;
    }
    if let Some(k) = p.console_keep {
        next.console_keep = if k == 0 { 0 } else { local::clamp_keep(k) };
    }
    if let Some(l) = p.layout {
        next.layout = l;
    }
    if let Some(g) = p.game_names {
        next.game_names = g;
    }
    if let Some(d) = p.decrypted_copy {
        next.decrypted_copy = d;
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
            g.pending_tried.clear();
            persist(&g);
        }
        if next.console_drive != drive_before {
            g.state.console_done.clear();
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
    use ps5upload_core::save_archive::zip_folder;

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
    fn a_pass_that_panics_does_not_stay_running() {
        let panicked = std::panic::catch_unwind(|| {
            let _running = begin().expect("no other pass in this test");
            panic!("a pass died");
        });
        assert!(panicked.is_err());
        let running = begin();
        assert!(running.is_some(), "the next pass can start");
        assert!(begin().is_none(), "one at a time");
        drop(running);
        assert!(!lock().state.status.running);
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
    fn a_closed_game_that_failed_is_not_tried_again_every_minute() {
        let t = 1_700_000_000_000u64;
        assert!(pending_due(None, t));
        assert!(!pending_due(Some(t), t + 60_000));
        assert!(!pending_due(Some(t), t + RETRY_MS - 1));
        assert!(pending_due(Some(t), t + RETRY_MS));
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
            drive_done: None,
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
            drive_done: None,
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
        // Its mirror is the same emulator: closing it makes them due, and they wait while it runs.
        let mirror_closed = Pick {
            title: Some(emulator_saves::MIRROR),
            ..all
        };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &mirror_closed)),
            ["0100F2C0115B6000"]
        );
        let mirror: BTreeSet<String> = [emulator_saves::MIRROR.to_string()].into();
        let mirror_busy = Pick {
            running: &mirror,
            ..all
        };
        assert_eq!(
            ids(due_saves("h", &saves, &Done::new(), 2000, &mirror_busy)),
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
            drive_done: None,
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

    /// #886: by user on a share already holding backups by game: the new one goes under the
    /// user's folder (an existing one, found by account ID, after a rename), both list and come
    /// back, and "Versions to keep" counts both, never deleting a file the app did not make.
    #[tokio::test]
    async fn by_user_on_a_share_lists_restores_and_prunes_with_the_by_game_ones() {
        let dir = scratch("by-user");
        let stem = auto_backup_stem("PPSA01234", 0x9abc_def0);
        let old_a = format!("/b/PPSA01234/{stem}2026-10-01_000000.zip");
        let old_b = format!("/b/PPSA01234/{stem}2026-10-02_000000.zip");
        let by_hand = format!("/b/PPSA01234/{stem}copy I made.zip");
        let other = "/b/PPSA01234/PPSA01234_user00000001_2026-09-01_000000.zip".to_string();
        let fs = MemFs::new(&[
            (&old_a, b"a".as_slice()),
            (&old_b, b"b".as_slice()),
            (&by_hand, b"h".as_slice()),
            (&other, b"o".as_slice()),
            (
                "/b/Sam (old name)_9abcdef0/Decrypted/PS5/PPSA01234/keep.zip",
                b"k".as_slice(),
            ),
        ]);
        let owner = Owner {
            title_id: "PPSA01234".into(),
            user: 0x9abc_def0,
            platform: Some(layout::Platform::Ps5),
            name: Some("Sam".into()),
            game: Some("Astro Bot".into()),
        };
        let rel = remote_place(&fs, "/b", Layout::ByUser, Kind::Encrypted, &owner)
            .await
            .unwrap();
        assert_eq!(
            rel, "Sam (old name)_9abcdef0/Encrypted/PS5/PPSA01234 - Astro Bot",
            "the user's folder already there, by account ID"
        );
        assert_eq!(
            remote_place(&fs, "/b", Layout::ByGame, Kind::Encrypted, &owner)
                .await
                .unwrap(),
            "PPSA01234"
        );
        let zip = make_zip(&dir, "PPSA01234", b"progress");
        let name = format!("{stem}2026-11-17_101500.zip");
        store_verified(
            &fs,
            &zip,
            "/b",
            &rel,
            "PPSA01234",
            0x9abc_def0,
            &name,
            2,
            &dir.join("rb"),
        )
        .await
        .unwrap();
        let at = format!("/b/{rel}/{name}");
        assert!(fs.bytes(&at).is_some());
        assert!(fs.bytes(&format!("{at}.sha256")).is_some());
        // Keep 2 across both layouts: the oldest by-game one went; nothing else did.
        assert!(fs.bytes(&old_a).is_none());
        for kept in [&old_b, &by_hand, &other] {
            assert!(fs.bytes(kept).is_some(), "{kept}");
        }
        let listed: Vec<String> = list_on_server(&fs, "/b", "PPSA01234")
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.file)
            .collect();
        assert!(listed.contains(&name));
        assert!(listed.contains(&format!("{stem}2026-10-02_000000.zip")));
        assert!(listed.contains(&"PPSA01234_user00000001_2026-09-01_000000.zip".to_string()));
        // Back from the user's folder, checked against its checksum.
        let out = dir.join("restore.zip");
        fetch_backup(&fs, "/b", "PPSA01234", &name, &out)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&zip).unwrap());
        // And from the by-game folder too.
        let back = dir.join("old.zip");
        assert!(fetch_backup(
            &fs,
            "/b",
            "PPSA01234",
            &format!("{stem}2026-10-02_000000.zip"),
            &back
        )
        .await
        .is_err_and(|e| e.contains("damaged")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_new_user_folder_is_not_made_when_the_share_cannot_be_read() {
        let fs = MemFs::new(&[]);
        let owner = Owner {
            title_id: "PPSA01234".into(),
            user: 1,
            platform: Some(layout::Platform::Ps5),
            name: Some("Alex".into()),
            game: None,
        };
        assert_eq!(
            remote_place(&fs, "/b", Layout::ByUser, Kind::Encrypted, &owner)
                .await
                .unwrap(),
            "Alex_00000001/Encrypted/PS5/PPSA01234"
        );
        fs.set_down(true);
        assert!(
            remote_place(&fs, "/b", Layout::ByUser, Kind::Encrypted, &owner)
                .await
                .is_err()
        );
        assert!(list_on_server(&fs, "/b", "PPSA01234").await.is_err());
    }

    /// The backup is on the server and checked by the time the folder is listed for pruning: a
    /// listing that fails then used to fail the backup, so the next look sent another copy.
    #[tokio::test]
    async fn a_failed_listing_for_the_prune_does_not_fail_a_verified_backup() {
        let dir = scratch("prune-listing");
        let fs = MemFs::new(&[]);
        let zip = make_zip(&dir, "PPSA01234", b"progress");
        let name = format!("{}2026-10-01_000000.zip", auto_backup_stem("PPSA01234", 1));
        fs.fail_next_lists(1);
        let v = store_verified(
            &fs,
            &zip,
            "/b",
            "PPSA01234",
            "PPSA01234",
            1,
            &name,
            5,
            &dir.join("rb"),
        )
        .await
        .unwrap();
        assert_eq!(v.file, name);
        assert!(fs.bytes(&format!("/b/PPSA01234/{name}")).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The checksum file is written after the backup; cut short, it used to make a good backup
    /// "changed or damaged" for good.
    #[tokio::test]
    async fn a_checksum_file_cut_short_does_not_condemn_a_good_backup() {
        let dir = scratch("short-sum");
        let fs = MemFs::new(&[]);
        let zip = make_zip(&dir, "PPSA01234", b"progress");
        let name = format!("{}2026-10-01_000000.zip", auto_backup_stem("PPSA01234", 1));
        store_verified(
            &fs,
            &zip,
            "/b",
            "PPSA01234",
            "PPSA01234",
            1,
            &name,
            5,
            &dir.join("rb"),
        )
        .await
        .unwrap();
        let sum_path = format!("/b/PPSA01234/{name}.sha256");
        let whole = fs.bytes(&sum_path).unwrap();
        let out = dir.join("restore.zip");
        for cut in [&b""[..], &whole[..10]] {
            fs.add(&[(&sum_path, cut)]);
            fetch_backup(&fs, "/b", "PPSA01234", &name, &out)
                .await
                .unwrap();
        }
        // A whole sum that is another file's still refuses it.
        let other = format!("{}  {name}\n", "0".repeat(64));
        fs.add(&[(&sum_path, other.as_bytes())]);
        let e = fetch_backup(&fs, "/b", "PPSA01234", &name, &out)
            .await
            .unwrap_err();
        assert!(e.contains("changed or damaged"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_damaged_copy_on_the_server_is_caught() {
        let dir = scratch("damaged");
        let fs = MemFs::new(&[]);
        let zip = make_zip(&dir, "PPSA01234", b"progress");
        let name = format!("{}2026-10-01_000000.zip", auto_backup_stem("PPSA01234", 1));
        store_verified(
            &fs,
            &zip,
            "/b",
            "PPSA01234",
            "PPSA01234",
            1,
            &name,
            5,
            &dir.join("rb"),
        )
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

    /// A backup zip named as the pass names one, made in `dir`.
    fn named_zip(dir: &Path, name: &str) -> PathBuf {
        let made = make_zip(dir, "PPSA01234", name.as_bytes());
        let zip = dir.join(name);
        std::fs::rename(&made, &zip).unwrap();
        zip
    }

    /// #885: one run writes the share and the PS5 drive; either one failing (a share that can't
    /// be reached, a drive that is out or garbles the copy) never stops or fails the other.
    #[test]
    fn a_run_reaches_the_share_and_the_ps5_drive_and_one_failing_spares_the_other() {
        use crate::save_backup_console::tests::{vol, with_m2, Fake};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = scratch("both-sides");
        let fs = MemFs::new(&[]);
        let host = "10.88.0.1";
        let stem = auto_backup_stem("PPSA01234", 1);
        let by_game = Owner {
            title_id: "PPSA01234".into(),
            user: 1,
            ..Owner::default()
        };
        let send = |name: &str, drive: &Fake, keep: u32| {
            let zip = named_zip(&dir, name);
            let share = |z: &Path| {
                rt.block_on(store_verified(
                    &fs,
                    z,
                    "/b",
                    "PPSA01234",
                    "PPSA01234",
                    1,
                    name,
                    5,
                    &dir.join("rb.zip"),
                ))
            };
            let mut copies = Copies::new("/mnt/ext0", keep, Arrange::default()).unwrap();
            let sent = deliver(
                &zip,
                host,
                &by_game,
                Some(&share),
                Some((&mut copies, drive)),
            );
            (sent, copies.status)
        };
        let on_drive = |name: &str| format!("/mnt/ext0/ps5upload/save-backups/PPSA01234/{name}");

        // Both places get the same file.
        let m2 = with_m2();
        let first = format!("{stem}2026-11-17_101500.zip");
        let (sent, _) = send(&first, &m2, 1);
        assert_eq!(sent.share.unwrap().unwrap().file, first);
        assert_eq!(sent.drive, Some(Outcome::Copied(on_drive(&first))));
        assert!(fs.bytes(&format!("/b/PPSA01234/{first}")).is_some());

        // The share can't be reached: the drive copy is still made (and keeps 1 version there).
        fs.set_down(true);
        let second = format!("{stem}2026-11-17_111500.zip");
        let (sent, status) = send(&second, &m2, 1);
        fs.set_down(false);
        assert!(sent.share.unwrap().is_err());
        assert_eq!(sent.drive, Some(Outcome::Copied(on_drive(&second))));
        assert_eq!(status.copied, 1);
        let files = m2.files.borrow();
        assert!(files.contains_key(&on_drive(&second)));
        assert!(!files.contains_key(&on_drive(&first)), "one version kept");
        drop(files);
        assert!(fs.bytes(&format!("/b/PPSA01234/{second}")).is_none());

        // The drive is not plugged in: the share still gets a verified backup.
        let away = Fake {
            vols: vec![vol("/data", "/dev/ssd0.user")],
            ..Fake::default()
        };
        let third = format!("{stem}2026-11-17_121500.zip");
        let (sent, status) = send(&third, &away, 1);
        assert!(sent.share.unwrap().is_ok());
        assert!(matches!(sent.drive, Some(Outcome::Skipped(_))));
        assert_eq!((status.skipped, status.failed), (1, 0));

        // The drive garbles the copy: removed there and reported; the share still verified.
        let bad = Fake {
            corrupt: true,
            ..with_m2()
        };
        let fourth = format!("{stem}2026-11-17_131500.zip");
        let (sent, status) = send(&fourth, &bad, 1);
        assert!(sent.share.unwrap().is_ok());
        assert!(matches!(sent.drive, Some(Outcome::Failed(_))));
        assert_eq!(status.failed, 1);
        assert!(bad.files.borrow().is_empty());

        // A place that is not due is not touched.
        let fifth = format!("{stem}2026-11-17_141500.zip");
        let zip = named_zip(&dir, &fifth);
        let sent = deliver(&zip, host, &by_game, None, None);
        assert!(sent.share.is_none() && sent.drive.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a run did in each place is recorded apart: a share that failed is tried again at the
    /// next pass, while the drive, which has the save, does not get it twice.
    #[test]
    fn each_place_keeps_its_own_record_of_what_it_has() {
        let host = "10.88.0.2";
        let entry = save(
            "/user/home/1/savedata_prospero/PPSA01234",
            "PPSA01234",
            1,
            500,
        );
        let mut tally = Tally::default();
        tally.count(
            host,
            &entry,
            Sent {
                share: Some(Err("connection reset".into())),
                drive: Some(Outcome::Copied("/mnt/ext0/x".into())),
            },
        );
        assert_eq!(
            (tally.failed, tally.drive_failed, tally.failures()),
            (1, 0, 1)
        );
        let key = done_key(host, &entry.path);
        {
            let g = lock();
            assert_eq!(g.state.console_done.get(&key), Some(&500));
            assert_eq!(g.state.done.get(&key), None);
        }
        // Next pass: due for the share only.
        let (done, drive_done) = {
            let g = lock();
            (g.state.done.clone(), g.state.console_done.clone())
        };
        assert!(changed_since(host, &entry, &done));
        assert!(!changed_since(host, &entry, &drive_done));
        let none = BTreeSet::new();
        let pick = Pick {
            users: &[],
            title: None,
            settle: false,
            running: &none,
            drive_done: Some(&drive_done),
        };
        let saves = [entry.clone()];
        assert_eq!(due_saves(host, &saves, &done, 9000, &pick).len(), 1);
        // A drive copy that failed counts as a failure (tried again soon), a share that went
        // through is recorded.
        let mut tally = Tally::default();
        tally.count(
            host,
            &entry,
            Sent {
                share: Some(Ok(Verified {
                    title_id: "PPSA01234".into(),
                    file: "f.zip".into(),
                    ..Verified::default()
                })),
                drive: Some(Outcome::Failed("x".into())),
            },
        );
        assert_eq!((tally.backed_up, tally.drive_failed), (1, 1));
        assert_eq!(lock().state.done.get(&key), Some(&500));
        // Both have it now: nothing is due.
        let (done, drive_done) = {
            let g = lock();
            (g.state.done.clone(), g.state.console_done.clone())
        };
        let pick = Pick {
            drive_done: Some(&drive_done),
            ..pick
        };
        assert!(due_saves(host, &saves, &done, 9000, &pick).is_empty());
    }

    #[test]
    fn the_ps5_drive_copy_settings_read_old_files_and_keep_their_own_versions() {
        let s: ServerSettings =
            serde_json::from_str(r#"{"mode":"after_close","target":"remote://a/b","keep":7}"#)
                .unwrap();
        assert_eq!(s.console_drive, "");
        assert_eq!(s.drive_keep(), 7, "0 keeps as many as the share");
        let s = ServerSettings {
            console_keep: 1,
            ..s
        };
        assert_eq!(s.drive_keep(), 1);
        let s = ServerSettings {
            console_keep: 999,
            ..s
        };
        assert_eq!(s.drive_keep(), 50);
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
