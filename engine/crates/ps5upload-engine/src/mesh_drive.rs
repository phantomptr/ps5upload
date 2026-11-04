//! Mesh Drive (8.0.0): a folder kept the same across devices, with its home (the hub) in a folder on
//! a console. Each device that joins picks a folder of its own; its engine keeps that folder and the
//! hub the same, one pass every half minute while the console's helper answers. Every device syncs
//! with the hub, so a file added anywhere reaches everyone.
//!
//! Where things live:
//! * the console's "drives" document (mesh doc): every drive, its hub folder, its optional password
//!   (PBKDF2 only), and its members with what each last reported (state, files, size, last sync):
//!   the one place every device reads who is in and how they are doing;
//! * this engine's `mesh-drive/config.json`: which of those members are folders on this device;
//! * `mesh-drive/<drive>-<member>.json`: what this folder and the hub last agreed on (the base the
//!   planner compares against, `mesh_drive_plan`).
//!
//! Safety: a folder without its marker file is offline, never "everything deleted"; deletes go to a
//! trash folder for 30 days; a pass that would delete many files waits for a person to confirm; two
//! engines never sync the same drive at once (the console's `drive:<id>` lock).
//!
//! Routes (each answers `supported:false` against a helper from before 8.0.0):
//! * `GET  /api/drive?addr=`                                   -> drives, this device's folders
//! * `POST /api/drive/create`  `{addr,name,hub,hub_name?,path,password?}`
//! * `POST /api/drive/join`    `{addr,drive,path,password?}`  (no approval step: the password, if set)
//! * `POST /api/drive/password` `{addr,drive,password?,current?}`
//! * `POST /api/drive/approve` `{addr,drive,member,approve}`  (kept for 8.0.0 apps)
//! * `POST /api/drive/leave`   `{addr,drive,member}`          (any member, e.g. a device long gone)
//! * `POST /api/drive/delete`  `{addr,drive}`                   (the drive goes; files stay)
//! * `POST /api/drive/pause`   `{addr,drive,member,paused}`
//! * `POST /api/drive/sync`    `{addr,drive}`                   (a pass now)
//! * `POST /api/drive/confirm` `{addr,drive,member}`            (let a held-back mass delete run)
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::fs_ops::{self, ListDirOptions};
use ps5upload_core::mesh;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::json_err;
use crate::mesh_drive_plan::{
    conflict_name, date_parts, ignored, parse_conflict, plan, too_many_deletes, Base, BaseEntry,
    Listing, Meta, Op, BASE_FILE, MARKER, TMP, TRASH,
};

/// One pass per member this often.
pub const EVERY: Duration = Duration::from_secs(30);
/// A drive holds at most this many files (a pass lists both sides in full).
const MAX_FILES: usize = 50_000;
/// A file changed this recently may still be being written: it waits for the next pass.
const SETTLE_SECS: i64 = 10;
/// The drive lock lasts this long, and a running pass renews it this often.
const LOCK_TTL_S: u64 = 600;
const LOCK_RENEW: Duration = Duration::from_secs(120);
/// Trash is emptied after this long.
const TRASH_DAYS: i64 = 30;

// ---- the console's document ----

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DriveDoc {
    #[serde(default)]
    pub drives: Vec<Drive>,
    /// What a newer version keeps here: carried through untouched.
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Drive {
    pub id: String,
    pub name: String,
    /// The hub: a folder on this console.
    pub hub: String,
    #[serde(default)]
    pub created: u64,
    /// The device that made it (key hex).
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub members: Vec<Member>,
    /// Set when the drive asks a password of a device joining it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<DriveLock>,
    /// Bumped by a pass that changed the home folder: the others list it again only then (or
    /// every FULL_SCAN), instead of on every pass.
    #[serde(default)]
    pub hub_rev: u64,
    /// The home folder, as the last pass that listed it saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<DriveStats>,
    /// Other consoles holding this drive (8.1.0): each is the master of its own circle, and the
    /// two masters keep their home folders the same. Listed on both consoles; links form a tree
    /// (a console joins a drive's masters at most once), so a change never comes back around.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

/// One master link, as seen from this console.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Link {
    /// The other console (address).
    pub console: String,
    #[serde(default)]
    pub console_name: String,
    /// The drive's home folder there.
    pub hub: String,
    #[serde(default)]
    pub created: u64,
    // What the engine that last carried the link reported.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub last_sync: u64,
    #[serde(default)]
    pub by_name: String,
    /// Devices in that console's circle for this drive.
    #[serde(default)]
    pub circle: u64,
    /// A person let a held-back mass delete through this link run (consumed by the next pass).
    #[serde(default)]
    pub allow_deletes: bool,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

/// A drive's password, kept only as an Argon2id hash (hex) with its salt and cost. Every field
/// has a default: a lock another version wrote (other fields, an older or newer scheme) must
/// never make the whole drive list unreadable; a lock this version cannot check opens to no one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DriveLock {
    /// "argon2id".
    #[serde(default)]
    pub alg: String,
    #[serde(default)]
    pub salt: String,
    #[serde(default)]
    pub hash: String,
    /// Memory (KiB), passes, lanes.
    #[serde(default)]
    pub m: u32,
    #[serde(default)]
    pub t: u32,
    #[serde(default)]
    pub p: u32,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DriveStats {
    pub files: u64,
    pub bytes: u64,
    /// Copies named "(conflict from ...)".
    pub conflicts: u64,
    pub updated: u64,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Member {
    pub id: String,
    /// The device whose engine syncs this folder (key hex).
    pub device: String,
    #[serde(default)]
    pub device_name: String,
    /// The folder on that device.
    pub path: String,
    /// False while it waits for a member to let it in.
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub joined: u64,
    // What that device last reported (its engine writes it after a pass, when it changed): the
    // console is the one place every device reads how the others are doing.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub last_sync: u64,
    #[serde(default)]
    pub reported: u64,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn new_id() -> String {
    let b: [u8; 8] = ava1::keys::random_bytes().unwrap_or_else(|_| now_ms().to_le_bytes());
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `{rev, doc}` from the console; None for a helper without documents.
fn doc_read(addr: &str) -> Result<Option<(u64, DriveDoc)>> {
    let Some(v) = crate::mesh_api::call(addr, || mesh::doc_get(addr, "drives"))? else {
        return Ok(None);
    };
    let rev = v.get("rev").and_then(Value::as_u64).unwrap_or(0);
    // A list this version cannot read is never taken as empty: writing it back would drop every
    // drive in it.
    let doc = match v.get("doc") {
        Some(d) if d.is_object() => serde_json::from_value(d.clone())
            .map_err(|e| anyhow!("the console's drive list cannot be read by this version: {e}"))?,
        _ => DriveDoc::default(),
    };
    Ok(Some((rev, doc)))
}

/// Read, change, write against the revision read; again when another device wrote in between.
fn doc_update<T>(addr: &str, mut f: impl FnMut(&mut DriveDoc) -> Result<T>) -> Result<Option<T>> {
    for _ in 0..6 {
        let Some((rev, mut doc)) = doc_read(addr)? else {
            return Ok(None);
        };
        let out = f(&mut doc)?;
        let body = serde_json::to_value(&doc)?;
        let Some(r) = crate::mesh_api::call(addr, || mesh::doc_put(addr, "drives", rev, &body))?
        else {
            return Ok(None);
        };
        if r.get("saved").and_then(Value::as_bool) == Some(true) {
            return Ok(Some(out));
        }
    }
    bail!("the drive list kept changing on the console; try again")
}

// ---- this engine's members ----

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct LocalConfig {
    #[serde(default)]
    members: Vec<LocalMember>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LocalMember {
    /// The console address the drive lives on.
    console: String,
    drive: String,
    member: String,
    path: String,
    /// The drive's name when this folder joined it (for messages; the console has the current one).
    #[serde(default)]
    name: String,
    /// The consoles the drive is linked to, as last read: where this folder syncs while its own
    /// console is away (8.1.0).
    #[serde(default)]
    links: Vec<String>,
    /// Paused on its own console, as last read: a paused folder does not fail over either.
    #[serde(default)]
    paused: bool,
}

fn state_dir() -> Option<PathBuf> {
    Some(crate::remote::store::data_dir()?.join("mesh-drive"))
}

fn config_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn load_config() -> LocalConfig {
    let Some(p) = state_dir().map(|d| d.join("config.json")) else {
        return LocalConfig::default();
    };
    std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_config(c: &LocalConfig) -> Result<()> {
    let dir = state_dir().ok_or_else(|| anyhow!("no data directory"))?;
    std::fs::create_dir_all(&dir)?;
    write_atomic(&dir.join("config.json"), &serde_json::to_vec_pretty(c)?)
}

fn edit_config(f: impl FnOnce(&mut LocalConfig)) -> Result<()> {
    let _g = config_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut c = load_config();
    f(&mut c);
    save_config(&c)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn base_path(drive: &str, member: &str) -> Option<PathBuf> {
    Some(state_dir()?.join(format!("{drive}-{member}.json")))
}

fn load_base(drive: &str, member: &str) -> Base {
    base_path(drive, member)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_base(drive: &str, member: &str, base: &Base) -> Result<()> {
    let p = base_path(drive, member).ok_or_else(|| anyhow!("no data directory"))?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    write_atomic(&p, &serde_json::to_vec(base)?)
}

// ---- status ----

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    /// idle, syncing, waiting (another device is syncing), offline, paused, pending (not let in
    /// yet), confirm (a mass delete waits), error, removed.
    pub state: String,
    pub detail: String,
    pub last_ok: u64,
    pub files: usize,
    pub uploaded: usize,
    pub downloaded: usize,
    pub deleted: usize,
    pub conflicts: usize,
    /// Deletes held back for confirmation.
    pub held_deletes: usize,
    /// The first of them (see [`HELD_SHOWN`]), so a person can see what they are confirming.
    pub held_paths: Vec<String>,
    /// The drive's size, as this folder and the console last agreed on it.
    pub bytes: u64,
    /// The pass running now: how many changes, how many done, the file at hand.
    pub todo: usize,
    pub done: usize,
    pub current: String,
    /// Files left out: they kept failing (tried again later), or this device cannot hold their name.
    pub skipped: Vec<String>,
    /// While the folder's own console is away: `rest`, `unreachable` or `helper` (empty
    /// otherwise), and since when (unix ms). Set with `offline`, and with `idle` while a linked
    /// console stands in.
    pub away: String,
    pub away_since: u64,
}

/// What a member's engine remembers between passes (not saved: a restart lists everything again).
#[derive(Default)]
struct Runtime {
    /// This folder as the last pass that changed nothing saw it.
    quiet_local: Option<Listing>,
    hub_rev: u64,
    last_full: Option<std::time::Instant>,
    /// Per file: failures in a row, and when to try again.
    fails: HashMap<String, (u32, std::time::Instant)>,
    /// A link: the other console's drive revision at the last quiet pass.
    side_rev: Option<u64>,
    /// Home console reads that failed in a row (failover waits for two).
    home_misses: u32,
}

fn runtimes() -> &'static Mutex<HashMap<String, Runtime>> {
    static R: OnceLock<Mutex<HashMap<String, Runtime>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn with_runtime<T>(member: &str, f: impl FnOnce(&mut Runtime) -> T) -> T {
    let mut m = runtimes().lock().unwrap_or_else(|e| e.into_inner());
    f(m.entry(member.to_string()).or_default())
}

/// Without a change here or a newer hub_rev, the console's folder is listed again this often
/// anyway (a file put there with Files, not through the drive, shows up by then).
const FULL_SCAN: Duration = Duration::from_secs(120);
/// A file that failed this many times in a row rests this long.
const FAIL_LIMIT: u32 = 3;
const FAIL_REST: Duration = Duration::from_secs(600);
/// A member's report on the console is written again at least this often.
const REPORT_EVERY_MS: u64 = 10 * 60 * 1000;

// ---- consoles that are away (8.2.0) ----

/// Why a console cannot be synced with right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AwayWhy {
    /// Rest mode (DDP said 620): every port refuses until it wakes.
    Rest,
    /// Nothing answers: off, on another network, or in rest without its network features.
    Unreachable,
    /// Awake, but the helper is not running (it does not survive rest mode).
    Helper,
}

impl AwayWhy {
    fn as_str(self) -> &'static str {
        match self {
            AwayWhy::Rest => "rest",
            AwayWhy::Unreachable => "unreachable",
            AwayWhy::Helper => "helper",
        }
    }
}

struct Away {
    why: AwayWhy,
    /// Unix ms it was first found away.
    since: u64,
    /// Tries that failed since; the next one waits longer.
    tries: u32,
    next: std::time::Instant,
    /// When DDP was last asked whether it woke (once per round is plenty).
    probed: Option<std::time::Instant>,
}

fn aways() -> &'static Mutex<HashMap<String, Away>> {
    static A: OnceLock<Mutex<HashMap<String, Away>>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// How often a sleeping or silent console is asked over DDP whether it woke.
const DDP_EVERY: Duration = Duration::from_secs(20);

/// The wait after `tries` failed tries in a row: 30 s, 1, 2, 4 min, then 5 min.
fn away_backoff(tries: u32) -> Duration {
    let secs = 30u64.saturating_mul(1 << tries.saturating_sub(1).min(4));
    Duration::from_secs(secs.min(300))
}

/// Why `e` means the console is away (not a fault of the file or the folder), when it does.
fn away_why(e: &anyhow::Error) -> Option<AwayWhy> {
    use ps5upload_core::mgmt::{error_token, AVA1_UNREACHABLE, CONSOLE_REST_MODE, HELPER_NOT_AVA1};
    match error_token(e).as_deref() {
        Some(CONSOLE_REST_MODE) => Some(AwayWhy::Rest),
        Some(AVA1_UNREACHABLE) => Some(AwayWhy::Unreachable),
        Some(HELPER_NOT_AVA1) => Some(AwayWhy::Helper),
        _ => None,
    }
}

/// After an error of no known kind: does the console still answer? One read of the drive list
/// decides (a dropped session mid-copy surfaces as a plain I/O error).
fn confirm_away(addr: &str, e: &anyhow::Error) -> Option<AwayWhy> {
    if let Some(w) = away_why(e) {
        return Some(w);
    }
    match doc_read(addr) {
        Err(e2) => Some(away_why(&e2).unwrap_or(AwayWhy::Unreachable)),
        Ok(_) => None,
    }
}

fn mark_away(addr: &str, why: AwayWhy) {
    let mut m = aways().lock().unwrap_or_else(|e| e.into_inner());
    let a = m.entry(addr.to_string()).or_insert_with(|| {
        crate::log_info!("mesh drive: {addr} is away ({})", why.as_str());
        Away {
            why,
            since: now_ms(),
            tries: 0,
            next: std::time::Instant::now(),
            probed: None,
        }
    });
    a.why = why;
    a.tries += 1;
    // A missing helper is usually sent again within a minute (the app does it on its own for
    // the console it shows): look again sooner than for a console that sleeps.
    let wait = match why {
        AwayWhy::Helper => away_backoff(a.tries).min(Duration::from_secs(60)),
        _ => away_backoff(a.tries),
    };
    a.next = std::time::Instant::now() + wait;
}

/// The console answered: it is back. Its folders look at everything again (the quiet shortcut
/// and per-file failures from before are forgotten).
fn mark_back(addr: &str) {
    let was = aways()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(addr);
    if let Some(a) = was {
        crate::log_info!(
            "mesh drive: {addr} is back after {} s",
            now_ms().saturating_sub(a.since) / 1000
        );
        let members: Vec<String> = load_config()
            .members
            .iter()
            .filter(|m| m.console == addr)
            .map(|m| m.member.clone())
            .collect();
        let mut rts = runtimes().lock().unwrap_or_else(|e| e.into_inner());
        for (id, rt) in rts.iter_mut() {
            if members.contains(id) || id.starts_with("link:") {
                rt.quiet_local = None;
                rt.side_rev = None;
                rt.last_full = None;
                rt.fails.clear();
            }
        }
    }
}

/// `(why, since)` while `addr` is away.
fn away_of(addr: &str) -> Option<(AwayWhy, u64)> {
    aways()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(addr)
        .map(|a| (a.why, a.since))
}

/// Whether to try `addr` this round: not away, its wait is over, or (asleep or silent) DDP now
/// says it is awake, so a wake is noticed within a round instead of after the wait.
fn worth_trying(addr: &str) -> bool {
    let (why, due) = {
        let mut m = aways().lock().unwrap_or_else(|e| e.into_inner());
        match m.get_mut(addr) {
            None => return true,
            Some(a) => {
                if a.next <= std::time::Instant::now() {
                    return true;
                }
                let fresh = a.probed.is_some_and(|t| t.elapsed() < DDP_EVERY);
                if !fresh {
                    a.probed = Some(std::time::Instant::now());
                }
                (a.why, fresh)
            }
        }
    };
    // Asked DDP a moment ago (another folder on the same console this round): same answer.
    if due {
        return false;
    }
    match why {
        AwayWhy::Helper => false,
        AwayWhy::Rest | AwayWhy::Unreachable => {
            // `ip`, `ip:port`, `[v6]` or `[v6]:port`: DDP wants the bare host.
            let host = match addr.strip_prefix('[') {
                Some(rest) => rest.split(']').next().unwrap_or(rest),
                None if addr.matches(':').count() == 1 => addr.split(':').next().unwrap_or(addr),
                None => addr,
            };
            let code = ps5upload_core::ddp::standby_probe(
                host,
                ps5upload_core::ddp::STANDBY_PROBE_BUDGET,
                ps5upload_core::ddp::STANDBY_PROBE_SENDS,
            );
            match (why, code) {
                (_, Some(200)) => true,
                // Silent before, asleep now: still asleep, but say so.
                (AwayWhy::Unreachable, Some(620)) => {
                    if let Some(a) = aways()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_mut(addr)
                    {
                        a.why = AwayWhy::Rest;
                    }
                    false
                }
                _ => false,
            }
        }
    }
}

/// A folder's status while its console is away.
fn set_away_status(member: &str, addr: &str) {
    let (why, since) = away_of(addr).unwrap_or((AwayWhy::Unreachable, now_ms()));
    set_status(member, |s| {
        s.state = "offline".into();
        s.away = why.as_str().into();
        s.away_since = since;
        s.detail = match why {
            AwayWhy::Rest => {
                format!("{addr} is in rest mode; changes made here sync when it wakes")
            }
            AwayWhy::Unreachable => {
                format!("{addr} does not answer; changes made here sync when it is back")
            }
            AwayWhy::Helper => {
                format!("{addr} is on but its helper is not running; send it and syncing resumes")
            }
        };
        s.todo = 0;
        s.done = 0;
        s.current.clear();
    });
}

/// A name this device's file system cannot hold (only Windows refuses names the console takes).
fn unstorable(rel: &str) -> bool {
    if !cfg!(windows) {
        return false;
    }
    rel.split('/').any(|c| {
        let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
        c.ends_with('.')
            || c.ends_with(' ')
            || c.chars()
                .any(|ch| "<>:\"|?*".contains(ch) || (ch as u32) < 32)
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit())
    })
}

/// Paths here and on the console that differ only in case, on a device whose folders ignore case
/// (`Foo.txt` here, `foo.txt` there): one would land on the other, so neither side moves them.
fn case_clashes(
    member: &Listing,
    hub: &Listing,
    case_insensitive: bool,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if !case_insensitive {
        return out;
    }
    let low: HashMap<String, &String> = hub.keys().map(|k| (k.to_lowercase(), k)).collect();
    for k in member.keys() {
        if let Some(h) = low.get(&k.to_lowercase()) {
            if *h != k {
                out.insert(k.clone());
                out.insert((*h).clone());
            }
        }
    }
    out
}

/// Paths of the console's folder that differ only in case from an earlier one: on a device whose
/// folders ignore case they would land on the same file.
fn case_twins(hub: &Listing, case_insensitive: bool) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if !case_insensitive {
        return out;
    }
    let mut seen: HashMap<String, &str> = HashMap::new();
    let mut keys: Vec<&String> = hub.keys().collect();
    keys.sort();
    for k in keys {
        let low = k.to_lowercase();
        if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(low) {
            e.insert(k);
        } else {
            out.insert(k.clone());
        }
    }
    out
}

fn statuses() -> &'static Mutex<HashMap<String, Status>> {
    static S: OnceLock<Mutex<HashMap<String, Status>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn set_status(member: &str, f: impl FnOnce(&mut Status)) {
    let mut m = statuses().lock().unwrap_or_else(|e| e.into_inner());
    f(m.entry(member.to_string()).or_default());
}

fn confirmed() -> &'static Mutex<std::collections::HashSet<String>> {
    static C: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

// ---- scanning ----

fn walk_local(root: &Path) -> Result<Listing> {
    let mut out = Listing::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if ignored(&name) {
                continue;
            }
            let ft = e.file_type()?;
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if ft.is_dir() {
                stack.push((e.path(), child));
            } else if ft.is_file() {
                let md = e.metadata()?;
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64);
                out.insert(
                    child,
                    Meta {
                        size: md.len(),
                        mtime,
                    },
                );
                if out.len() > MAX_FILES {
                    bail!("more than {MAX_FILES} files: too many for a Mesh Drive");
                }
            }
        }
    }
    Ok(out)
}

fn join_hub(hub: &str, rel: &str) -> String {
    format!("{}/{rel}", hub.trim_end_matches('/'))
}

fn walk_hub(addr: &str, hub: &str) -> Result<Listing> {
    let mut out = Listing::new();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        let dir = if rel.is_empty() {
            hub.trim_end_matches('/').to_string()
        } else {
            join_hub(hub, &rel)
        };
        let mut offset = 0u64;
        loop {
            let l = fs_ops::list_dir(addr, &dir, ListDirOptions { offset, limit: 256 })?;
            let n = l.entries.len() as u64;
            for e in l.entries {
                if ignored(&e.name) || e.name == "." || e.name == ".." {
                    continue;
                }
                let child = if rel.is_empty() {
                    e.name.clone()
                } else {
                    format!("{rel}/{}", e.name)
                };
                match e.kind.as_str() {
                    "dir" => stack.push(child),
                    "file" => {
                        out.insert(
                            child,
                            Meta {
                                size: e.size,
                                mtime: e.mtime,
                            },
                        );
                        if out.len() > MAX_FILES {
                            bail!("more than {MAX_FILES} files: too many for a Mesh Drive");
                        }
                    }
                    _ => {}
                }
            }
            if n < 256 && !l.truncated {
                break;
            }
            if n == 0 {
                break;
            }
            offset += n;
        }
    }
    Ok(out)
}

fn local_stat(p: &Path) -> Option<Meta> {
    let md = std::fs::metadata(p).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    Some(Meta {
        size: md.len(),
        mtime,
    })
}

fn hub_stat(addr: &str, path: &str) -> Option<Meta> {
    let s = fs_ops::fs_stat(addr, path).ok()?;
    (s.kind == "file").then_some(Meta {
        size: s.size,
        mtime: s.mtime as i64,
    })
}

fn local_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// No path that leaves the folder, whatever a listing said.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && rel
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\\'))
}

// ---- one pass ----

/// What a pass changed: the hub, and the member's side.
#[derive(Clone, Copy, Default)]
struct Changed {
    hub: bool,
    side: bool,
    /// The pass got as far as planning (a held mass delete was looked at).
    planned: bool,
}

struct Pass<'a> {
    addr: &'a str,
    drive: &'a Drive,
    member_id: String,
    /// This member's copy: a folder on this device, or on a second console.
    side: &'a dyn Side,
    /// Names this member's conflict copies.
    device_name: String,
    /// The side's own revision when it is another console (its drive's hub_rev): with it and
    /// the hub's unchanged, a link pass lists neither console until FULL_SCAN.
    side_rev: Option<u64>,
    /// Set by the lock's heartbeat when another engine holds the drive now: the pass stops.
    lost: std::sync::Arc<AtomicBool>,
}

/// How many held deletes are named in a status: enough to tell a moved folder from a clean-up.
const HELD_SHOWN: usize = 200;

/// The paths of the deletes in `ops`, the first [`HELD_SHOWN`].
fn held_shown(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .filter(|o| o.is_delete())
        .take(HELD_SHOWN)
        .map(|o| o.path().to_string())
        .collect()
}

/// The drive's lock, held while a pass runs: a heartbeat renews it however long one file takes,
/// and dropping it (a panic included) stops the heartbeat and releases the lock.
struct DriveLease {
    addr: String,
    name: String,
    stop: std::sync::Arc<AtomicBool>,
    /// Shared with the pass (and, for a link, with the other console's lease): stops it.
    lost: std::sync::Arc<AtomicBool>,
    /// This console's lock went to someone else: not ours to release.
    own_lost: std::sync::Arc<AtomicBool>,
    beat: Option<std::thread::JoinHandle<()>>,
}

impl DriveLease {
    fn hold(addr: &str, name: &str, label: &str) -> Self {
        Self::hold_with(
            addr,
            name,
            label,
            std::sync::Arc::new(AtomicBool::new(false)),
        )
    }

    /// Holds the lock with `lost` shared (a link holds the drive's lock on two consoles: losing
    /// either stops the pass).
    fn hold_with(addr: &str, name: &str, label: &str, lost: std::sync::Arc<AtomicBool>) -> Self {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let own_lost = std::sync::Arc::new(AtomicBool::new(false));
        let (a, n, l, st, lo, own) = (
            addr.to_string(),
            name.to_string(),
            label.to_string(),
            stop.clone(),
            lost.clone(),
            own_lost.clone(),
        );
        let beat = std::thread::Builder::new()
            .name("mesh-drive-lease".into())
            .spawn(move || {
                let mut since = std::time::Instant::now();
                while !st.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(500));
                    if since.elapsed() < LOCK_RENEW {
                        continue;
                    }
                    since = std::time::Instant::now();
                    let r = crate::mesh_api::call(&a, || {
                        mesh::lock(&a, "take", &n, LOCK_TTL_S, false, &l, "")
                    });
                    // Only a clear "someone else has it" counts; a missed renewal is retried.
                    if let Ok(Some(v)) = r {
                        if v.get("got").and_then(Value::as_bool) == Some(false) {
                            own.store(true, Ordering::SeqCst);
                            lo.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }
            })
            .ok();
        Self {
            addr: addr.to_string(),
            name: name.to_string(),
            stop,
            lost,
            own_lost,
            beat,
        }
    }
}

impl Drop for DriveLease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(b) = self.beat.take() {
            let _ = b.join();
        }
        // The other console's loss (a link) still leaves this one's lock ours to give back.
        if !self.own_lost.load(Ordering::SeqCst) {
            let (a, n) = (self.addr.as_str(), self.name.as_str());
            let _ = crate::mesh_api::call(a, || mesh::lock(a, "release", n, 0, false, "", ""));
        }
    }
}

/// `rel`, or `rel` with " (2)", " (3)"... before its extension when `taken` says that name is used.
fn free_name(rel: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(rel) {
        return rel.to_string();
    }
    let (dir, file) = rel.rsplit_once('/').map_or(("", rel), |(d, f)| (d, f));
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], &file[i..]),
        _ => (file, ""),
    };
    for n in 2..10_000 {
        let name = format!("{stem} ({n}){ext}");
        let cand = if dir.is_empty() {
            name
        } else {
            format!("{dir}/{name}")
        };
        if !taken(&cand) {
            return cand;
        }
    }
    format!("{rel}.{}", new_id())
}

fn is_os_clutter(name: &str) -> bool {
    name == ".DS_Store" || name == "Thumbs.db" || name == "desktop.ini" || name.starts_with("._")
}

/// A console folder of the drive that nothing is left in goes to the trash (moved, never
/// deleted: anything written into it meanwhile is still there).
fn prune_console_dir(addr: &str, root: &str, rel: &str, today: &str) -> bool {
    let dir = join_hub(root, rel);
    let l = match fs_ops::list_dir(
        addr,
        &dir,
        ListDirOptions {
            offset: 0,
            limit: 8,
        },
    ) {
        Ok(l) => l,
        Err(e) => {
            crate::log_info!("mesh drive: kept {dir}: cannot list it ({e:#})");
            return false;
        }
    };
    if let Some(e) = l
        .entries
        .iter()
        .find(|e| e.name != "." && e.name != ".." && !is_os_clutter(&e.name))
    {
        crate::log_info!("mesh drive: kept {dir}: it still holds {}", e.name);
        return false;
    }
    let day = join_hub(&join_hub(root, TRASH), today);
    let to = join_hub(
        &day,
        &free_name(rel, |r| hub_stat(addr, &join_hub(&day, r)).is_some()),
    );
    if let Some(parent) = to.rsplit_once('/').map(|(d, _)| d) {
        if let Err(e) = fs_ops::fs_mkdir(addr, parent) {
            crate::log_info!("mesh drive: kept {dir}: cannot make {parent} ({e:#})");
            return false;
        }
    }
    // The console finishes moving the folder's files into the trash a moment after the pass
    // has moved on, so the name picked above can be taken by the time the move runs: then the
    // folder goes under a name nothing else uses.
    let moved = fs_ops::fs_move(addr, &dir, &to)
        .or_else(|_| fs_ops::fs_move(addr, &dir, &format!("{to} ({})", new_id())));
    match moved {
        Ok(()) => true,
        Err(e) => {
            crate::log_info!("mesh drive: kept {dir}: cannot move it to the trash ({e:#})");
            false
        }
    }
}

/// The folders `deleted` files lay in, deepest first, without the drive's root.
fn emptied_folders<'a>(deleted: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut dirs = std::collections::BTreeSet::new();
    for rel in deleted {
        let mut p = rel.as_str();
        while let Some((parent, _)) = p.rsplit_once('/') {
            dirs.insert(parent.to_string());
            p = parent;
        }
    }
    let mut v: Vec<String> = dirs.into_iter().collect();
    v.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    v
}

// ---- the member's side: this device's folder, or a folder on another console (8.1.0) ----

/// Where a member's copy lives. A pass reads and changes it only through this, so the same
/// planner, safety rules and trash serve a folder on this device and one on a second console.
trait Side {
    /// For messages: the folder, and where it is.
    fn describe(&self) -> String;
    fn has_marker(&self) -> bool;
    fn walk(&self) -> Result<Listing>;
    fn stat(&self, rel: &str) -> Option<Meta>;
    fn exists(&self, rel: &str) -> bool;
    /// This side's `rel` to the hub's `rel`.
    fn send(&self, hub_addr: &str, hub: &str, rel: &str) -> Result<()>;
    /// Several files to the hub in one job, when this side can (a folder on this device):
    /// false when it cannot, and each goes with `send`.
    fn send_many(&self, _hub_addr: &str, _hub: &str, _rels: &[String]) -> Result<bool> {
        Ok(false)
    }
    /// The hub's `rel` to this side; never over a file that is no longer `expect`.
    fn fetch(&self, hub_addr: &str, hub: &str, rel: &str, expect: Option<Meta>) -> Result<()>;
    /// BLAKE3, hex.
    fn hash(&self, rel: &str) -> Result<String>;
    fn rename(&self, from: &str, to: &str) -> Result<()>;
    fn trash(&self, rel: &str, today: &str) -> Result<()>;
    /// Removes the folder `rel` when nothing of the drive is left in it (a folder deleted on the
    /// other side leaves its files' folders behind here otherwise). False when it holds anything.
    fn prune(&self, rel: &str, today: &str) -> bool;
    fn empty_trash(&self, cutoff: &str);
    /// What this side and the hub last agreed on. Empty only when there is none yet: a base that
    /// cannot be read is an error (a pass against an empty one would hash everything and bring
    /// deleted files back).
    fn load_base(&self, drive: &str, member: &str) -> Result<Base>;
    fn save_base(&self, drive: &str, member: &str, base: &Base) -> Result<()>;
    /// Whether a just-written file waits a pass (times come from this computer's clock).
    fn settles(&self) -> bool;
    /// A name this side cannot hold.
    fn unstorable(&self, rel: &str) -> bool;
    fn case_insensitive(&self) -> bool;
    /// The console this side is on, when it is one (a link's far end).
    fn console(&self) -> Option<&str> {
        None
    }
}

struct LocalSide {
    root: PathBuf,
    /// "" for the home console; the other console's address while failing over to it (its base
    /// is kept apart: what this folder agreed with one console says nothing about another).
    via: String,
}

impl LocalSide {
    fn base_key(&self, member: &str) -> String {
        if self.via.is_empty() {
            member.to_string()
        } else {
            let tag: String = self
                .via
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            format!("{member}@{tag}")
        }
    }
}

impl Side for LocalSide {
    fn describe(&self) -> String {
        self.root.display().to_string()
    }
    fn has_marker(&self) -> bool {
        self.root.join(MARKER).is_file()
    }
    fn walk(&self) -> Result<Listing> {
        walk_local(&self.root)
    }
    fn stat(&self, rel: &str) -> Option<Meta> {
        local_stat(&local_path(&self.root, rel))
    }
    fn exists(&self, rel: &str) -> bool {
        local_path(&self.root, rel).symlink_metadata().is_ok()
    }
    fn send(&self, hub_addr: &str, hub: &str, rel: &str) -> Result<()> {
        let src = local_path(&self.root, rel);
        let dest = join_hub(hub, rel);
        if let Some(parent) = dest.rsplit_once('/').map(|(d, _)| d) {
            fs_ops::fs_mkdir(hub_addr, parent)?;
        }
        let mut cfg = crate::make_transfer_config(hub_addr);
        cfg.cancel = Some(xfer_cancel());
        let job: [u8; 16] = ava1::keys::random_bytes()?;
        ps5upload_ava1::upload::upload_file(&cfg, job, &dest, &src)
            .map_err(|e| anyhow!("upload {rel}: {e:#}"))?;
        Ok(())
    }
    fn send_many(&self, hub_addr: &str, hub: &str, rels: &[String]) -> Result<bool> {
        let entries: Vec<ps5upload_core::transfer::FileListEntry> = rels
            .iter()
            .map(|rel| ps5upload_core::transfer::FileListEntry {
                src: local_path(&self.root, rel).to_string_lossy().into_owned(),
                dest: join_hub(hub, rel),
            })
            .collect();
        let mut cfg = crate::make_transfer_config(hub_addr);
        cfg.cancel = Some(xfer_cancel());
        let job: [u8; 16] = ava1::keys::random_bytes()?;
        ps5upload_ava1::upload::upload_list(&cfg, job, hub, &entries)
            .map_err(|e| anyhow!("upload {} files: {e:#}", rels.len()))?;
        Ok(true)
    }
    fn fetch(&self, hub_addr: &str, hub: &str, rel: &str, expect: Option<Meta>) -> Result<()> {
        let src = join_hub(hub, rel);
        let target = local_path(&self.root, rel);
        let tmp_dir = self.root.join(TMP).join(new_id());
        std::fs::create_dir_all(&tmp_dir)?;
        let job: [u8; 16] = ava1::keys::random_bytes()?;
        let r = ps5upload_ava1::download::to_local(
            hub_addr,
            &src,
            ps5upload_core::download::DownloadKind::File,
            &tmp_dir,
            false,
            job,
            &ps5upload_ava1::download::Counters::default(),
            Some(xfer_cancel()),
        );
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let got = tmp_dir.join(name);
        let res = r
            .map_err(|e| anyhow!("download {rel}: {e:#}"))
            .and_then(|_| {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if local_stat(&target) != expect {
                    bail!(
                        "{rel} changed on this device while it was syncing; it is synced next pass"
                    );
                }
                std::fs::rename(&got, &target).with_context(|| format!("place {rel}"))
            });
        let _ = std::fs::remove_dir_all(&tmp_dir);
        // Only goes when nothing else is downloading into it.
        let _ = std::fs::remove_dir(self.root.join(TMP));
        res
    }
    fn hash(&self, rel: &str) -> Result<String> {
        let mut h = blake3::Hasher::new();
        let mut f = std::fs::File::open(local_path(&self.root, rel))?;
        std::io::copy(&mut f, &mut h)?;
        Ok(h.finalize().to_hex().to_string())
    }
    fn rename(&self, from: &str, to: &str) -> Result<()> {
        std::fs::rename(local_path(&self.root, from), local_path(&self.root, to))
            .with_context(|| format!("rename {from}"))
    }
    fn trash(&self, rel: &str, today: &str) -> Result<()> {
        let from = local_path(&self.root, rel);
        let day = self.root.join(TRASH).join(today);
        // Deleted twice in a day: the second copy gets a new name, never replaces the first.
        let to = local_path(
            &day,
            &free_name(rel, |r| local_path(&day, r).symlink_metadata().is_ok()),
        );
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&from, &to).with_context(|| format!("move {rel} to the trash"))
    }
    fn prune(&self, rel: &str, _today: &str) -> bool {
        let dir = local_path(&self.root, rel);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return false;
        };
        let names: Vec<std::ffi::OsString> = rd.flatten().map(|e| e.file_name()).collect();
        // Only the clutter an OS leaves (.DS_Store, Thumbs.db, ._ files) may stay behind it.
        if !names.iter().all(|n| is_os_clutter(&n.to_string_lossy())) {
            return false;
        }
        for n in &names {
            let _ = std::fs::remove_file(dir.join(n));
        }
        std::fs::remove_dir(&dir).is_ok()
    }
    fn empty_trash(&self, cutoff: &str) {
        if let Ok(rd) = std::fs::read_dir(self.root.join(TRASH)) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.len() == 10 && n.as_str() < cutoff {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
    fn load_base(&self, drive: &str, member: &str) -> Result<Base> {
        Ok(load_base(drive, &self.base_key(member)))
    }
    fn save_base(&self, drive: &str, member: &str, base: &Base) -> Result<()> {
        save_base(drive, &self.base_key(member), base)
    }
    fn settles(&self) -> bool {
        true
    }
    fn unstorable(&self, rel: &str) -> bool {
        unstorable(rel)
    }
    fn case_insensitive(&self) -> bool {
        cfg!(windows) || cfg!(target_os = "macos")
    }
}

/// A folder on a second console. Bytes go console to console (directly when the consoles can
/// reach each other, else through this computer); the base lives in the folder itself
/// (BASE_FILE), so whichever engine runs the next pass starts from the same agreement.
struct ConsoleSide {
    addr: String,
    root: String,
}

/// A base larger than this is refused (a small-file write on the console holds 16 MiB).
const CONSOLE_BASE_MAX: usize = 16 * 1024 * 1024;

impl ConsoleSide {
    fn path(&self, rel: &str) -> String {
        join_hub(&self.root, rel)
    }
}

thread_local! {
    /// The running pass's cancel flag: its copies stop when it flips (see [`Watchdog`]).
    static XFER_CANCEL: std::cell::RefCell<Option<std::sync::Arc<AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

/// The flag a copy started now obeys: the running pass's, or one that never flips.
fn xfer_cancel() -> std::sync::Arc<AtomicBool> {
    XFER_CANCEL
        .with(|c| c.borrow().clone())
        .unwrap_or_else(|| std::sync::Arc::new(AtomicBool::new(false)))
}

/// A batch of uploads holds at most this many files, or this many bytes (a batch is one job: the
/// pass's progress moves, and what it did is saved, between batches).
const BATCH_FILES: usize = 200;
const BATCH_BYTES: u64 = 512 << 20;

/// How often a running pass checks that its consoles still answer.
const WATCH_EVERY: Duration = Duration::from_secs(10);

/// Watches a pass's consoles while it copies. A copy waits out a dropped connection for as long
/// as it takes (right for a person's upload, which resumes when the console is back), so a
/// console put to rest mid-copy would hold the pass, and every drive behind it, until it woke.
/// Two missed checks in a row (or the drive's lock lost) flip the pass's cancel flag: the copy
/// stops, the pass finds the console away and ends, keeping what it did.
struct Watchdog {
    stop: std::sync::Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn start(consoles: Vec<String>, lost: std::sync::Arc<AtomicBool>) -> Self {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        XFER_CANCEL.with(|c| *c.borrow_mut() = Some(cancel.clone()));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let thread = std::thread::Builder::new()
            .name("mesh-drive-watch".into())
            .spawn(move || {
                let mut misses = 0;
                let mut last = std::time::Instant::now();
                while !st.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(500));
                    if lost.load(Ordering::SeqCst) {
                        cancel.store(true, Ordering::SeqCst);
                        return;
                    }
                    if last.elapsed() < WATCH_EVERY {
                        continue;
                    }
                    last = std::time::Instant::now();
                    let answers = consoles.iter().all(|c| {
                        crate::mesh_api::call(c, || mesh::state(c, u64::MAX, u64::MAX)).is_ok()
                    });
                    misses = if answers { 0 } else { misses + 1 };
                    if misses >= 2 {
                        crate::log_info!(
                            "mesh drive: a console stopped answering mid-pass; its copy stops"
                        );
                        cancel.store(true, Ordering::SeqCst);
                        return;
                    }
                }
            })
            .ok();
        Self { stop, thread }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        XFER_CANCEL.with(|c| *c.borrow_mut() = None);
    }
}

/// Copies one file between consoles: directly when they can reach each other, else through
/// this computer.
fn console_copy(from: &str, src: &str, to: &str, dest: &str) -> Result<()> {
    if let Some(parent) = dest.rsplit_once('/').map(|(d, _)| d) {
        fs_ops::fs_mkdir(to, parent)?;
    }
    let job: [u8; 16] = ava1::keys::random_bytes()?;
    ps5upload_ava1::c2c::ps5_to_ps5_routed(
        from,
        src,
        to,
        dest,
        job,
        std::sync::Arc::default(),
        xfer_cancel(),
        None,
        &ps5upload_ava1::c2c::RouteOptions::default(),
        |_| {},
    )
    .map(|_| ())
    .map_err(|e| anyhow!("copy {src}: {e:#}"))
}

impl Side for ConsoleSide {
    fn describe(&self) -> String {
        format!("{} on {}", self.root, self.addr)
    }
    fn console(&self) -> Option<&str> {
        Some(&self.addr)
    }
    fn has_marker(&self) -> bool {
        hub_stat(&self.addr, &self.path(MARKER)).is_some()
    }
    fn walk(&self) -> Result<Listing> {
        walk_hub(&self.addr, &self.root)
    }
    fn stat(&self, rel: &str) -> Option<Meta> {
        hub_stat(&self.addr, &self.path(rel))
    }
    fn exists(&self, rel: &str) -> bool {
        fs_ops::fs_stat(&self.addr, &self.path(rel)).is_ok()
    }
    fn send(&self, hub_addr: &str, hub: &str, rel: &str) -> Result<()> {
        console_copy(&self.addr, &self.path(rel), hub_addr, &join_hub(hub, rel))
    }
    fn fetch(&self, hub_addr: &str, hub: &str, rel: &str, expect: Option<Meta>) -> Result<()> {
        // Into a temporary name first, then into place once it is known nothing changed here.
        let tmp = join_hub(&join_hub(&self.root, TMP), &new_id());
        let target = self.path(rel);
        let r = console_copy(hub_addr, &join_hub(hub, rel), &self.addr, &tmp).and_then(|_| {
            if self.stat(rel) != expect {
                bail!(
                    "{rel} changed on {} while it was syncing; it is synced next pass",
                    self.addr
                );
            }
            if let Some(parent) = target.rsplit_once('/').map(|(d, _)| d) {
                fs_ops::fs_mkdir(&self.addr, parent)?;
            }
            if expect.is_some() {
                fs_ops::fs_delete(&self.addr, &target)?;
            }
            fs_ops::fs_move(&self.addr, &tmp, &target)
        });
        if r.is_err() {
            let _ = fs_ops::fs_delete(&self.addr, &tmp);
        }
        // The temporary folder goes once empty (one pass runs at a time: the drive's lock).
        let tmp_dir = join_hub(&self.root, TMP);
        if fs_ops::list_dir(&self.addr, &tmp_dir, ListDirOptions::default())
            .is_ok_and(|l| l.entries.iter().all(|e| e.name == "." || e.name == ".."))
        {
            let _ = fs_ops::fs_delete(&self.addr, &tmp_dir);
        }
        r
    }
    fn hash(&self, rel: &str) -> Result<String> {
        Ok(fs_ops::fs_hash(&self.addr, &self.path(rel))?
            .hash
            .to_ascii_lowercase())
    }
    fn rename(&self, from: &str, to: &str) -> Result<()> {
        fs_ops::fs_move(&self.addr, &self.path(from), &self.path(to))
    }
    fn trash(&self, rel: &str, today: &str) -> Result<()> {
        let day = join_hub(&join_hub(&self.root, TRASH), today);
        let to = join_hub(
            &day,
            &free_name(rel, |r| hub_stat(&self.addr, &join_hub(&day, r)).is_some()),
        );
        if let Some(parent) = to.rsplit_once('/').map(|(d, _)| d) {
            fs_ops::fs_mkdir(&self.addr, parent)?;
        }
        fs_ops::fs_move(&self.addr, &self.path(rel), &to)
    }
    fn prune(&self, rel: &str, today: &str) -> bool {
        prune_console_dir(&self.addr, &self.root, rel, today)
    }
    fn empty_trash(&self, cutoff: &str) {
        empty_console_trash(&self.addr, &self.root, cutoff);
    }
    fn load_base(&self, _drive: &str, _member: &str) -> Result<Base> {
        let path = join_hub(&join_hub(&self.root, TRASH), LINK_BASE);
        let bytes = match read_console_file(&self.addr, &path)? {
            Some(b) => b,
            // Where 8.1.0 kept it (in the folder itself, which an 8.0 engine syncs as a file).
            None => match read_console_file(&self.addr, &self.path(BASE_FILE))? {
                Some(b) => b,
                None => return Ok(Base::new()),
            },
        };
        serde_json::from_slice(&bytes)
            .map_err(|e| anyhow!("the link's record on {} is damaged: {e}", self.addr))
    }
    fn save_base(&self, _drive: &str, _member: &str, base: &Base) -> Result<()> {
        let body = serde_json::to_vec(base)?;
        if body.len() > CONSOLE_BASE_MAX {
            bail!("too many files for a drive folder on a second console");
        }
        let dir = join_hub(&self.root, TRASH);
        fs_ops::fs_mkdir(&self.addr, &dir)?;
        // Written whole or not at all (the write commits once).
        ps5upload_core::diagnostics::fs_write_bytes_large(
            &self.addr,
            &join_hub(&dir, LINK_BASE),
            &body,
            false,
        )?;
        let _ = fs_ops::fs_delete(&self.addr, &self.path(BASE_FILE));
        Ok(())
    }
    fn settles(&self) -> bool {
        false
    }
    fn unstorable(&self, _rel: &str) -> bool {
        false
    }
    fn case_insensitive(&self) -> bool {
        false
    }
}

/// A whole small file from a console (chunked reads). Ok(None) only when it is not there (its
/// folder answers); any other failure is an error.
fn read_console_file(addr: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let size = match fs_ops::fs_stat(addr, path) {
        Ok(st) => st.size as usize,
        Err(e) => {
            // The console answers for a folder above it (the trash folder may not exist yet, the
            // drive's folder does): the file is simply not there. Nothing answers: an error.
            let mut dir = path;
            while let Some((up, _)) = dir.rsplit_once('/') {
                if up.is_empty() {
                    break;
                }
                if fs_ops::fs_stat(addr, up).is_ok() {
                    return Ok(None);
                }
                dir = up;
            }
            return Err(e.context(format!("read {path}")));
        }
    };
    if size > CONSOLE_BASE_MAX {
        bail!("{path} is too large");
    }
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let chunk = fs_ops::fs_read(addr, path, out.len() as u64, (size - out.len()) as u64)?;
        if chunk.is_empty() {
            bail!("{path} ended early");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Some(out))
}

/// The link's record, inside the trash folder (which every version, 8.0 included, never syncs).
const LINK_BASE: &str = "link-base.json";

/// Trash folders on a console older than `cutoff` (a day folder name) go. Best effort.
fn empty_console_trash(addr: &str, root: &str, cutoff: &str) {
    let trash = join_hub(root, TRASH);
    if let Ok(l) = fs_ops::list_dir(addr, &trash, ListDirOptions::default()) {
        for e in l.entries {
            if e.kind == "dir" && e.name.len() == 10 && e.name.as_str() < cutoff {
                let _ = fs_ops::fs_delete(addr, &join_hub(&trash, &e.name));
            }
        }
    }
}

impl Pass<'_> {
    /// The name this member's copy of a conflicting file moves to, never one already used there.
    fn conflict_copy(&self, rel: &str, stamp: &str) -> String {
        free_name(&conflict_name(rel, &self.device_name, stamp), |r| {
            self.side.exists(r)
        })
    }

    /// Whether this member's file and the hub's hold the same bytes (BLAKE3 on both sides).
    fn same_bytes(&self, rel: &str) -> Result<bool> {
        let here = self.side.hash(rel)?;
        let hub = fs_ops::fs_hash(self.addr, &join_hub(&self.drive.hub, rel))?;
        Ok(hub.hash.eq_ignore_ascii_case(&here))
    }

    fn trash_hub(&self, rel: &str, today: &str) -> Result<()> {
        let from = join_hub(&self.drive.hub, rel);
        let day = join_hub(&join_hub(&self.drive.hub, TRASH), today);
        let to = join_hub(
            &day,
            &free_name(rel, |r| hub_stat(self.addr, &join_hub(&day, r)).is_some()),
        );
        if let Some(parent) = to.rsplit_once('/').map(|(d, _)| d) {
            fs_ops::fs_mkdir(self.addr, parent)?;
        }
        fs_ops::fs_move(self.addr, &from, &to)
    }

    /// Whether a console install is running (installs read the disk hard: syncing waits).
    fn install_running(&self) -> bool {
        crate::mesh_api::call(self.addr, || mesh::state(self.addr, 0, 0))
            .ok()
            .flatten()
            .and_then(|v| v.get("locks").and_then(Value::as_array).cloned())
            .is_some_and(|l| {
                l.iter()
                    .any(|x| x.get("name").and_then(Value::as_str) == Some("install"))
            })
    }

    /// After a failure: the console (the hub's, or the far end of a link) that went away, and
    /// why. None: the console answers, so the failure belongs to the file.
    fn gone(&self, e: &anyhow::Error) -> Option<(String, AwayWhy)> {
        if let Some(w) = confirm_away(self.addr, e) {
            return Some((self.addr.to_string(), w));
        }
        let other = self.side.console()?;
        let w = confirm_away(other, e)?;
        Some((other.to_string(), w))
    }

    /// One pass. Returns whether it changed the hub, and whether it changed this member's side.
    fn run(&self) -> Result<Changed> {
        let member_id = self.member_id.as_str();
        // Markers: both sides must be there.
        if !self.side.has_marker() {
            set_status(member_id, |s| {
                s.state = "offline".into();
                s.detail = format!(
                    "{} is missing its {MARKER} file (unplugged or moved?)",
                    self.side.describe()
                );
            });
            return Ok(Changed::default());
        }
        if hub_stat(self.addr, &join_hub(&self.drive.hub, MARKER)).is_none() {
            set_status(member_id, |s| {
                s.state = "offline".into();
                s.detail = format!(
                    "the drive's folder on the console ({}) is missing its {MARKER} file",
                    self.drive.hub
                );
            });
            return Ok(Changed::default());
        }
        if self.install_running() {
            set_status(member_id, |s| {
                s.state = "waiting".into();
                s.detail = "an install is running on the console".into();
            });
            return Ok(Changed::default());
        }
        // A link: nothing changed on either console through the drive since the last full look,
        // so neither is listed (a file put there another way shows by FULL_SCAN).
        if let Some(rev) = self.side_rev {
            let quiet = with_runtime(member_id, |rt| {
                rt.side_rev == Some(rev)
                    && rt.hub_rev == self.drive.hub_rev
                    && rt.last_full.is_some_and(|t| t.elapsed() < FULL_SCAN)
            });
            if quiet {
                return Ok(Changed::default());
            }
        }
        let mut member = self.side.walk()?;
        // Nothing changed here and nobody changed the console's folder through the drive: skip
        // listing it (the slow half of a pass) until FULL_SCAN says to look anyway.
        let quiet = with_runtime(member_id, |rt| {
            rt.quiet_local.as_ref() == Some(&member)
                && rt.hub_rev == self.drive.hub_rev
                && rt.last_full.is_some_and(|t| t.elapsed() < FULL_SCAN)
        });
        if quiet {
            set_status(member_id, |s| {
                if s.state != "error" && s.state != "confirm" {
                    s.state = "idle".into();
                    s.detail.clear();
                    s.last_ok = now_ms();
                }
            });
            return Ok(Changed::default());
        }
        set_status(member_id, |s| {
            s.state = "syncing".into();
            s.detail.clear();
        });
        let hub = walk_hub(self.addr, &self.drive.hub)?;
        let mut base = self.side.load_base(&self.drive.id, member_id)?;
        let now = now_ms() as i64 / 1000;
        // Still being written: leave it for the next pass (and do not take its absence as a delete).
        let settling: Vec<String> = if self.side.settles() {
            member
                .iter()
                .filter(|(_, m)| (0..SETTLE_SECS).contains(&(now - m.mtime)))
                .map(|(p, _)| p.clone())
                .collect()
        } else {
            Vec::new()
        };
        let mut ops = plan(&member, &hub, &base);
        ops.retain(|o| safe_rel(o.path()) && !settling.iter().any(|p| p == o.path()));
        // Left out: what this side cannot name, and what keeps failing (until it has rested).
        let ci = self.side.case_insensitive();
        let twins = case_twins(&hub, ci);
        let clash = case_clashes(&member, &hub, ci);
        let mut skipped: Vec<String> = Vec::new();
        let resting: std::collections::HashSet<String> = with_runtime(member_id, |rt| {
            rt.fails
                .retain(|_, (n, at)| *n < FAIL_LIMIT || at.elapsed() < FAIL_REST);
            rt.fails
                .iter()
                .filter(|(_, (n, _))| *n >= FAIL_LIMIT)
                .map(|(k, _)| k.clone())
                .collect()
        });
        ops.retain(|o| {
            let p = o.path();
            let fetches = matches!(o, Op::Download(_) | Op::Conflict(_) | Op::Compare(_));
            let keep = !(resting.contains(p)
                || clash.contains(p)
                || (fetches && (self.side.unstorable(p) || twins.contains(p))));
            if !keep {
                skipped.push(p.to_string());
            }
            keep
        });
        let mut held = 0;
        let mut held_paths = Vec::new();
        // A confirmation covers the deletes it was given for: used up by this pass either way.
        let was_confirmed = confirmed()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(member_id);
        if too_many_deletes(&ops, base.len()) && !was_confirmed {
            held = ops.iter().filter(|o| o.is_delete()).count();
            held_paths = held_shown(&ops);
            ops.retain(|o| !o.is_delete());
        }
        if !ops.is_empty() {
            let shown: Vec<String> = ops.iter().take(12).map(|o| format!("{o:?}")).collect();
            crate::log_info!(
                "mesh drive {} ({}): {} change(s): {}",
                self.drive.name,
                member_id,
                ops.len(),
                shown.join(", ")
            );
        }
        let (today, stamp) = date_parts(now);
        let (mut up, mut down, mut del, mut conf) = (0, 0, 0, 0);
        let mut hub_changed = false;
        let mut side_changed = false;
        // What each op left on both sides (stat right after it): the base is refreshed from the
        // listings at the end, from the entries that still match these.
        let mut touched: HashMap<String, (Option<Meta>, Option<Meta>)> = HashMap::new();
        // Uploaded while being edited: left for the next pass, never settled from a listing.
        let mut edited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut first_error: Option<String> = None;
        // Deleted here and on the hub: the folders they lay in go too once nothing is left in
        // them, so a folder deleted on one device leaves no empty copy of itself on the others.
        let (mut gone_member, mut gone_hub): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        // The console that went away mid-pass: the pass stops, keeping what it did.
        let mut went: Option<String> = None;
        let todo = ops.len();
        set_status(member_id, |s| {
            s.todo = todo;
            s.done = 0;
        });
        let hub_addr = self.addr;
        let hub_root = self.drive.hub.as_str();
        let watch = (!ops.is_empty()).then(|| {
            let mut consoles = vec![self.addr.to_string()];
            consoles.extend(self.side.console().map(str::to_string));
            Watchdog::start(consoles, self.lost.clone())
        });
        // Uploads go in batches (one job for many small files is far faster than a job each);
        // a batch that fails is not counted against its files: each then goes on its own.
        let mut batched: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut batching = true;
        for (i, op) in ops.iter().enumerate() {
            if batching && matches!(op, Op::Upload(_)) && !batched.contains(op.path()) {
                let mut chunk: Vec<String> = Vec::new();
                let mut bytes = 0u64;
                for o in ops[i..].iter().filter(|o| matches!(o, Op::Upload(_))) {
                    if chunk.len() >= BATCH_FILES || bytes >= BATCH_BYTES {
                        break;
                    }
                    bytes += member.get(o.path()).map_or(0, |m| m.size);
                    chunk.push(o.path().to_string());
                }
                if chunk.len() >= 2 {
                    match self.side.send_many(hub_addr, hub_root, &chunk) {
                        Ok(true) => batched.extend(chunk),
                        Ok(false) => batching = false,
                        Err(e) => {
                            crate::log_warn!("mesh drive {}: {e:#}", self.drive.name);
                            batching = false;
                        }
                    }
                }
            }
            if self.lost.load(Ordering::SeqCst) {
                first_error.get_or_insert_with(|| {
                    "another device took over the drive's lock; this pass stopped".into()
                });
                break;
            }
            let rel = op.path().to_string();
            let agreed_before = base.get(&rel).copied();
            set_status(member_id, |s| s.current = rel.clone());
            hub_changed |= matches!(op, Op::Upload(_) | Op::DeleteHub(_));
            side_changed |= matches!(
                op,
                Op::Download(_) | Op::Conflict(_) | Op::DeleteMember(_) | Op::Compare(_)
            );
            let r: Result<()> = (|| {
                match op {
                    Op::Upload(_) => {
                        let before = self.side.stat(&rel);
                        if batched.contains(&rel) {
                            // Sent in a batch: what went is what the listing showed.
                            let listed = member.get(&rel).copied();
                            up += 1;
                            if before != listed {
                                edited.insert(rel.clone());
                            }
                            let h = hub_stat(hub_addr, &join_hub(hub_root, &rel));
                            if let (Some(m), Some(h)) = (listed, h) {
                                base.insert(rel.clone(), BaseEntry { m, h });
                            }
                            return Ok(());
                        }
                        self.side.send(hub_addr, hub_root, &rel)?;
                        up += 1;
                        let after = self.side.stat(&rel);
                        let h = hub_stat(hub_addr, &join_hub(hub_root, &rel));
                        // Saved again while it went up: the agreement is on the bytes that went
                        // (the state before), so the next pass sees only this side changed and
                        // sends the new bytes. (Recording nothing made it a change on both sides:
                        // a conflict that renamed the newest edit.)
                        if after != before {
                            edited.insert(rel.clone());
                        }
                        if let (Some(m), Some(h)) = (before, h) {
                            base.insert(rel.clone(), BaseEntry { m, h });
                        }
                    }
                    Op::Download(_) => {
                        self.side
                            .fetch(hub_addr, hub_root, &rel, member.get(&rel).copied())?;
                        down += 1;
                        if let (Some(m), Some(h)) = (self.side.stat(&rel), hub.get(&rel)) {
                            base.insert(rel.clone(), BaseEntry { m, h: *h });
                        }
                    }
                    Op::Conflict(_) => {
                        // Ours keeps a new name (it reaches everyone next pass); theirs comes here.
                        let renamed = self.conflict_copy(&rel, &stamp);
                        self.side.rename(&rel, &renamed)?;
                        member.remove(&rel);
                        self.side.fetch(hub_addr, hub_root, &rel, None)?;
                        conf += 1;
                        if let (Some(m), Some(h)) = (self.side.stat(&rel), hub.get(&rel)) {
                            base.insert(rel.clone(), BaseEntry { m, h: *h });
                        }
                    }
                    Op::DeleteMember(_) => {
                        self.side.trash(&rel, &today)?;
                        del += 1;
                        base.remove(&rel);
                        gone_member.push(rel.clone());
                    }
                    Op::DeleteHub(_) => {
                        self.trash_hub(&rel, &today)?;
                        del += 1;
                        base.remove(&rel);
                        gone_hub.push(rel.clone());
                    }
                    Op::Compare(_) => {
                        if self.same_bytes(&rel)? {
                            if let (Some(m), Some(h)) = (member.get(&rel), hub.get(&rel)) {
                                base.insert(rel.clone(), BaseEntry { m: *m, h: *h });
                            }
                        } else {
                            let renamed = self.conflict_copy(&rel, &stamp);
                            self.side.rename(&rel, &renamed)?;
                            member.remove(&rel);
                            self.side.fetch(hub_addr, hub_root, &rel, None)?;
                            conf += 1;
                            if let (Some(m), Some(h)) = (self.side.stat(&rel), hub.get(&rel)) {
                                base.insert(rel.clone(), BaseEntry { m, h: *h });
                            }
                        }
                    }
                    Op::Forget(_) => {
                        base.remove(&rel);
                    }
                }
                Ok(())
            })();
            // Only where the op recorded a new agreement (an upload edited meanwhile records none,
            // so its edit still goes next pass).
            if r.is_ok()
                && !edited.contains(&rel)
                && base.contains_key(&rel)
                && base.get(&rel).copied() != agreed_before
            {
                touched.insert(
                    rel.clone(),
                    (
                        self.side.stat(&rel),
                        hub_stat(hub_addr, &join_hub(hub_root, &rel)),
                    ),
                );
            }
            match r {
                Ok(()) => with_runtime(member_id, |rt| {
                    rt.fails.remove(&rel);
                }),
                Err(e) => {
                    // Stopped because another device took the drive's lock: not the file's fault.
                    if self.lost.load(Ordering::SeqCst) {
                        first_error.get_or_insert_with(|| {
                            "another device took over the drive's lock; this pass stopped".into()
                        });
                        break;
                    }
                    if let Some((addr, why)) = self.gone(&e) {
                        // Not this file's fault: it is not counted, and nothing else is tried
                        // (each would wait out the same silence).
                        crate::log_info!(
                            "mesh drive {}: {addr} went away mid-pass ({}); stopping",
                            self.drive.name,
                            why.as_str()
                        );
                        mark_away(&addr, why);
                        went = Some(addr);
                        break;
                    }
                    crate::log_warn!("mesh drive {}: {e:#}", self.drive.name);
                    first_error.get_or_insert_with(|| format!("{e:#}"));
                    with_runtime(member_id, |rt| {
                        let f = rt
                            .fails
                            .entry(rel.clone())
                            .or_insert((0, std::time::Instant::now()));
                        f.0 += 1;
                        f.1 = std::time::Instant::now();
                    });
                }
            }
            set_status(member_id, |s| s.done += 1);
            // Progress survives an interrupted pass.
            if up + down + del + conf > 0 && (up + down + del + conf) % 20 == 0 {
                let _ = self.side.save_base(&self.drive.id, member_id, &base);
            }
        }
        // A status read right after a copy can differ from what a listing shows a moment later
        // (the time a copy settles on): a base taken from it made the next pass see the file as
        // changed again and copy or compare it once more. One listing of each side settles it:
        // entries that still match what the op left (same size, time within 2 s) are recorded
        // as listed; anything else (edited meanwhile) is left for the next pass.
        drop(watch);
        if !touched.is_empty() && !self.lost.load(Ordering::SeqCst) && went.is_none() {
            if let (Ok(side_now), Ok(hub_now)) = (self.side.walk(), walk_hub(hub_addr, hub_root)) {
                let close = |a: Option<&Meta>, b: Option<Meta>| match (a, b) {
                    (Some(a), Some(b)) => a.size == b.size && (a.mtime - b.mtime).abs() <= 2,
                    _ => false,
                };
                for (rel, (m_after, h_after)) in &touched {
                    let (m, h) = (side_now.get(rel), hub_now.get(rel));
                    if close(m, *m_after) && close(h, *h_after) {
                        if let (Some(m), Some(h)) = (m, h) {
                            base.insert(rel.clone(), BaseEntry { m: *m, h: *h });
                        }
                    }
                }
            }
        }
        if !self.lost.load(Ordering::SeqCst) && went.is_none() {
            for dir in emptied_folders(gone_member.iter()) {
                self.side.prune(&dir, &today);
            }
            for dir in emptied_folders(gone_hub.iter()) {
                prune_console_dir(hub_addr, hub_root, &dir, &today);
            }
        }
        if let Some(addr) = went {
            // What was done is kept (a base that cannot be written now is rebuilt by comparing).
            let _ = self.side.save_base(&self.drive.id, member_id, &base);
            with_runtime(member_id, |rt| {
                rt.quiet_local = None;
                rt.side_rev = None;
            });
            set_away_status(member_id, &addr);
            return Ok(Changed {
                hub: hub_changed,
                side: side_changed,
                planned: false,
            });
        }
        self.side.save_base(&self.drive.id, member_id, &base)?;
        let files = base.len();
        let bytes: u64 = base.values().map(|e| e.h.size).sum();
        let changed = up + down + del + conf > 0;
        with_runtime(member_id, |rt| {
            rt.last_full = Some(std::time::Instant::now());
            rt.hub_rev = self.drive.hub_rev;
            // Only a pass that changed nothing vouches for the other console as it stood.
            rt.side_rev = self.side_rev.filter(|_| !changed && first_error.is_none());
            // Only a pass that changed nothing vouches for this listing; otherwise list again.
            rt.quiet_local =
                (!changed && first_error.is_none() && settling.is_empty()).then_some(member);
        });
        skipped.sort();
        set_status(member_id, |s| {
            s.files = files;
            s.bytes = bytes;
            s.todo = 0;
            s.done = 0;
            s.current.clear();
            s.skipped = skipped;
            s.uploaded = up;
            s.downloaded = down;
            s.deleted = del;
            s.conflicts = conf;
            s.held_deletes = held;
            s.held_paths = held_paths;
            if let Some(e) = &first_error {
                s.state = "error".into();
                s.detail = e.clone();
            } else if held > 0 {
                s.state = "confirm".into();
                s.detail = format!("{held} files would be deleted everywhere");
            } else {
                s.state = "idle".into();
                s.detail.clear();
                s.last_ok = now_ms();
            }
        });
        // Trash folders are named by day; the ones older than TRASH_DAYS go. Best effort.
        let cutoff = date_parts(now - TRASH_DAYS * 86_400).0;
        self.side.empty_trash(&cutoff);
        empty_console_trash(self.addr, &self.drive.hub, &cutoff);
        Ok(Changed {
            hub: hub_changed,
            side: side_changed,
            planned: true,
        })
    }
}

/// Takes `drive:<id>` on `addr` for one pass. False (and a "waiting" status unless it was in
/// sync) when another engine holds it.
fn take_drive_lock(addr: &str, drive: &Drive, member_id: &str) -> Result<bool> {
    let lock_name = format!("drive:{}", drive.id);
    let label = format!("Syncing Mesh Drive {}", drive.name);
    let took = crate::mesh_api::call(addr, || {
        mesh::lock(addr, "take", &lock_name, LOCK_TTL_S, false, &label, "")
    })?;
    if took
        .as_ref()
        .and_then(|v| v.get("got"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Ok(true);
    }
    set_status(member_id, |s| {
        // In sync and another device is passing through: still in sync, nothing to say.
        if s.state == "idle" {
            return;
        }
        s.state = "waiting".into();
        s.detail = took
            .as_ref()
            .and_then(|v| v.get("by_name"))
            .and_then(Value::as_str)
            .map(|n| format!("{n} is syncing it"))
            .unwrap_or_default();
    });
    Ok(false)
}

/// One pass of `side` against `drive`'s home on `addr`, under the drive's lock. None when
/// another engine holds the lock; Some(the pass's result) otherwise (a failed pass already set
/// its status).
fn pass_on(
    addr: &str,
    drive: &Drive,
    member_id: &str,
    side: &dyn Side,
    device_name: String,
) -> Result<Option<Result<Changed>>> {
    if !take_drive_lock(addr, drive, member_id)? {
        return Ok(None);
    }
    let lease = DriveLease::hold(
        addr,
        &format!("drive:{}", drive.id),
        &format!("Syncing Mesh Drive {}", drive.name),
    );
    let pass = Pass {
        addr,
        drive,
        member_id: member_id.to_string(),
        side,
        device_name,
        side_rev: None,
        lost: lease.lost.clone(),
    };
    let r = pass.run();
    drop(lease);
    if let Err(e) = &r {
        if let Some(why) = confirm_away(addr, e) {
            // The console went away before or between the pass's reads: offline, not a fault.
            mark_away(addr, why);
            set_away_status(member_id, addr);
            return Ok(Some(Ok(Changed::default())));
        }
        set_status(member_id, |s| {
            s.state = "error".into();
            s.detail = format!("{e:#}");
        });
    }
    Ok(Some(r))
}

fn sync_member(lm: &LocalMember, me: &str) -> Result<()> {
    let addr = lm.console.as_str();
    // Away, and not worth another try yet: nothing is read from it this round.
    if !worth_trying(addr) {
        let misses = with_runtime(&lm.member, |rt| rt.home_misses);
        if misses >= 2 && !lm.paused && !lm.links.is_empty() {
            return failover(lm);
        }
        set_away_status(&lm.member, addr);
        return Ok(());
    }
    let doc = match doc_read(addr) {
        Ok(Some((_, doc))) => doc,
        Ok(None) => {
            set_status(&lm.member, |s| {
                s.state = "offline".into();
                s.detail = "the console's helper is older than 8.0.0".into();
            });
            return Ok(());
        }
        // Its own console is away: a linked console holds the same drive (after a second miss
        // in a row: one dropped read is not "away").
        Err(e) => {
            if away_why(&e).is_none() && e.to_string().contains("cannot be read by this version") {
                set_status(&lm.member, |s| {
                    s.state = "error".into();
                    s.detail = format!("{e:#}");
                });
                return Ok(());
            }
            mark_away(addr, away_why(&e).unwrap_or(AwayWhy::Unreachable));
            let misses = with_runtime(&lm.member, |rt| {
                rt.home_misses += 1;
                rt.home_misses
            });
            if misses < 2 || lm.paused || lm.links.is_empty() {
                set_away_status(&lm.member, addr);
                return Ok(());
            }
            return failover(lm);
        }
    };
    mark_back(addr);
    with_runtime(&lm.member, |rt| rt.home_misses = 0);
    set_status(&lm.member, |s| {
        s.away.clear();
        s.away_since = 0;
    });
    // The console's list read fine and no longer has this folder (the drive was deleted, or the
    // folder removed, from another device): forget it here too, so the folder can be used again.
    // Its files stay where they are.
    let Some(drive) = doc.drives.iter().find(|d| d.id == lm.drive) else {
        crate::log_info!(
            "mesh drive {}: deleted on the console; this folder stops syncing",
            lm.drive
        );
        return forget_local(lm);
    };
    let Some(member) = drive
        .members
        .iter()
        .find(|m| m.id == lm.member && m.device == me)
    else {
        crate::log_info!(
            "mesh drive {}: this folder was removed from it; it stops syncing",
            drive.name
        );
        return forget_local(lm);
    };
    // Remembered for when this console is away.
    let links: Vec<String> = drive.links.iter().map(|l| l.console.clone()).collect();
    if links != lm.links || member.paused != lm.paused {
        let _ = edit_config(|c| {
            for m in c.members.iter_mut().filter(|m| m.member == lm.member) {
                m.links = links.clone();
                m.paused = member.paused;
            }
        });
    }
    if member.paused {
        set_status(&lm.member, |s| s.state = "paused".into());
        return Ok(());
    }
    let side = LocalSide {
        root: PathBuf::from(&lm.path),
        via: String::new(),
    };
    let r = match pass_on(
        addr,
        drive,
        &lm.member,
        &side,
        crate::mesh_api::device_name(),
    ) {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(()),
        // The drive's lock could not be asked for: the console just went away.
        Err(e) => {
            if let Some(why) = confirm_away(addr, &e) {
                mark_away(addr, why);
                set_away_status(&lm.member, addr);
                return Ok(());
            }
            return Err(e);
        }
    };
    let hub_changed = r.as_ref().is_ok_and(|c| c.hub);
    if let Err(e) = report(addr, drive, member, &lm.member, hub_changed) {
        crate::log_warn!("mesh drive {}: report: {e:#}", drive.name);
    }
    r.map(|_| ())
}

/// This folder's own console does not answer: sync through a console the drive is linked to,
/// with a base kept apart for it (the first such pass only adds and compares, never deletes).
fn failover(lm: &LocalMember) -> Result<()> {
    for b in &lm.links {
        if !worth_trying(b) {
            continue;
        }
        let doc = match doc_read(b) {
            Ok(Some((_, doc))) => doc,
            Ok(None) => continue,
            Err(e) => {
                mark_away(b, away_why(&e).unwrap_or(AwayWhy::Unreachable));
                continue;
            }
        };
        mark_back(b);
        let Some(drive) = doc.drives.iter().find(|d| d.id == lm.drive) else {
            continue;
        };
        // Still linked from that side too.
        let Some(back) = drive.links.iter().find(|l| l.console == lm.console) else {
            continue;
        };
        let home_name = if back.console_name.is_empty() {
            lm.console.clone()
        } else {
            back.console_name.clone()
        };
        let side = LocalSide {
            root: PathBuf::from(&lm.path),
            via: b.clone(),
        };
        let Some(r) = pass_on(b, drive, &lm.member, &side, crate::mesh_api::device_name())? else {
            return Ok(());
        };
        if r.as_ref().is_ok_and(|c| c.hub) {
            let _ = doc_update(b, |d| {
                if let Some(d) = d.drives.iter_mut().find(|d| d.id == lm.drive) {
                    d.hub_rev += 1;
                }
                Ok(())
            });
        }
        let (why, since) = away_of(&lm.console).unwrap_or((AwayWhy::Unreachable, now_ms()));
        set_status(&lm.member, |s| {
            if s.state == "idle" {
                s.detail = format!("{home_name} is away: synced through {b}");
                s.away = why.as_str().into();
                s.away_since = since;
            }
        });
        return r.map(|_| ());
    }
    set_away_status(&lm.member, &lm.console);
    Ok(())
}

// ---- master links (8.1.0) ----

/// The two ends of a link in a fixed order, so whichever engine carries it, the home side is
/// the same console and the base sits in the same folder.
fn link_ends<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Carries every link of every drive this engine is in. A link runs at most once per round,
/// from whichever of its consoles is read first.
fn sync_links(me: &str) {
    if matches!(crate::mesh_api::kind(), "android" | "ios") {
        return; // a phone is not up long enough to carry a link
    }
    let mut consoles: Vec<String> = load_config()
        .members
        .iter()
        .map(|m| m.console.clone())
        .collect();
    consoles.sort();
    consoles.dedup();
    let mut done = std::collections::HashSet::new();
    for a in consoles {
        if !worth_trying(&a) {
            continue;
        }
        let doc = match doc_read(&a) {
            Ok(Some((_, doc))) => doc,
            Ok(None) => continue,
            Err(e) => {
                mark_away(&a, away_why(&e).unwrap_or(AwayWhy::Unreachable));
                continue;
            }
        };
        for drive in doc
            .drives
            .iter()
            .filter(|d| d.members.iter().any(|m| m.device == me))
        {
            for l in &drive.links {
                let (lo, hi) = link_ends(&a, &l.console);
                if !done.insert(format!("{}|{lo}|{hi}", drive.id)) {
                    continue;
                }
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_link(&drive.id, lo, hi)
                }));
                match r {
                    Ok(Err(e)) => crate::log_warn!("mesh drive link {lo}-{hi}: {e:#}"),
                    Err(_) => crate::log_warn!("mesh drive link {lo}-{hi}: the pass panicked"),
                    Ok(Ok(())) => {}
                }
            }
        }
    }
}

fn link_member_id(drive: &str, hi: &str) -> String {
    format!("link:{drive}:{hi}")
}

/// One pass between the drive's home folders on `lo` and `hi`.
fn run_link(id: &str, lo: &str, hi: &str) -> Result<()> {
    let member_id = link_member_id(id, hi);
    // Either end away: the link waits (its state says so on the end that answers).
    for (end, other) in [(lo, hi), (hi, lo)] {
        if !worth_trying(end) {
            set_link_state(other, id, end, "offline", "", false, None);
            return Ok(());
        }
    }
    let dlo = match doc_read(lo) {
        Ok(Some((_, d))) => d,
        Ok(None) => return Ok(()),
        Err(e) => {
            mark_away(lo, away_why(&e).unwrap_or(AwayWhy::Unreachable));
            set_link_state(hi, id, lo, "offline", "", false, None);
            return Ok(());
        }
    };
    let Some(a) = dlo.drives.iter().find(|d| d.id == id) else {
        return Ok(());
    };
    let dhi = match doc_read(hi) {
        Ok(Some((_, d))) => d,
        Ok(None) => return Ok(()),
        Err(e) => {
            mark_away(hi, away_why(&e).unwrap_or(AwayWhy::Unreachable));
            set_link_state(lo, id, hi, "offline", "", false, None);
            return Ok(());
        }
    };
    mark_back(lo);
    mark_back(hi);
    let Some(b) = dhi.drives.iter().find(|d| d.id == id) else {
        // Unlinked from the other side: drop this end too.
        drop_link(lo, id, hi);
        return Ok(());
    };
    let (Some(to_hi), Some(to_lo)) = (
        a.links.iter().find(|l| l.console == hi),
        b.links.iter().find(|l| l.console == lo),
    ) else {
        drop_link(lo, id, hi);
        drop_link(hi, id, lo);
        return Ok(());
    };
    if to_hi.allow_deletes || to_lo.allow_deletes {
        confirmed()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(member_id.clone());
    }
    // The drive's lock on both consoles: no device pass on either runs meanwhile.
    if !take_drive_lock(lo, a, &member_id)? {
        return Ok(());
    }
    let lost = std::sync::Arc::new(AtomicBool::new(false));
    let label = format!("Syncing Mesh Drive {} between consoles", a.name);
    let lease_lo = DriveLease::hold_with(lo, &format!("drive:{id}"), &label, lost.clone());
    if !take_drive_lock(hi, b, &member_id)? {
        drop(lease_lo);
        return Ok(());
    }
    let lease_hi = DriveLease::hold_with(hi, &format!("drive:{id}"), &label, lost.clone());
    let side = ConsoleSide {
        addr: hi.to_string(),
        root: b.hub.clone(),
    };
    let hi_name = if to_hi.console_name.is_empty() {
        hi.to_string()
    } else {
        to_hi.console_name.clone()
    };
    let pass = Pass {
        addr: lo,
        drive: a,
        member_id: member_id.clone(),
        side: &side,
        device_name: hi_name,
        side_rev: Some(b.hub_rev),
        lost,
    };
    let r = pass.run();
    drop(lease_hi);
    drop(lease_lo);
    if let Err(e) = &r {
        if let Some((end, why)) = pass.gone(e) {
            mark_away(&end, why);
            set_away_status(&member_id, &end);
        } else {
            set_status(&member_id, |s| {
                s.state = "error".into();
                s.detail = format!("{e:#}");
            });
        }
    }
    let st = statuses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&member_id)
        .cloned()
        .unwrap_or_default();
    let state = match st.state.as_str() {
        "syncing" | "waiting" => "idle",
        s => s,
    };
    let changed = r.as_ref().ok().copied().unwrap_or_default();
    // A "let the deletes through" is used up only by a pass that planned (one that stopped early
    // because a console was away or an install ran never looked at it).
    let consumed = changed.planned && state != "confirm";
    if !consumed {
        confirmed()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&member_id);
    }
    set_link_state(
        lo,
        id,
        hi,
        state,
        if state == "error" { &st.detail } else { "" },
        changed.hub,
        Some((b.members.len() as u64, consumed)),
    );
    set_link_state(
        hi,
        id,
        lo,
        state,
        if state == "error" { &st.detail } else { "" },
        changed.side,
        Some((a.members.len() as u64, consumed)),
    );
    r.map(|_| ())
}

/// The link from `at` to `other` reports `state`; `bump` moves `at`'s hub_rev on (its home
/// folder changed). Written only when something changed or REPORT_EVERY_MS passed.
fn set_link_state(
    at: &str,
    id: &str,
    other: &str,
    state: &str,
    error: &str,
    bump: bool,
    circle_and_consumed: Option<(u64, bool)>,
) {
    // Nothing is written to a console that is away (it would only wait out the silence).
    if away_of(at).is_some() {
        return;
    }
    let now = now_ms();
    let error: String = error.chars().take(300).collect();
    let r = doc_update(at, |doc| {
        let Some(d) = doc.drives.iter_mut().find(|d| d.id == id) else {
            return Ok(false);
        };
        if bump {
            d.hub_rev += 1;
        }
        let Some(l) = d.links.iter_mut().find(|l| l.console == other) else {
            return Ok(bump);
        };
        let reported = l.other.get("reported").and_then(Value::as_u64).unwrap_or(0);
        let mut news = bump || l.state != state || l.error != error;
        if let Some((circle, consumed)) = circle_and_consumed {
            news |= l.circle != circle || (consumed && l.allow_deletes);
            l.circle = circle;
            if consumed {
                l.allow_deletes = false;
            }
        }
        if !news && now.saturating_sub(reported) < REPORT_EVERY_MS {
            return Ok(false);
        }
        l.state = state.to_string();
        l.error = error.clone();
        if state == "idle" {
            l.last_sync = now;
        }
        l.by_name = crate::mesh_api::device_name();
        l.other.insert("reported".into(), json!(now));
        Ok(true)
    });
    if let Err(e) = r {
        crate::log_warn!("mesh drive link {at}->{other}: {e:#}");
    }
}

/// Removes the link to `other` from `at`'s copy of the drive. Best effort.
fn drop_link(at: &str, id: &str, other: &str) {
    let _ = doc_update(at, |doc| {
        if let Some(d) = doc.drives.iter_mut().find(|d| d.id == id) {
            d.links.retain(|l| l.console != other);
        }
        Ok(())
    });
}

/// This member's state, on the console where every device reads it. Written when something a
/// person would see changed, the home folder changed (hub_rev, so the others look), or every
/// REPORT_EVERY_MS so a stale report stands out.
fn report(
    addr: &str,
    drive: &Drive,
    seen: &Member,
    member_id: &str,
    hub_changed: bool,
) -> Result<()> {
    let st = statuses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(member_id)
        .cloned()
        .unwrap_or_default();
    // Passing states are not news.
    let state = match st.state.as_str() {
        "syncing" | "waiting" => "idle",
        s => s,
    }
    .to_string();
    let error = if state == "error" {
        st.detail.chars().take(300).collect()
    } else {
        String::new()
    };
    let now = now_ms();
    let news = seen.state != state
        || seen.error != error
        || seen.files != st.files as u64
        || seen.bytes != st.bytes
        || seen.app != env!("CARGO_PKG_VERSION")
        || seen.device_name != crate::mesh_api::device_name();
    if !hub_changed && !news && now.saturating_sub(seen.reported) < REPORT_EVERY_MS {
        return Ok(());
    }
    let base = load_base(&drive.id, member_id);
    let stats = DriveStats {
        files: base.len() as u64,
        bytes: base.values().map(|e| e.h.size).sum(),
        conflicts: base
            .keys()
            .filter(|k| k.contains("(conflict from "))
            .count() as u64,
        updated: now,
        other: Default::default(),
    };
    doc_update(addr, |doc| {
        let Some(d) = doc.drives.iter_mut().find(|d| d.id == drive.id) else {
            return Ok(());
        };
        if hub_changed {
            d.hub_rev += 1;
        }
        if st.state != "error" && st.state != "offline" {
            d.stats = Some(stats.clone());
        }
        if let Some(m) = d.members.iter_mut().find(|m| m.id == member_id) {
            m.state = state.clone();
            m.error = error.clone();
            m.files = st.files as u64;
            m.bytes = st.bytes;
            m.app = env!("CARGO_PKG_VERSION").to_string();
            m.kind = crate::mesh_api::kind().to_string();
            m.os = std::env::consts::OS.to_string();
            m.device_name = crate::mesh_api::device_name();
            if st.last_ok > m.last_sync {
                m.last_sync = st.last_ok;
            }
            m.reported = now;
        }
        Ok(())
    })?;
    Ok(())
}

fn forget_local(lm: &LocalMember) -> Result<()> {
    statuses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&lm.member);
    runtimes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&lm.member);
    remove_local(&lm.drive, Some(&lm.member))
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// One round over every folder of this device. Never two at once.
pub fn tick() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    // Cleared however the round ends, a panic included: a stuck flag would stop every drive.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done;
    let me = crate::mesh_api::id_hex().unwrap_or_default();
    for lm in load_config().members {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sync_member(&lm, &me)));
        match r {
            Ok(Err(e)) => crate::log_warn!("mesh drive {}: {e:#}", lm.drive),
            Err(_) => {
                crate::log_warn!("mesh drive {}: the pass panicked", lm.drive);
                set_status(&lm.member, |s| {
                    s.state = "error".into();
                    s.detail = "the last pass stopped unexpectedly; it is tried again".into();
                });
            }
            Ok(Ok(())) => {}
        }
    }
    sync_links(&me);
}

pub fn spawn() {
    crate::ava1_api::spawn_periodic(EVERY, tick);
}

fn kick() {
    std::thread::spawn(tick);
}

// ---- setting up ----

/// The folder this device keeps for a drive: made, and marked as the drive's.
fn prepare_local(path: &str) -> Result<()> {
    let p = Path::new(path);
    if !p.is_absolute() {
        bail!("choose a full folder path");
    }
    std::fs::create_dir_all(p).with_context(|| format!("make {path}"))?;
    let marker = p.join(MARKER);
    if !marker.exists() {
        std::fs::write(
            &marker,
            b"PS5Upload Mesh Drive. This folder is kept the same on your devices.\n",
        )
        .with_context(|| format!("write {}", marker.display()))?;
    }
    Ok(())
}

fn prepare_hub(addr: &str, hub: &str) -> Result<()> {
    if !hub.starts_with('/') || hub.split('/').any(|c| c == "..") {
        bail!("the console folder must be a full path");
    }
    fs_ops::fs_mkdir(addr, hub)?;
    ps5upload_core::diagnostics::fs_write_bytes(
        addr,
        &join_hub(hub, MARKER),
        b"PS5Upload Mesh Drive home folder.\n",
        false,
    )?;
    Ok(())
}

/// Where this device's copy goes unless the user picks another folder.
fn suggested_folder() -> Option<String> {
    const NAME: &str = "PS5Upload Mesh Drive";
    let p = match crate::mesh_api::kind() {
        "android" => PathBuf::from("/storage/emulated/0").join(NAME),
        "docker" => PathBuf::from("/data/mesh-drive"),
        "ios" => crate::remote::store::data_dir()?.join(NAME),
        _ => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)?
            .join(NAME),
    };
    Some(p.to_string_lossy().into_owned())
}

// ---- checking a folder before it is used ----

/// What a folder choice would mean, for the app to show before anything is made.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct FolderCheck {
    pub path: String,
    /// It can be used as chosen.
    pub ok: bool,
    pub exists: bool,
    /// It does not exist yet and will be made.
    pub will_create: bool,
    /// Files already in it (they join the drive), counted up to COUNT_LIMIT.
    pub files: usize,
    pub more: bool,
    /// It already holds a Mesh Drive marker (a drive's folder before, or this drive's).
    pub marker: bool,
    /// Why it cannot be used: bad_name, not_absolute, no_parent, not_folder, exists (a new
    /// folder's name is taken), taken (another drive's folder), inside / holds (inside, or holding,
    /// another drive's folder), app_data, not_writable, unreadable.
    pub problem: Option<String>,
    /// The other drive's name, or the system's words.
    pub detail: Option<String>,
}

const COUNT_LIMIT: usize = 10_000;

/// A folder name the user typed: something every filesystem here takes.
fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n == name
        && n.len() <= 200
        && n != "."
        && n != ".."
        && !n.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

fn inside(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_end_matches('/'), b.trim_end_matches('/'));
    a == b || a.starts_with(&format!("{b}/"))
}

fn fail_check(path: &str, problem: &str, detail: Option<String>) -> FolderCheck {
    FolderCheck {
        path: path.to_string(),
        problem: Some(problem.to_string()),
        detail,
        ..Default::default()
    }
}

/// Files under a console folder, counted up to COUNT_LIMIT (markers and trash not counted).
fn count_hub(addr: &str, root: &str) -> Result<(usize, bool)> {
    let mut n = 0usize;
    let mut stack = vec![root.trim_end_matches('/').to_string()];
    while let Some(dir) = stack.pop() {
        let mut offset = 0u64;
        loop {
            let l = fs_ops::list_dir(addr, &dir, ListDirOptions { offset, limit: 256 })?;
            let got = l.entries.len() as u64;
            for e in l.entries {
                if ignored(&e.name) || e.name == "." || e.name == ".." {
                    continue;
                }
                match e.kind.as_str() {
                    "dir" => stack.push(format!("{dir}/{}", e.name)),
                    "file" => n += 1,
                    _ => {}
                }
                if n >= COUNT_LIMIT {
                    return Ok((n, true));
                }
            }
            if got == 0 || (got < 256 && !l.truncated) {
                break;
            }
            offset += got;
        }
    }
    Ok((n, false))
}

fn count_local(root: &Path) -> (usize, bool) {
    let mut n = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if ignored(&name) {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => n += 1,
                _ => {}
            }
            if n >= COUNT_LIMIT {
                return (n, true);
            }
        }
    }
    (n, false)
}

/// The drive's home on the console. `new_name`: a folder to make under `path` (then `path` is its
/// parent); otherwise `path` itself is the folder. `drive`: the drive it is checked for (its own
/// folder is not "taken").
/// The console's whole data area or a whole drive: everything on it would reach every device.
fn hub_too_broad(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    if p.is_empty()
        || [
            "/data",
            "/mnt",
            "/user",
            "/system_data",
            "/mnt/usb",
            "/mnt/ext",
        ]
        .contains(&p)
    {
        return true;
    }
    // /mnt/usb0, /mnt/ext1 ...: a whole drive.
    p.strip_prefix("/mnt/")
        .is_some_and(|d| !d.contains('/') && (d.starts_with("usb") || d.starts_with("ext")))
}

pub fn check_hub(
    addr: &str,
    doc: &DriveDoc,
    path: &str,
    new_name: Option<&str>,
    drive: Option<&str>,
) -> FolderCheck {
    let parent = path.trim().trim_end_matches('/');
    if !parent.starts_with('/') || parent.split('/').any(|c| c == "..") {
        return fail_check(path, "not_absolute", None);
    }
    let target = match new_name {
        Some(n) if !valid_name(n) => return fail_check(path, "bad_name", None),
        Some(n) => format!("{}/{n}", if parent.is_empty() { "" } else { parent }),
        None => parent.to_string(),
    };
    if hub_too_broad(&target) {
        return fail_check(&target, "too_broad", None);
    }
    for d in doc.drives.iter().filter(|d| Some(d.id.as_str()) != drive) {
        let problem = if inside(&target, &d.hub) && inside(&d.hub, &target) {
            "taken"
        } else if inside(&target, &d.hub) {
            "inside"
        } else if inside(&d.hub, &target) {
            "holds"
        } else {
            continue;
        };
        return fail_check(&target, problem, Some(d.name.clone()));
    }
    if new_name.is_some() {
        match fs_ops::fs_stat(addr, parent) {
            Ok(st) if st.kind == "dir" => {}
            Ok(_) => return fail_check(&target, "not_folder", None),
            Err(e) if fs_ops::is_not_found(&format!("{e:#}")) => {
                return fail_check(&target, "no_parent", None)
            }
            Err(e) => return fail_check(&target, "unreadable", Some(format!("{e:#}"))),
        }
    }
    match fs_ops::fs_stat(addr, &target) {
        Ok(st) if st.kind == "dir" => {
            if new_name.is_some() {
                // Taken: the app offers to use the folder that is there instead.
                let mut c = fail_check(&target, "exists", None);
                c.exists = true;
                return c;
            }
            let (files, more) = match count_hub(addr, &target) {
                Ok(c) => c,
                Err(e) => return fail_check(&target, "unreadable", Some(format!("{e:#}"))),
            };
            FolderCheck {
                path: target.clone(),
                ok: true,
                exists: true,
                files,
                more,
                marker: fs_ops::fs_stat(addr, &join_hub(&target, MARKER)).is_ok(),
                ..Default::default()
            }
        }
        Ok(_) => fail_check(&target, "not_folder", None),
        Err(e) if fs_ops::is_not_found(&format!("{e:#}")) => {
            if new_name.is_none() {
                return fail_check(&target, "no_parent", None);
            }
            FolderCheck {
                path: target,
                ok: true,
                will_create: true,
                ..Default::default()
            }
        }
        Err(e) => fail_check(&target, "unreadable", Some(format!("{e:#}"))),
    }
}

/// This device's folder for a drive. `drive`: the drive it is for (its own entry here is not an
/// overlap).
/// A whole disk, a phone's whole storage or a home folder: syncing it would copy everything on the
/// device to the console (a picker's "use this folder" at the top level is an easy slip).
fn too_broad(p: &Path) -> bool {
    use std::path::Component;
    if !p.components().any(|c| matches!(c, Component::Normal(_))) {
        return true; // "/", "C:\"
    }
    let s = p.to_string_lossy();
    let s = s.trim_end_matches(['/', '\\']);
    if [
        "/storage/emulated/0",
        "/storage/emulated",
        "/sdcard",
        "/storage",
        "/Users",
        "/home",
        "/Volumes",
        "/mnt",
        "/media",
    ]
    .contains(&s)
    {
        return true;
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .is_some_and(|h| Path::new(&h) == Path::new(s))
}

pub fn check_local(path: &str, drive: Option<&str>) -> FolderCheck {
    let p = Path::new(path.trim());
    let shown = p.to_string_lossy().into_owned();
    if !p.is_absolute() {
        return fail_check(&shown, "not_absolute", None);
    }
    if let Some(data) = crate::remote::store::data_dir() {
        let (a, b) = (p.to_string_lossy(), data.to_string_lossy());
        if inside(&a, &b) || inside(&b, &a) {
            return fail_check(&shown, "app_data", None);
        }
    }
    if too_broad(p) {
        return fail_check(&shown, "too_broad", None);
    }
    for m in load_config()
        .members
        .iter()
        .filter(|m| Some(m.drive.as_str()) != drive)
    {
        let (a, b) = (shown.as_str(), m.path.as_str());
        let problem = if inside(a, b) && inside(b, a) {
            "taken"
        } else if inside(a, b) {
            "inside"
        } else if inside(b, a) {
            "holds"
        } else {
            continue;
        };
        let other = if m.name.is_empty() {
            m.drive.clone()
        } else {
            m.name.clone()
        };
        return fail_check(&shown, problem, Some(other));
    }
    match std::fs::metadata(p) {
        Ok(md) if md.is_dir() => {
            // Writable: a probe file in, then out.
            let probe = p.join(".ps5upload-check");
            if let Err(e) = std::fs::write(&probe, b"") {
                return fail_check(&shown, "not_writable", Some(e.to_string()));
            }
            let _ = std::fs::remove_file(&probe);
            let (files, more) = count_local(p);
            FolderCheck {
                path: shown,
                ok: true,
                exists: true,
                files,
                more,
                marker: p.join(MARKER).is_file(),
                ..Default::default()
            }
        }
        Ok(_) => fail_check(&shown, "not_folder", None),
        Err(_) => match p.parent() {
            Some(parent) if parent.is_dir() => FolderCheck {
                path: shown,
                ok: true,
                will_create: true,
                ..Default::default()
            },
            _ => fail_check(&shown, "no_parent", None),
        },
    }
}

fn refuse_unless_ok(c: &FolderCheck, what: &str) -> Result<()> {
    if c.ok {
        return Ok(());
    }
    let why = match c.problem.as_deref() {
        Some("bad_name") => "that name cannot be a folder name".to_string(),
        Some("not_absolute") => "choose a full folder path".to_string(),
        Some("no_parent") => "the folder it would go in does not exist".to_string(),
        Some("not_folder") => "that is a file, not a folder".to_string(),
        Some("exists") => "a folder with that name is already there".to_string(),
        Some("taken") => format!(
            "it is already the folder of {}",
            c.detail.clone().unwrap_or_default()
        ),
        Some("inside") => format!(
            "it is inside the folder of {}",
            c.detail.clone().unwrap_or_default()
        ),
        Some("holds") => format!(
            "it holds the folder of {}",
            c.detail.clone().unwrap_or_default()
        ),
        Some("app_data") => "it is the app's own data folder".to_string(),
        Some("not_writable") => format!(
            "it cannot be written ({})",
            c.detail.clone().unwrap_or_default()
        ),
        _ => c
            .detail
            .clone()
            .unwrap_or_else(|| "it cannot be read".into()),
    };
    bail!("{what} {}: {why}", c.path)
}

/// This device as a new member of a drive.
fn new_member(id: &str, device: &str, path: &str) -> Member {
    Member {
        id: id.to_string(),
        device: device.to_string(),
        device_name: crate::mesh_api::device_name(),
        path: path.to_string(),
        approved: true,
        joined: now_ms(),
        kind: crate::mesh_api::kind().to_string(),
        os: std::env::consts::OS.to_string(),
        app: env!("CARGO_PKG_VERSION").to_string(),
        state: "starting".into(),
        ..Default::default()
    }
}

// ---- a drive's password ----

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
        .collect()
}

/// Argon2id of `password` with `salt` at the given cost.
fn argon2id(password: &[u8], salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32]> {
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(|e| anyhow!("argon2: {e}"))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = [0u8; 32];
    a.hash_password_into(password, salt, &mut out)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(out)
}

fn make_lock(password: &str) -> Result<DriveLock> {
    let salt: [u8; 16] = ava1::keys::random_bytes()?;
    let (m, t, p) = (
        argon2::Params::DEFAULT_M_COST,
        argon2::Params::DEFAULT_T_COST,
        argon2::Params::DEFAULT_P_COST,
    );
    Ok(DriveLock {
        alg: "argon2id".into(),
        salt: hex(&salt),
        hash: hex(&argon2id(password.as_bytes(), &salt, m, t, p)?),
        m,
        t,
        p,
        other: Default::default(),
    })
}

fn lock_opens(lock: &DriveLock, password: &str) -> bool {
    // Set trimmed, so checked trimmed (a phone keyboard adds a trailing space).
    let password = password.trim();
    if lock.alg != "argon2id" {
        return false;
    }
    // A cost far past the default is refused rather than run (a forged document could ask for
    // gigabytes).
    if lock.m > 256 * 1024 || lock.t > 16 || lock.p > 8 {
        return false;
    }
    let Ok(got) = argon2id(
        password.as_bytes(),
        &unhex(&lock.salt),
        lock.m,
        lock.t,
        lock.p,
    ) else {
        return false;
    };
    let want = unhex(&lock.hash);
    // Constant-time compare.
    want.len() == got.len()
        && got
            .iter()
            .zip(want.iter())
            .fold(0u8, |a, (x, y)| a | (x ^ y))
            == 0
}

fn me_or_err() -> Result<String> {
    crate::mesh_api::id_hex().ok_or_else(|| anyhow!("this engine has no identity"))
}

fn add_local(addr: &str, drive: &str, member: &str, path: &str, name: &str) -> Result<()> {
    edit_config(|c| {
        // The same folder joined again: its old entry (and what it agreed on) goes.
        for old in c
            .members
            .iter()
            .filter(|m| m.drive == drive && m.path == path && m.member != member)
        {
            if let Some(p) = base_path(drive, &old.member) {
                let _ = std::fs::remove_file(p);
            }
        }
        c.members
            .retain(|m| !(m.drive == drive && (m.member == member || m.path == path)));
        c.members.push(LocalMember {
            console: addr.to_string(),
            drive: drive.to_string(),
            member: member.to_string(),
            path: path.to_string(),
            name: name.to_string(),
            links: Vec::new(),
            paused: false,
        });
    })
}

/// Forgets this device's folders of `drive` kept through `console`.
fn remove_local_at(drive: &str, console: &str) -> Result<()> {
    let gone: Vec<String> = load_config()
        .members
        .iter()
        .filter(|m| m.drive == drive && m.console == console)
        .map(|m| m.member.clone())
        .collect();
    for m in gone {
        remove_local(drive, Some(&m))?;
    }
    Ok(())
}

fn remove_local(drive: &str, member: Option<&str>) -> Result<()> {
    edit_config(|c| {
        c.members
            .retain(|m| !(m.drive == drive && member.is_none_or(|id| m.member == id)));
    })?;
    if let Some(p) = member.and_then(|m| base_path(drive, m)) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

// ---- routes ----

fn unsupported() -> Value {
    json!({ "supported": false })
}

async fn answer(f: impl FnOnce() -> Result<Value> + Send + 'static) -> Response {
    match tokio::task::spawn_blocking(f)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

fn addr_of(addr: Option<String>) -> Result<String> {
    crate::required_console_addr(addr).ok_or_else(|| anyhow!("addr is required: say which console"))
}

#[derive(Deserialize)]
pub struct ListQuery {
    addr: Option<String>,
}

pub async fn list(Query(q): Query<ListQuery>) -> Response {
    answer(move || {
        let addr = addr_of(q.addr)?;
        let Some((_, doc)) = doc_read(&addr)? else {
            return Ok(unsupported());
        };
        // This device's folders of drives here that it keeps through another (linked) console.
        let elsewhere: Vec<Value> = load_config()
            .members
            .iter()
            .filter(|m| m.console != addr && doc.drives.iter().any(|d| d.id == m.drive))
            .map(|m| json!({ "drive": m.drive, "console": m.console, "path": m.path }))
            .collect();
        let mine: Vec<Value> = load_config()
            .members
            .into_iter()
            .filter(|m| m.console == addr)
            .map(|m| {
                let st = statuses()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&m.member)
                    .cloned()
                    .unwrap_or_default();
                json!({ "drive": m.drive, "member": m.member, "path": m.path, "status": st })
            })
            .collect();
        // Room left where each drive's home is (cached: list is polled).
        let free: serde_json::Map<String, Value> = doc
            .drives
            .iter()
            .filter_map(|d| Some((d.id.clone(), json!(hub_free(&addr, &d.hub)?))))
            .collect();
        Ok(json!({
            "supported": true,
            "elsewhere": elsewhere,
            "free": free,
            "me": crate::mesh_api::id_hex(),
            "kind": crate::mesh_api::kind(),
            "suggest": suggested_folder(),
            "drives": doc.drives,
            "local": mine,
        }))
    })
    .await
}

/// This device's folders on every console, with their status: read from this engine alone, so
/// it answers while a console is asleep or off (the page shows them with what to do).
pub async fn local() -> Response {
    let folders: Vec<Value> = load_config()
        .members
        .into_iter()
        .map(|m| {
            let st = statuses()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&m.member)
                .cloned()
                .unwrap_or_default();
            json!({
                "drive": m.drive,
                "member": m.member,
                "name": m.name,
                "console": m.console,
                "path": m.path,
                "links": m.links,
                "status": st,
            })
        })
        .collect();
    Json(json!({ "supported": true, "folders": folders })).into_response()
}

/// Usable bytes on the console drive holding `hub`, at most a half-minute old.
fn hub_free(addr: &str, hub: &str) -> Option<u64> {
    type Cache = Mutex<HashMap<String, (std::time::Instant, Option<u64>)>>;
    static C: OnceLock<Cache> = OnceLock::new();
    let key = format!("{addr}|{hub}");
    let c = C.get_or_init(Default::default);
    if let Some((at, v)) = c.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if at.elapsed() < Duration::from_secs(30) {
            return *v;
        }
    }
    let v = ps5upload_core::volumes::free_space(addr, hub)
        .ok()
        .map(|f| f.usable_bytes);
    c.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (std::time::Instant::now(), v));
    v
}

#[derive(Deserialize)]
pub struct CreateReq {
    addr: Option<String>,
    name: String,
    /// The home folder on the console, or (with `hub_name`) the folder to make it in.
    hub: String,
    /// Make a new folder of this name inside `hub`.
    hub_name: Option<String>,
    /// This device's folder: required, the user chooses it.
    path: String,
    /// A password other devices type to join (none: anyone on the network joins).
    password: Option<String>,
}

pub async fn create(Json(req): Json<CreateReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let me = me_or_err()?;
        let name: String = req.name.trim().chars().take(60).collect();
        if name.is_empty() {
            bail!("give the drive a name");
        }
        // Checked before anything is made: a refusal leaves no folder behind.
        let Some((_, current)) = doc_read(&addr)? else {
            return Ok(unsupported());
        };
        let hub_check = check_hub(&addr, &current, &req.hub, req.hub_name.as_deref(), None);
        refuse_unless_ok(&hub_check, "The console folder")?;
        let local_check = check_local(&req.path, None);
        refuse_unless_ok(&local_check, "This device's folder")?;
        let hub = hub_check.path.clone();
        let local = Some(local_check.path.clone());
        prepare_hub(&addr, &hub)?;
        if let Some(p) = &local {
            prepare_local(p)?;
        }
        let id = new_id();
        let member_id = new_id();
        let r = doc_update(&addr, |doc| {
            // One drive's folder inside another's would sync the inner drive's files twice.
            let inside = |a: &str, b: &str| {
                a == b || a.starts_with(&format!("{}/", b.trim_end_matches('/')))
            };
            if let Some(d) = doc
                .drives
                .iter()
                .find(|d| inside(&hub, &d.hub) || inside(&d.hub, &hub))
            {
                bail!(
                    "{hub} is inside, or holds, the folder of the Mesh Drive {}",
                    d.name
                );
            }
            doc.drives.push(Drive {
                id: id.clone(),
                name: name.clone(),
                hub: hub.clone(),
                created: now_ms(),
                by: me.clone(),
                members: local
                    .iter()
                    .map(|p| new_member(&member_id, &me, p))
                    .collect(),
                lock: match req.password.as_deref().map(str::trim) {
                    Some(pw) if !pw.is_empty() => Some(make_lock(pw)?),
                    _ => None,
                },
                ..Default::default()
            });
            Ok(())
        })?;
        if r.is_none() {
            return Ok(unsupported());
        }
        if let Some(p) = &local {
            if let Err(e) = add_local(&addr, &id, &member_id, p, &name) {
                // Never leave a drive on the console that this device cannot sync.
                let _ = doc_update(&addr, |doc| {
                    doc.drives.retain(|d| d.id != id);
                    Ok(())
                });
                return Err(e);
            }
            kick();
        }
        Ok(json!({ "supported": true, "ok": true, "drive": id }))
    })
    .await
}

#[derive(Deserialize)]
pub struct CheckReq {
    addr: Option<String>,
    /// The console folder (or the folder a new one goes in, with `hub_name`).
    hub: Option<String>,
    hub_name: Option<String>,
    /// This device's folder.
    path: Option<String>,
    /// Checked for this drive (joining it): its own folders are not "taken".
    drive: Option<String>,
    /// Check `hub` on this other console instead (linking a drive to it).
    console: Option<String>,
}

pub async fn check(Json(req): Json<CheckReq>) -> Response {
    answer(move || {
        let hub = match &req.hub {
            Some(h) if req.console.is_some() => {
                let other = crate::console_addr(req.console.as_deref().unwrap_or(""));
                let Some((_, doc)) = doc_read(&other)? else {
                    return Ok(unsupported());
                };
                if let Some(id) = req.drive.as_deref() {
                    if doc.drives.iter().any(|d| d.id == id) {
                        return Ok(json!({
                            "supported": true,
                            "hub": fail_check(h, "already_linked", None),
                            "local": null,
                        }));
                    }
                }
                Some(check_hub(&other, &doc, h, req.hub_name.as_deref(), None))
            }
            Some(h) => {
                let addr = addr_of(req.addr.clone())?;
                let Some((_, doc)) = doc_read(&addr)? else {
                    return Ok(unsupported());
                };
                Some(check_hub(
                    &addr,
                    &doc,
                    h,
                    req.hub_name.as_deref(),
                    req.drive.as_deref(),
                ))
            }
            None => None,
        };
        let local = req
            .path
            .as_deref()
            .map(|p| check_local(p, req.drive.as_deref()));
        Ok(json!({ "supported": true, "hub": hub, "local": local }))
    })
    .await
}

#[derive(Deserialize)]
pub struct JoinReq {
    addr: Option<String>,
    drive: String,
    path: String,
    /// The drive's password, when it has one.
    password: Option<String>,
}

pub async fn join(Json(req): Json<JoinReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let me = me_or_err()?;
        if let Some(m) = load_config()
            .members
            .iter()
            .find(|m| m.drive == req.drive && m.console != addr)
        {
            bail!(
                "kept_elsewhere: this device already keeps this drive (in {}) through {}; it syncs through whichever linked console is up",
                m.path,
                m.console
            );
        }
        let check = check_local(&req.path, Some(&req.drive));
        refuse_unless_ok(&check, "This device's folder")?;
        let path = check.path.clone();
        // The password before anything is made here.
        let Some((_, current)) = doc_read(&addr)? else {
            return Ok(unsupported());
        };
        let d = current
            .drives
            .iter()
            .find(|d| d.id == req.drive)
            .ok_or_else(|| anyhow!("that drive is gone"))?;
        if let Some(lock) = &d.lock {
            match req.password.as_deref() {
                Some(pw) if lock_opens(lock, pw) => {}
                Some(_) => bail!("wrong_password: that is not the drive's password"),
                None => bail!("password_needed: this drive asks for a password"),
            }
        }
        prepare_local(&path)?;
        let member_id = new_id();
        let joined = doc_update(&addr, |doc| {
            let d = doc
                .drives
                .iter_mut()
                .find(|d| d.id == req.drive)
                .ok_or_else(|| anyhow!("that drive is gone"))?;
            // This device joining again with the same folder replaces its old entry.
            d.members.retain(|m| !(m.device == me && m.path == path));
            d.members.push(new_member(&member_id, &me, &path));
            Ok(d.name.clone())
        })?;
        let Some(drive_name) = joined else {
            return Ok(unsupported());
        };
        if let Err(e) = add_local(&addr, &req.drive, &member_id, &path, &drive_name) {
            let _ = doc_update(&addr, |doc| {
                if let Some(d) = doc.drives.iter_mut().find(|d| d.id == req.drive) {
                    d.members.retain(|m| m.id != member_id);
                }
                Ok(())
            });
            return Err(e);
        }
        kick();
        Ok(json!({ "supported": true, "ok": true, "member": member_id, "approved": true }))
    })
    .await
}

#[derive(Deserialize)]
pub struct PasswordReq {
    addr: Option<String>,
    drive: String,
    /// The new password; empty or absent removes it.
    password: Option<String>,
    /// The current one, for a device that is not in the drive.
    current: Option<String>,
}

/// Sets, changes or removes a drive's password. Allowed to a device keeping the drive, or to
/// anyone who knows the current password (the device that set it may be long gone).
pub async fn password(Json(req): Json<PasswordReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let me = me_or_err()?;
        let next = match req.password.as_deref().map(str::trim) {
            Some(pw) if !pw.is_empty() => Some(make_lock(pw)?),
            _ => None,
        };
        let r = doc_update(&addr, |doc| {
            let d = doc
                .drives
                .iter_mut()
                .find(|d| d.id == req.drive)
                .ok_or_else(|| anyhow!("that drive is gone"))?;
            let member = d.members.iter().any(|m| m.device == me);
            let knows = match (&d.lock, req.current.as_deref()) {
                (None, _) => true,
                (Some(l), Some(pw)) => lock_opens(l, pw),
                (Some(_), None) => false,
            };
            if !member && !knows {
                bail!("wrong_password: only a device in the drive, or one that knows its password, can change it");
            }
            d.lock = next.clone();
            Ok(d.links.iter().map(|l| l.console.clone()).collect::<Vec<_>>())
        })?;
        let Some(linked) = r else {
            return Ok(unsupported());
        };
        // The same password on every linked console (best effort: one that is away keeps the old
        // one until it is set again).
        for c in linked {
            let _ = doc_update(&c, |doc| {
                if let Some(d) = doc.drives.iter_mut().find(|d| d.id == req.drive) {
                    d.lock = next.clone();
                }
                Ok(())
            });
        }
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

// ---- linking consoles (8.1.0) ----

#[derive(Deserialize)]
pub struct LinkReq {
    addr: Option<String>,
    drive: String,
    /// The other console.
    console: String,
    /// The drive's home there: a folder, or (with `hub_name`) where a new one is made.
    hub: String,
    hub_name: Option<String>,
    /// What the two consoles are called (shown on each side's card).
    name: Option<String>,
    here_name: Option<String>,
}

/// Links `drive` (here) with another console: that console gets a home folder for it and
/// becomes the master of its own circle for the drive; an engine in the drive keeps the two
/// homes the same. A console holds a drive at most once, so links form a tree.
pub async fn link(Json(req): Json<LinkReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let me = me_or_err()?;
        let other = crate::console_addr(&req.console);
        if other.is_empty() || other == addr {
            bail!("choose another console to link with");
        }
        let Some((_, here)) = doc_read(&addr)? else {
            return Ok(unsupported());
        };
        let a = here
            .drives
            .iter()
            .find(|d| d.id == req.drive)
            .ok_or_else(|| anyhow!("that drive is gone"))?
            .clone();
        if !a.members.iter().any(|m| m.device == me) && a.by != me {
            bail!("only a device in this drive can link it to another console");
        }
        let there = match doc_read(&other) {
            Ok(Some((_, d))) => d,
            Ok(None) => {
                bail!("{other}'s helper is older than 8.0.0: send it this app's helper first")
            }
            Err(e) => bail!("{other} does not answer: {e:#}"),
        };
        // That console still keeps its copy from an earlier link (unlinked since) and is linked
        // to nothing else: link to that copy again, in its folder.
        let relink = there
            .drives
            .iter()
            .find(|d| d.id == a.id)
            .map(|d| (d.links.is_empty(), d.hub.clone()));
        let relinking = relink.is_some();
        let target = match relink {
            Some((true, hub)) => hub,
            Some((false, _)) => bail!("already_linked: that console already holds this drive"),
            None => {
                let check = check_hub(&other, &there, &req.hub, req.hub_name.as_deref(), None);
                refuse_unless_ok(&check, "The other console's folder")?;
                let target = check.path.clone();
                prepare_hub(&other, &target)?;
                target
            }
        };
        let now = now_ms();
        let name_there = req.name.clone().unwrap_or_default();
        let name_here = req.here_name.clone().unwrap_or_default();
        doc_update(&other, |doc| {
            if let Some(d) = doc.drives.iter_mut().find(|d| d.id == a.id) {
                if !d.links.is_empty() {
                    bail!("already_linked: that console already holds this drive");
                }
                d.links.push(Link {
                    console: addr.clone(),
                    console_name: name_here.clone(),
                    hub: a.hub.clone(),
                    created: now,
                    ..Default::default()
                });
                d.lock = a.lock.clone();
                return Ok(());
            }
            doc.drives.push(Drive {
                id: a.id.clone(),
                name: a.name.clone(),
                hub: target.clone(),
                created: now,
                by: me.clone(),
                lock: a.lock.clone(),
                links: vec![Link {
                    console: addr.clone(),
                    console_name: name_here.clone(),
                    hub: a.hub.clone(),
                    created: now,
                    ..Default::default()
                }],
                ..Default::default()
            });
            Ok(())
        })?
        .ok_or_else(|| anyhow!("{other}'s helper is older than 8.0.0"))?;
        let linked = doc_update(&addr, |doc| {
            let d = doc
                .drives
                .iter_mut()
                .find(|d| d.id == a.id)
                .ok_or_else(|| anyhow!("that drive is gone"))?;
            d.links.retain(|l| l.console != other);
            d.links.push(Link {
                console: other.clone(),
                console_name: name_there.clone(),
                hub: target.clone(),
                created: now,
                ..Default::default()
            });
            Ok(())
        });
        if !matches!(linked, Ok(Some(()))) {
            // Never half-linked: a copy made for this link goes again (its folder stays); one
            // that was there before only loses the link back.
            if relinking {
                drop_link(&other, &a.id, &addr);
            } else {
                let _ = doc_update(&other, |doc| {
                    doc.drives.retain(|d| d.id != a.id);
                    Ok(())
                });
            }
            linked?;
            return Ok(unsupported());
        }
        kick();
        Ok(json!({ "supported": true, "ok": true, "hub": target }))
    })
    .await
}

#[derive(Deserialize)]
pub struct UnlinkReq {
    addr: Option<String>,
    drive: String,
    console: String,
    /// Let a held-back mass delete through this link run (instead of unlinking).
    allow_deletes: Option<bool>,
}

/// Ends a link (both sides keep their copy, each a drive of its own circle), or with
/// `allow_deletes` lets a held-back mass delete through it.
pub async fn unlink(Json(req): Json<UnlinkReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let other = crate::console_addr(&req.console);
        if req.allow_deletes == Some(true) {
            let r = doc_update(&addr, |doc| {
                if let Some(l) = doc
                    .drives
                    .iter_mut()
                    .find(|d| d.id == req.drive)
                    .and_then(|d| d.links.iter_mut().find(|l| l.console == other))
                {
                    l.allow_deletes = true;
                }
                Ok(())
            })?;
            if r.is_none() {
                return Ok(unsupported());
            }
            kick();
            return Ok(json!({ "supported": true, "ok": true }));
        }
        drop_link(&addr, &req.drive, &other);
        drop_link(&other, &req.drive, &addr);
        // The link's base lives in the second console's folder (link_ends): it means nothing now.
        if let Ok(Some((_, doc))) = doc_read(link_ends(&addr, &other).1) {
            if let Some(d) = doc.drives.iter().find(|d| d.id == req.drive) {
                let hi = link_ends(&addr, &other).1;
                let _ = fs_ops::fs_delete(hi, &join_hub(&d.hub, BASE_FILE));
                let _ = fs_ops::fs_delete(hi, &join_hub(&join_hub(&d.hub, TRASH), LINK_BASE));
            }
        }
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

// ---- conflicts (8.1.0) ----

#[derive(Deserialize)]
pub struct ConflictsQuery {
    addr: Option<String>,
    drive: String,
}

/// The drive's conflict copies, each with the file it is a copy of (read from the console's
/// home folder, which every device and linked console follows).
pub async fn conflicts(Query(q): Query<ConflictsQuery>) -> Response {
    answer(move || {
        let addr = addr_of(q.addr)?;
        let Some((_, doc)) = doc_read(&addr)? else {
            return Ok(unsupported());
        };
        let d = doc
            .drives
            .iter()
            .find(|d| d.id == q.drive)
            .ok_or_else(|| anyhow!("that drive is gone"))?;
        let hub = walk_hub(&addr, &d.hub)?;
        let mut out: Vec<Value> = hub
            .iter()
            .filter_map(|(path, m)| {
                let c = parse_conflict(path)?;
                let orig = hub.get(&c.original);
                Some(json!({
                    "copy": path,
                    "original": c.original,
                    "device": c.device,
                    "stamp": c.stamp,
                    "copy_size": m.size,
                    "copy_mtime": m.mtime,
                    "original_size": orig.map(|o| o.size),
                    "original_mtime": orig.map(|o| o.mtime),
                }))
            })
            .collect();
        out.sort_by(|a, b| {
            b["stamp"]
                .as_str()
                .cmp(&a["stamp"].as_str())
                .then(a["copy"].as_str().cmp(&b["copy"].as_str()))
        });
        Ok(json!({ "supported": true, "conflicts": out }))
    })
    .await
}

#[derive(Deserialize)]
pub struct ResolveReq {
    addr: Option<String>,
    drive: String,
    /// The conflict copy's path in the drive.
    copy: String,
    /// keep_copy (it replaces the original), keep_original (the copy goes to the trash),
    /// keep_both (the copy gets a name of its own).
    action: String,
    /// keep_both: the copy's new file name (default: "<name> (from <device>)").
    name: Option<String>,
}

/// Moves a file in the console's home folder to that folder's trash.
fn trash_on(addr: &str, root: &str, rel: &str, today: &str) -> Result<()> {
    let day = join_hub(&join_hub(root, TRASH), today);
    let to = join_hub(
        &day,
        &free_name(rel, |r| hub_stat(addr, &join_hub(&day, r)).is_some()),
    );
    if let Some(parent) = to.rsplit_once('/').map(|(d, _)| d) {
        fs_ops::fs_mkdir(addr, parent)?;
    }
    fs_ops::fs_move(addr, &join_hub(root, rel), &to)
}

/// Settles one conflict on the console's home folder; every device (and linked console) follows
/// on its next pass. Nothing is deleted outright: what goes, goes to the drive's trash.
pub async fn resolve(Json(req): Json<ResolveReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let Some((_, doc)) = doc_read(&addr)? else {
            return Ok(unsupported());
        };
        let d = doc
            .drives
            .iter()
            .find(|d| d.id == req.drive)
            .ok_or_else(|| anyhow!("that drive is gone"))?
            .clone();
        if !safe_rel(&req.copy) {
            bail!("that is not a file of the drive");
        }
        let c = parse_conflict(&req.copy).ok_or_else(|| anyhow!("that is not a conflict copy"))?;
        if hub_stat(&addr, &join_hub(&d.hub, &req.copy)).is_none() {
            bail!("gone: that copy is no longer there (another device settled it)");
        }
        // Under the drive's lock, so no pass moves these files meanwhile.
        let member_id = format!("resolve:{}", d.id);
        if !take_drive_lock(&addr, &d, &member_id)? {
            bail!("busy: the drive is syncing right now; try again in a moment");
        }
        let lease = DriveLease::hold(&addr, &format!("drive:{}", d.id), "Settling a conflict");
        let today = date_parts(now_ms() as i64 / 1000).0;
        let r: Result<String> = (|| match req.action.as_str() {
            "keep_original" => {
                trash_on(&addr, &d.hub, &req.copy, &today)?;
                Ok(c.original.clone())
            }
            "keep_copy" => {
                if hub_stat(&addr, &join_hub(&d.hub, &c.original)).is_some() {
                    trash_on(&addr, &d.hub, &c.original, &today)?;
                }
                fs_ops::fs_move(
                    &addr,
                    &join_hub(&d.hub, &req.copy),
                    &join_hub(&d.hub, &c.original),
                )?;
                Ok(c.original.clone())
            }
            "keep_both" => {
                let (dir, file) = c
                    .original
                    .rsplit_once('/')
                    .map_or(("", c.original.as_str()), |(a, b)| (a, b));
                let wanted = match req.name.as_deref().map(str::trim) {
                    Some(n) if !n.is_empty() => {
                        if !valid_name(n) {
                            bail!("bad_name: that cannot be a file name");
                        }
                        n.to_string()
                    }
                    _ => {
                        let (stem, ext) = match file.rfind('.') {
                            Some(i) if i > 0 => (&file[..i], &file[i..]),
                            _ => (file, ""),
                        };
                        format!("{stem} (from {}){ext}", c.device)
                    }
                };
                let rel = if dir.is_empty() {
                    wanted
                } else {
                    format!("{dir}/{wanted}")
                };
                let rel = free_name(&rel, |r| hub_stat(&addr, &join_hub(&d.hub, r)).is_some());
                fs_ops::fs_move(&addr, &join_hub(&d.hub, &req.copy), &join_hub(&d.hub, &rel))?;
                Ok(rel)
            }
            _ => bail!("choose keep_copy, keep_original or keep_both"),
        })();
        drop(lease);
        let kept = r?;
        let _ = doc_update(&addr, |doc| {
            if let Some(d) = doc.drives.iter_mut().find(|x| x.id == d.id) {
                d.hub_rev += 1;
            }
            Ok(())
        });
        kick();
        Ok(json!({ "supported": true, "ok": true, "kept": kept }))
    })
    .await
}

#[derive(Deserialize)]
pub struct MemberReq {
    addr: Option<String>,
    drive: String,
    member: Option<String>,
    approve: Option<bool>,
    paused: Option<bool>,
}

pub async fn approve(Json(req): Json<MemberReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let me = me_or_err()?;
        let member = req.member.clone().ok_or_else(|| anyhow!("which member?"))?;
        let approve = req.approve.unwrap_or(true);
        let r = doc_update(&addr, |doc| {
            let d = doc
                .drives
                .iter_mut()
                .find(|d| d.id == req.drive)
                .ok_or_else(|| anyhow!("that drive is gone"))?;
            // Only a device already in can let another in.
            if !d.members.iter().any(|m| m.device == me && m.approved) && d.by != me {
                bail!("only a device that is in this drive can let others in");
            }
            if approve {
                if let Some(m) = d.members.iter_mut().find(|m| m.id == member) {
                    m.approved = true;
                }
            } else {
                d.members.retain(|m| m.id != member);
            }
            Ok(())
        })?;
        if r.is_none() {
            return Ok(unsupported());
        }
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

pub async fn leave(Json(req): Json<MemberReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let member = req.member.clone().ok_or_else(|| anyhow!("which member?"))?;
        let r = doc_update(&addr, |doc| {
            if let Some(d) = doc.drives.iter_mut().find(|d| d.id == req.drive) {
                d.members.retain(|m| m.id != member);
            }
            Ok(())
        })?;
        remove_local(&req.drive, Some(&member))?;
        if r.is_none() {
            return Ok(unsupported());
        }
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

pub async fn delete(Json(req): Json<MemberReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let r = doc_update(&addr, |doc| {
            let linked: Vec<String> = doc
                .drives
                .iter()
                .filter(|d| d.id == req.drive)
                .flat_map(|d| d.links.iter().map(|l| l.console.clone()))
                .collect();
            doc.drives.retain(|d| d.id != req.drive);
            Ok(linked)
        })?;
        // Linked consoles keep their copy as a drive of their own circle; only the link goes.
        for c in r.clone().unwrap_or_default() {
            drop_link(&c, &req.drive, &addr);
        }
        remove_local_at(&req.drive, &addr)?;
        if r.is_none() {
            return Ok(unsupported());
        }
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

pub async fn pause(Json(req): Json<MemberReq>) -> Response {
    answer(move || {
        let addr = addr_of(req.addr)?;
        let member = req.member.clone().ok_or_else(|| anyhow!("which member?"))?;
        let paused = req.paused.unwrap_or(true);
        let r = doc_update(&addr, |doc| {
            if let Some(m) = doc
                .drives
                .iter_mut()
                .find(|d| d.id == req.drive)
                .and_then(|d| d.members.iter_mut().find(|m| m.id == member))
            {
                m.paused = paused;
            }
            Ok(())
        })?;
        if r.is_none() {
            return Ok(unsupported());
        }
        kick();
        Ok(json!({ "supported": true, "ok": true }))
    })
    .await
}

pub async fn sync_now(Json(_req): Json<MemberReq>) -> Response {
    // Asked for: look at both sides now, not only at what changed through the drive (a file
    // deleted on the console with Files otherwise waits for the next FULL_SCAN).
    for rt in runtimes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values_mut()
    {
        rt.quiet_local = None;
        rt.side_rev = None;
        rt.last_full = None;
    }
    kick();
    Json(json!({ "supported": true, "ok": true })).into_response()
}

pub async fn confirm(Json(req): Json<MemberReq>) -> Response {
    if let Some(m) = req.member {
        confirmed()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(m);
    }
    kick();
    Json(json!({ "supported": true, "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_held_mass_delete_names_what_it_would_delete() {
        let mut ops: Vec<Op> = (0..250).map(|i| Op::DeleteHub(format!("a/{i}"))).collect();
        ops.insert(0, Op::Upload("kept".into()));
        let shown = held_shown(&ops);
        assert_eq!(shown.len(), HELD_SHOWN);
        assert_eq!(shown[0], "a/0");
    }

    #[test]
    fn a_deleted_folder_takes_its_emptied_folders_deepest_first() {
        let gone = [
            "a/b/c.txt".to_string(),
            "a/b/d.txt".into(),
            "a/e.txt".into(),
            "top.txt".into(),
        ];
        assert_eq!(
            super::emptied_folders(gone.iter()),
            vec!["a/b".to_string(), "a".into()]
        );
    }

    #[test]
    fn only_a_folder_with_nothing_of_the_drive_left_is_pruned() {
        use super::Side;
        let root = std::env::temp_dir().join(format!("mesh-prune-{}", std::process::id()));
        std::fs::create_dir_all(root.join("gone/inner")).unwrap();
        std::fs::write(root.join("gone/.DS_Store"), b"x").unwrap();
        std::fs::create_dir_all(root.join("kept")).unwrap();
        std::fs::write(root.join("kept/save.dat"), b"x").unwrap();
        let side = super::LocalSide {
            root: root.clone(),
            via: String::new(),
        };
        assert!(side.prune("gone/inner", "2026-10-28"));
        assert!(side.prune("gone", "2026-10-28"));
        assert!(!root.join("gone").exists());
        assert!(!side.prune("kept", "2026-10-28"));
        assert!(root.join("kept/save.dat").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_away_console_is_tried_less_and_less_often() {
        let secs: Vec<u64> = (1..=7).map(|n| away_backoff(n).as_secs()).collect();
        assert_eq!(secs, vec![30, 60, 120, 240, 300, 300, 300]);
    }

    #[test]
    fn a_sleeping_or_silent_console_is_away_but_a_file_error_is_not() {
        let e = |t: &str| anyhow::anyhow!("payload rejected MESH_DOC_GET: {t}");
        assert_eq!(
            away_why(&anyhow::anyhow!(
                "console_rest_mode: The PS5 is in rest mode"
            )),
            Some(AwayWhy::Rest)
        );
        assert_eq!(
            away_why(&anyhow::anyhow!("ava1_unreachable: nothing answered")),
            Some(AwayWhy::Unreachable)
        );
        assert_eq!(
            away_why(&anyhow::anyhow!("helper_not_ava1: not running")),
            Some(AwayWhy::Helper)
        );
        assert_eq!(away_why(&e("fs_no_space")), None);
        assert_eq!(away_why(&anyhow::anyhow!("permission denied")), None);
    }

    #[test]
    fn a_console_that_comes_back_is_looked_at_in_full() {
        let addr = "192.0.2.77";
        mark_away(addr, AwayWhy::Rest);
        assert!(away_of(addr).is_some_and(|(w, _)| w == AwayWhy::Rest));
        with_runtime("link:x:192.0.2.77", |rt| {
            rt.last_full = Some(std::time::Instant::now());
            rt.fails
                .insert("a.txt".into(), (3, std::time::Instant::now()));
        });
        mark_back(addr);
        assert!(away_of(addr).is_none());
        with_runtime("link:x:192.0.2.77", |rt| {
            assert!(rt.last_full.is_none());
            assert!(rt.fails.is_empty());
        });
    }

    use super::*;

    #[test]
    fn a_lock_from_another_version_never_breaks_the_drive_list() {
        // 8.0.x wrote PBKDF2 locks ({salt, hash, iter}); a later one may add fields.
        let raw = r#"{"drives":[{"id":"d","name":"n","hub":"/data/x","lock":{"salt":"aa","hash":"bb","iter":5,"new":1}}]}"#;
        let doc: DriveDoc = serde_json::from_str(raw).unwrap();
        let lock = doc.drives[0].lock.as_ref().unwrap();
        assert!(!lock_opens(lock, "anything"));
    }

    #[test]
    fn a_drive_password_opens_only_itself() {
        let pw = String::from_utf8(vec![b'o'; 12]).unwrap();
        let l = make_lock(&pw).unwrap();
        assert_eq!(l.alg, "argon2id");
        assert!(!l.hash.contains(&pw));
        assert!(lock_opens(&l, &pw));
        assert!(lock_opens(&l, &format!("{pw} ")));
        assert!(!lock_opens(&l, &pw[1..]));
        assert!(!lock_opens(&l, ""));
        // Two locks of one password differ (their salts do).
        assert_ne!(make_lock(&pw).unwrap().hash, l.hash);
        // A forged cost is refused, not run.
        let mut heavy = l.clone();
        heavy.m = 4 * 1024 * 1024;
        assert!(!lock_opens(&heavy, &pw));
    }

    #[test]
    fn console_roots_are_too_broad() {
        for bad in ["/", "/data", "/data/", "/mnt/usb0", "/mnt/ext1", "/user"] {
            assert!(hub_too_broad(bad), "{bad}");
        }
        for ok in ["/data/Shared", "/mnt/usb0/mesh", "/mnt/ext0/PS5Upload Mesh"] {
            assert!(!hub_too_broad(ok), "{ok}");
        }
    }

    #[test]
    fn whole_disks_and_homes_are_too_broad() {
        for bad in [
            "/",
            "/storage/emulated/0",
            "/storage/emulated/0/",
            "/sdcard",
            "/Users",
            "/mnt",
        ] {
            assert!(too_broad(Path::new(bad)), "{bad}");
        }
        for ok in [
            "/mesh",
            "/storage/emulated/0/Documents/Shared",
            "/Users/me/Shared",
            "/mnt/user/share",
        ] {
            assert!(!too_broad(Path::new(ok)), "{ok}");
        }
        if let Some(h) = std::env::var_os("HOME") {
            assert!(too_broad(Path::new(&h)));
        }
    }

    #[test]
    fn free_name_never_reuses_a_name() {
        let used = ["a/x.txt", "a/x (2).txt", "noext", ".hidden"];
        let taken = |r: &str| used.contains(&r);
        assert_eq!(free_name("a/y.txt", taken), "a/y.txt");
        assert_eq!(free_name("a/x.txt", taken), "a/x (3).txt");
        assert_eq!(free_name("noext", taken), "noext (2)");
        assert_eq!(free_name(".hidden", taken), ".hidden (2)");
    }

    #[test]
    fn case_clashes_hold_both_sides_back() {
        let mut here = Listing::new();
        let mut there = Listing::new();
        here.insert("Foo.txt".into(), Meta { size: 1, mtime: 0 });
        here.insert("same.txt".into(), Meta { size: 1, mtime: 0 });
        there.insert("foo.txt".into(), Meta { size: 2, mtime: 0 });
        there.insert("same.txt".into(), Meta { size: 1, mtime: 0 });
        let c = case_clashes(&here, &there, cfg!(any(windows, target_os = "macos")));
        if cfg!(any(windows, target_os = "macos")) {
            assert!(c.contains("Foo.txt") && c.contains("foo.txt") && !c.contains("same.txt"));
        } else {
            assert!(c.is_empty());
        }
    }

    #[test]
    fn case_twins_only_where_case_is_ignored() {
        let mut hub = Listing::new();
        for k in ["a/Photo.jpg", "a/photo.jpg", "b.txt"] {
            hub.insert(k.into(), Meta { size: 1, mtime: 0 });
        }
        let t = case_twins(&hub, cfg!(any(windows, target_os = "macos")));
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(t.len(), 1);
            assert!(t.contains("a/photo.jpg"));
        } else {
            assert!(t.is_empty());
        }
    }

    #[test]
    fn folder_names_and_nesting() {
        for ok in ["Shared", "PS5 saves", "a.b", "Ünïcode 日本"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            " lead",
            "trail ",
            ".",
            "..",
            "a/b",
            "a\\b",
            "c:d",
            "x?y",
            "tab\there",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(inside("/data/A/B", "/data/A"));
        assert!(inside("/data/A", "/data/A/"));
        assert!(!inside("/data/AB", "/data/A"));
    }

    #[test]
    fn a_local_folder_is_checked_before_use() {
        let root = std::env::temp_dir().join(format!("mesh-check-{}", new_id()));
        std::fs::create_dir_all(root.join("has")).unwrap();
        std::fs::write(root.join("has").join("one.txt"), b"1").unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();

        let c = check_local(&s(&root.join("has")), None);
        assert!(c.ok && c.exists && !c.will_create && c.files == 1, "{c:?}");
        let c = check_local(&s(&root.join("new")), None);
        assert!(c.ok && c.will_create, "{c:?}");
        let c = check_local(&s(&root.join("missing").join("deeper")), None);
        assert_eq!(c.problem.as_deref(), Some("no_parent"));
        let c = check_local(&s(&root.join("file.txt")), None);
        assert_eq!(c.problem.as_deref(), Some("not_folder"));
        assert_eq!(
            check_local("relative/path", None).problem.as_deref(),
            Some("not_absolute")
        );
        assert!(refuse_unless_ok(&check_local("relative", None), "x").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn paths_from_a_listing_never_leave_the_folder() {
        assert!(safe_rel("a/b.txt"));
        for bad in ["", "/etc/passwd", "a/../../x", "./a", "a//b", "a\\..\\b"] {
            assert!(!safe_rel(bad), "{bad}");
        }
    }

    #[test]
    fn a_local_walk_skips_markers_trash_and_clutter() {
        let dir = std::env::temp_dir().join(format!("mesh-walk-{}", new_id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(TRASH).join("2026-01-01")).unwrap();
        std::fs::write(dir.join(MARKER), b"x").unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("sub").join("b.bin"), b"12").unwrap();
        std::fs::write(dir.join(".DS_Store"), b"x").unwrap();
        std::fs::write(dir.join(TRASH).join("2026-01-01").join("old"), b"x").unwrap();
        let l = walk_local(&dir).unwrap();
        assert_eq!(
            l.keys().cloned().collect::<Vec<_>>(),
            vec!["a.txt", "sub/b.bin"]
        );
        assert_eq!(l["a.txt"].size, 5);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_drive_document_reads_with_missing_fields() {
        let d: DriveDoc = serde_json::from_value(json!({
            "drives": [{ "id": "d1", "name": "Shared", "hub": "/data/Mesh",
                         "members": [{ "id": "m1", "device": "aa", "path": "/x" }] }]
        }))
        .unwrap();
        assert!(!d.drives[0].members[0].approved);
        assert_eq!(
            serde_json::from_value::<DriveDoc>(json!({})).unwrap(),
            DriveDoc::default()
        );
    }
}
