//! Folder sizes in Files (#974): what a console folder holds, shown in the Size column when it is
//! already known, never measured behind the person's back.
//!
//! * `GET  /api/ps5/folder-sizes?addr=&dir=` -> `{dir, self, children: {name: Entry}, queue}`: the
//!   known sizes of `dir` and the folders directly in it (and `dir` noted as opened, for the
//!   optional fresh sweep below)
//! * `POST /api/ps5/folder-sizes/measure` `{addr, paths?, dir?}` -> the queue: measure these
//!   folders, or every folder directly in `dir` ("Measure everything here")
//! * `POST /api/ps5/folder-sizes/cancel` `{addr?, paths?}` -> the queue: stop these (all of that
//!   console's without `paths`, all of everyone's without `addr`)
//! * `GET  /api/ps5/folder-sizes/queue?addr=` -> the queue
//!
//! `Entry` is `{bytes, files, measured_at_ms, source, stored, partial}`: `stored` says `bytes` is
//! what the folder takes on the drive (the helper's count, so a zstd-packed game counts its packs);
//! otherwise it is its files' sizes added up. `source` is `measured` (a Measure size),
//! `space` (a "What takes space?" walk, disk_usage.rs) or `cleanup` (the clean-up scan's folders):
//! the last two were measured anyway, so keeping their sizes costs the console nothing.
//!
//! The queue (`{rev, running, queued, paused, keep_fresh, last_error}`) runs one folder at a time
//! for the whole engine. A measure is the helper's `JOB_OP_TREE_SIZE` (one job next to the drive,
//! names and metadata only, with short naps); a helper from before it gets the fs.list walk "What
//! takes space?" uses, one list in flight. While a transfer, console file operation or install runs
//! on that console nothing starts there (`paused: "busy"`), and a measure already running is
//! stopped and put back first in line. `rev` changes whenever a known size does.
//!
//! Sizes are kept per console and folder in `state/folder-sizes.json` under the data folder (at
//! most [`MAX_PER_CONSOLE`] folders a console, the oldest dropped first), with the time each was
//! measured, so the app shows how old it is. PS5Upload's own writes, deletes and moves drop the
//! sizes of the folders they touch and of every folder above them ([`changed`]); a change made any
//! other way leaves a size as it was, dated.
//!
//! "Keep folder sizes fresh" (setting `keep_folder_sizes_fresh`, off by default): when the queue
//! is empty and nothing runs on the console, known sizes older than a day of folders shown in a
//! folder the person opened in the last week are measured again, each at most once a day.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::disk_usage::{self, normalize};
use crate::{console_addr_or_default, json_err, AppState};

/// Folders kept per console; past it the oldest sizes go first.
pub(crate) const MAX_PER_CONSOLE: usize = 2000;
/// Consoles kept; past it the one with the oldest sizes goes.
const MAX_CONSOLES: usize = 16;
/// Writes remembered per console ([`Store::put_since`]).
const MAX_MARKS: usize = 1000;
/// A finished clean-up scan's sizes are kept only while it is this fresh: the helper keeps
/// answering with an old scan until the next one, and its folders may have changed since.
const CLEANUP_FRESH_MS: u64 = 10 * 60 * 1000;
/// Opened folders remembered per console, for the fresh sweep.
const MAX_OPENED: usize = 200;
/// Folders one request may queue.
const MAX_QUEUE: usize = 1000;
/// A measure that takes longer than this is given up (a network share mounted on the console).
const MEASURE_DEADLINE: Duration = Duration::from_secs(30 * 60);
/// How often a running measure checks for a cancel or a job that started on its console.
const WATCH: Duration = Duration::from_millis(250);
/// How often the worker looks again while everything queued waits for a busy console.
const BUSY_RETRY: Duration = Duration::from_secs(3);
/// The idle worker's tick: unsaved changes written, the fresh sweep considered.
const IDLE_TICK: Duration = Duration::from_secs(10 * 60);
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// The fresh sweep's reach: folders opened within this.
const OPENED_WITHIN_MS: u64 = 7 * DAY_MS;
/// Folders one fresh sweep queues at most.
const FRESH_BATCH: usize = 20;

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// `p` is `dir` or below it.
fn within(p: &str, dir: &str) -> bool {
    p == dir || dir == "/" || p.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn valid_path(p: &str) -> bool {
    p.starts_with('/') && !p.split('/').any(|s| s == "..") && !p.contains('\0')
}

// ─── Store ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Source {
    /// A Measure size (or the fresh sweep).
    Measured,
    /// A "What takes space?" walk.
    Space,
    /// The clean-up scan.
    Cleanup,
    /// A transfer that wrote the folder.
    Transfer,
}

/// One folder's known size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub bytes: u64,
    #[serde(default)]
    pub files: Option<u64>,
    pub measured_at_ms: u64,
    pub source: Source,
    /// `bytes` is what the folder takes on the drive, not its files' sizes added up.
    #[serde(default)]
    pub stored: bool,
    /// Something inside could not be counted (an unreadable folder, a limit reached).
    #[serde(default)]
    pub partial: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ConsoleSizes {
    /// The address it was last reached at: the fresh sweep measures through it.
    #[serde(default)]
    addr: String,
    #[serde(default)]
    sizes: BTreeMap<String, Entry>,
    /// Folders the person opened in Files, with when.
    #[serde(default)]
    opened: BTreeMap<String, u64>,
}

impl ConsoleSizes {
    fn newest(&self) -> u64 {
        self.sizes
            .values()
            .map(|e| e.measured_at_ms)
            .chain(self.opened.values().copied())
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    consoles: BTreeMap<String, ConsoleSizes>,
}

/// Every console's known sizes, and where they are kept.
#[derive(Debug, Default)]
pub(crate) struct Store {
    doc: Doc,
    /// None: kept in memory only (tests, or no data folder).
    path: Option<PathBuf>,
    /// Bumped whenever a size is added or dropped.
    rev: u64,
    /// Changes not written yet (an opened folder is not worth a write of its own).
    dirty: bool,
    /// When PS5Upload last wrote where, per console, newest last (in memory, at most
    /// [`MAX_MARKS`]): a size measured from before such a write is not kept ([`Store::put_since`]).
    marks: HashMap<String, VecDeque<(String, u64)>>,
}

impl Store {
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let doc = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Doc>(&b).ok())
            .unwrap_or_default();
        Store {
            doc,
            path,
            rev: 1,
            dirty: false,
            marks: HashMap::new(),
        }
    }

    pub(crate) fn rev(&self) -> u64 {
        self.rev
    }

    /// Writes the sizes when anything changed since the last write. Best effort: a size that is
    /// not kept is measured again some day, nothing worse.
    pub(crate) fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(path) = &self.path else { return };
        let Ok(bytes) = serde_json::to_vec(&self.doc) else {
            return;
        };
        if let Err(e) = write_atomic(path, &bytes) {
            crate::engine_log::record(
                "warn",
                format!("folder sizes: cannot write {}: {e}", path.display()),
            );
        }
    }

    fn console(&mut self, key: &str, addr: &str) -> &mut ConsoleSizes {
        if !self.doc.consoles.contains_key(key) && self.doc.consoles.len() >= MAX_CONSOLES {
            if let Some(oldest) = self
                .doc
                .consoles
                .iter()
                .min_by_key(|(_, c)| c.newest())
                .map(|(k, _)| k.clone())
            {
                self.doc.consoles.remove(&oldest);
            }
        }
        let c = self.doc.consoles.entry(key.to_string()).or_default();
        if !addr.is_empty() {
            c.addr = addr.to_string();
        }
        c
    }

    /// Keeps `path`'s size (replacing an older one), the oldest dropped past the cap.
    pub(crate) fn put(&mut self, key: &str, addr: &str, path: &str, entry: Entry) {
        let path = normalize(path);
        let c = self.console(key, addr);
        if c.sizes.get(&path).is_some_and(|old| *old == entry) {
            return;
        }
        c.sizes.insert(path, entry);
        while c.sizes.len() > MAX_PER_CONSOLE {
            let Some(oldest) = c
                .sizes
                .iter()
                .min_by_key(|(_, e)| e.measured_at_ms)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            c.sizes.remove(&oldest);
        }
        self.rev += 1;
        self.dirty = true;
    }

    /// PS5Upload wrote, deleted or moved `paths`: every size that covers them (the folder itself,
    /// what is inside it, every folder above it) is no longer right. True when one was dropped.
    pub(crate) fn changed(&mut self, key: &str, paths: &[String]) -> bool {
        let paths: Vec<String> = paths
            .iter()
            .filter(|p| valid_path(p))
            .map(|p| normalize(p))
            .collect();
        // Marked even when no size is known yet: a measure running now must not keep one.
        let at = now_ms();
        // As many consoles as sizes are kept for, the one written to longest ago dropped first
        // (a caller naming console after console grows nothing).
        if !self.marks.contains_key(key) && self.marks.len() >= MAX_CONSOLES {
            if let Some(oldest) = self
                .marks
                .iter()
                .min_by_key(|(_, m)| m.back().map_or(0, |(_, t)| *t))
                .map(|(k, _)| k.clone())
            {
                self.marks.remove(&oldest);
            }
        }
        let marks = self.marks.entry(key.to_string()).or_default();
        for p in &paths {
            marks.push_back((p.clone(), at));
        }
        while marks.len() > MAX_MARKS {
            marks.pop_front();
        }
        let Some(c) = self.doc.consoles.get_mut(key) else {
            return false;
        };
        let before = c.sizes.len();
        for p in &paths {
            c.sizes.retain(|k, _| !within(k, p) && !within(p, k));
        }
        let dropped = c.sizes.len() != before;
        if dropped {
            self.rev += 1;
            self.dirty = true;
        }
        dropped
    }

    /// PS5Upload wrote at or after `since_ms` somewhere `path` covers or that covers it.
    fn changed_since(&self, key: &str, path: &str, since_ms: u64) -> bool {
        let path = normalize(path);
        self.marks.get(key).is_some_and(|m| {
            m.iter()
                .rev()
                .take_while(|(_, at)| *at >= since_ms)
                .any(|(p, _)| within(p, &path) || within(&path, p))
        })
    }

    /// [`Store::put`] for a size measured from `since_ms` on: dropped when PS5Upload wrote there
    /// since (the size may predate the write, or be half of it). False when dropped.
    pub(crate) fn put_since(
        &mut self,
        key: &str,
        addr: &str,
        path: &str,
        entry: Entry,
        since_ms: u64,
    ) -> bool {
        if self.changed_since(key, path, since_ms) {
            return false;
        }
        self.put(key, addr, path, entry);
        true
    }

    /// `dir`'s own size and its folders' sizes by name.
    pub(crate) fn sizes_in(
        &self,
        key: &str,
        dir: &str,
    ) -> (Option<Entry>, BTreeMap<String, Entry>) {
        let dir = normalize(dir);
        let Some(c) = self.doc.consoles.get(key) else {
            return (None, BTreeMap::new());
        };
        let prefix = if dir == "/" {
            "/".to_string()
        } else {
            format!("{dir}/")
        };
        let children = c
            .sizes
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter_map(|(k, e)| {
                let name = &k[prefix.len()..];
                (!name.is_empty() && !name.contains('/')).then(|| (name.to_string(), e.clone()))
            })
            .collect();
        (c.sizes.get(&dir).cloned(), children)
    }

    pub(crate) fn note_opened(&mut self, key: &str, addr: &str, dir: &str, at_ms: u64) {
        let dir = normalize(dir);
        let c = self.console(key, addr);
        c.opened.insert(dir, at_ms);
        while c.opened.len() > MAX_OPENED {
            let Some(oldest) = c
                .opened
                .iter()
                .min_by_key(|(_, t)| **t)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            c.opened.remove(&oldest);
        }
        self.dirty = true;
    }

    /// What the fresh sweep would measure again now: known sizes older than a day of folders
    /// shown in a folder opened in the last week, `(console key, addr, path)`, oldest first.
    pub(crate) fn stale_opened(&self, now: u64) -> Vec<(String, String, String)> {
        let mut out: Vec<(u64, String, String, String)> = Vec::new();
        for (key, c) in &self.doc.consoles {
            if c.addr.is_empty() {
                continue;
            }
            for (dir, at) in &c.opened {
                if now.saturating_sub(*at) > OPENED_WITHIN_MS {
                    continue;
                }
                let (_, children) = self.sizes_in(key, dir);
                for (name, e) in children {
                    if now.saturating_sub(e.measured_at_ms) > DAY_MS {
                        let path = if dir == "/" {
                            format!("/{name}")
                        } else {
                            format!("{dir}/{name}")
                        };
                        out.push((e.measured_at_ms, key.clone(), c.addr.clone(), path));
                    }
                }
            }
        }
        out.sort();
        out.dedup_by(|a, b| a.1 == b.1 && a.3 == b.3);
        out.into_iter().map(|(_, k, a, p)| (k, a, p)).collect()
    }
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn store_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    crate::remote::store::data_dir().map(|d| d.join("state").join("folder-sizes.json"))
}

fn store() -> MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE
        .get_or_init(|| Mutex::new(Store::load(store_path())))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn key_of(addr: &str) -> String {
    crate::helper_stop::console_key(addr)
}

/// PS5Upload is about to write, delete or move `paths` on the console at `addr` (or just did): the
/// sizes that cover them are dropped (see [`Store::changed`]). Cheap; call it from every route
/// that changes the console's files.
pub(crate) fn changed<S: AsRef<str>>(addr: &str, paths: &[S]) {
    let paths: Vec<String> = paths
        .iter()
        .map(|p| p.as_ref().trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        return;
    }
    let mut s = store();
    if s.changed(&key_of(addr), &paths) {
        s.flush();
    }
}

/// A finished "What takes space?" walk of `root` that started at `started_ms`: its size and its
/// folders' sizes, which it measured anyway. A cancelled walk is not kept, nor a folder
/// PS5Upload wrote to while it ran.
pub(crate) fn record_walk(addr: &str, root: &str, w: &disk_usage::Walked, started_ms: u64) {
    if w.partial == Some("cancelled") || console_busy(addr) {
        return;
    }
    let at = now_ms();
    let key = key_of(addr);
    let root = normalize(root);
    let limited = w.partial.is_some();
    let entry = |d: &disk_usage::DuDir| Entry {
        bytes: d.bytes,
        files: Some(d.files),
        measured_at_ms: at,
        source: Source::Space,
        stored: false,
        partial: limited || d.skipped > 0,
    };
    let mut s = store();
    s.put_since(&key, addr, &root, entry(&w.tree), started_ms);
    for c in w.tree.children.iter().filter(|c| c.note.is_none()) {
        let path = if root == "/" {
            format!("/{}", c.name)
        } else {
            format!("{root}/{}", c.name)
        };
        s.put_since(&key, addr, &path, entry(c), started_ms);
    }
    s.flush();
}

/// A finished clean-up scan's status: the folders it sized (what they take on the drive).
pub(crate) fn record_cleanup_scan(addr: &str, status: &Value) {
    if status.get("state").and_then(Value::as_str) != Some("done") || console_busy(addr) {
        return;
    }
    let Some(items) = status.get("items").and_then(Value::as_array) else {
        return;
    };
    let age = status.get("age_ms").and_then(Value::as_u64).unwrap_or(0);
    if age > CLEANUP_FRESH_MS {
        // An old scan, read again (the page polls it): what it sized may have changed since,
        // and a size dropped for a write would come back.
        return;
    }
    let at = now_ms().saturating_sub(age);
    let key = key_of(addr);
    let mut s = store();
    for it in items {
        let (Some(path), Some(true)) = (
            it.get("path").and_then(Value::as_str),
            it.get("is_dir").and_then(Value::as_bool),
        ) else {
            continue;
        };
        if !valid_path(path) {
            continue;
        }
        let alloc = it.get("alloc_bytes").and_then(Value::as_u64).unwrap_or(0);
        let bytes = it.get("bytes").and_then(Value::as_u64).unwrap_or(0);
        let fresher = s
            .sizes_in(&key, parent_of(&normalize(path)))
            .1
            .get(normalize(path).rsplit('/').next().unwrap_or_default())
            .is_some_and(|e| e.measured_at_ms >= at);
        if fresher {
            continue;
        }
        s.put_since(
            &key,
            addr,
            path,
            Entry {
                bytes: if alloc > 0 { alloc } else { bytes },
                files: it.get("files").and_then(Value::as_u64),
                measured_at_ms: at,
                source: Source::Cleanup,
                stored: alloc > 0,
                partial: false,
            },
            at,
        );
    }
    s.flush();
}

// ─── Queue ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Item {
    pub addr: String,
    pub path: String,
    #[serde(skip)]
    key: String,
    /// Queued by the fresh sweep, not by the person.
    #[serde(skip)]
    fresh: bool,
}

impl Item {
    pub(crate) fn new(addr: &str, path: &str) -> Self {
        Item {
            addr: addr.to_string(),
            path: normalize(path),
            key: key_of(addr),
            fresh: false,
        }
    }
}

/// Live counts of the running measure.
#[derive(Debug, Default)]
pub(crate) struct Live {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
}

/// What a measure found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Measured {
    pub entry: Entry,
    /// Sizes of folders inside it that came for free (the fs.list walk's).
    pub inside: Vec<(String, Entry)>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MeasureError {
    /// Stopped by the stop flag.
    Stopped,
    Failed(String),
}

/// Runs one measure. The real one asks the console; tests stand in for it.
pub(crate) trait Measurer: Send + Sync {
    fn measure(
        &self,
        item: &Item,
        stop: &AtomicBool,
        live: &Live,
    ) -> Result<Measured, MeasureError>;
}

struct Running {
    item: Item,
    started_at_ms: u64,
    stop: Arc<AtomicBool>,
    /// Stopped because its console got busy: it goes back first in line.
    paused: Arc<AtomicBool>,
    live: Arc<Live>,
}

#[derive(Default)]
struct QueueInner {
    items: VecDeque<Item>,
    running: Option<Running>,
    /// Consoles whose queued folders wait for a job there to end.
    waiting: HashSet<String>,
    last_error: Option<(Item, String, u64)>,
    /// When the fresh sweep last tried each folder: at most once a day, whatever the outcome.
    fresh_tried: HashMap<(String, String), u64>,
}

/// The measuring queue: one folder at a time.
pub(crate) struct Queue {
    inner: Mutex<QueueInner>,
    cv: Condvar,
    busy: Box<dyn Fn(&str) -> bool + Send + Sync>,
}

impl Queue {
    pub(crate) fn new(busy: Box<dyn Fn(&str) -> bool + Send + Sync>) -> Self {
        Queue {
            inner: Mutex::new(QueueInner::default()),
            cv: Condvar::new(),
            busy,
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `items` the person asked for, after what is queued; one already queued or running
    /// is not queued twice. A fresh-sweep item of the same folder becomes the person's.
    pub(crate) fn enqueue(&self, items: Vec<Item>) {
        let mut g = self.lock();
        for it in items {
            if g.items.len() >= MAX_QUEUE {
                break;
            }
            if g.running
                .as_ref()
                .is_some_and(|r| r.item.key == it.key && r.item.path == it.path)
            {
                continue;
            }
            if let Some(q) = g
                .items
                .iter_mut()
                .find(|q| q.key == it.key && q.path == it.path)
            {
                q.fresh &= it.fresh;
                continue;
            }
            g.items.push_back(it);
        }
        self.cv.notify_all();
    }

    /// Stops what matches: `key` None is every console, `paths` None every folder of it.
    pub(crate) fn cancel(&self, key: Option<&str>, paths: Option<&[String]>) {
        let hit = |it: &Item| {
            key.is_none_or(|k| it.key == k)
                && paths.is_none_or(|ps| ps.iter().any(|p| normalize(p) == it.path))
        };
        let mut g = self.lock();
        g.items.retain(|it| !hit(it));
        if let Some(r) = &g.running {
            if hit(&r.item) {
                r.stop.store(true, Ordering::Relaxed);
                r.paused.store(false, Ordering::Relaxed);
            }
        }
        self.cv.notify_all();
    }

    pub(crate) fn is_idle(&self) -> bool {
        let g = self.lock();
        g.items.is_empty() && g.running.is_none()
    }

    /// The queue as the app shows it, for one console or all.
    pub(crate) fn state(&self, key: Option<&str>, rev: u64, keep_fresh: bool) -> Value {
        let g = self.lock();
        let mine = |it: &Item| key.is_none_or(|k| it.key == k);
        let running = g.running.as_ref().filter(|r| mine(&r.item)).map(|r| {
            json!({
                "addr": r.item.addr,
                "path": r.item.path,
                "started_at_ms": r.started_at_ms,
                "files": r.live.files.load(Ordering::Relaxed),
                "bytes": r.live.bytes.load(Ordering::Relaxed),
            })
        });
        let queued: Vec<&Item> = g.items.iter().filter(|it| mine(it)).collect();
        let paused = queued.iter().any(|it| g.waiting.contains(&it.key));
        let last_error = g.last_error.as_ref().filter(|(it, _, _)| mine(it)).map(
            |(it, e, at)| json!({ "addr": it.addr, "path": it.path, "error": e, "at_ms": at }),
        );
        json!({
            "rev": rev,
            "running": running,
            "queued": queued,
            "paused": if paused { Some("busy") } else { None },
            "keep_fresh": keep_fresh,
            "last_error": last_error,
        })
    }

    /// Takes the next folder whose console is free, waiting up to `wait` for one. None: nothing
    /// could start (all waiting for busy consoles, or nothing queued).
    fn take(&self, wait: Duration) -> Option<Running> {
        let mut g = self.lock();
        let mut waited = false;
        loop {
            let mut waiting = HashSet::new();
            let mut pick = None;
            for (i, it) in g.items.iter().enumerate() {
                if waiting.contains(&it.key) {
                    continue;
                }
                if (self.busy)(&it.addr) {
                    waiting.insert(it.key.clone());
                } else {
                    pick = Some(i);
                    break;
                }
            }
            g.waiting = waiting;
            if let Some(i) = pick {
                let item = g.items.remove(i).expect("picked from the queue");
                let r = Running {
                    item,
                    started_at_ms: now_ms(),
                    stop: Arc::new(AtomicBool::new(false)),
                    paused: Arc::new(AtomicBool::new(false)),
                    live: Arc::new(Live::default()),
                };
                g.running = Some(Running {
                    item: r.item.clone(),
                    started_at_ms: r.started_at_ms,
                    stop: r.stop.clone(),
                    paused: r.paused.clone(),
                    live: r.live.clone(),
                });
                return Some(r);
            }
            if waited {
                return None;
            }
            waited = true;
            let wait = if g.items.is_empty() {
                wait
            } else {
                wait.min(BUSY_RETRY)
            };
            g = self
                .cv
                .wait_timeout(g, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Runs `r` with `m`, stopping it when cancelled or when its console gets busy, and keeps
    /// what it found in `store`.
    fn run_one(&self, r: Running, m: &dyn Measurer, store: &dyn Fn(&Item, Measured, u64)) {
        let done = AtomicBool::new(false);
        let out = std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if !r.stop.load(Ordering::Relaxed) && (self.busy)(&r.item.addr) {
                        r.paused.store(true, Ordering::Relaxed);
                        r.stop.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(WATCH);
                }
            });
            let out = m.measure(&r.item, &r.stop, &r.live);
            done.store(true, Ordering::Relaxed);
            out
        });
        // A measure that ended as its console got busy may have counted a write half done:
        // measured again once the console is free, like one that was stopped.
        let paused = r.paused.load(Ordering::Relaxed);
        // Kept before the slot is freed, so a poll never sees the folder neither measuring nor known.
        if let (Ok(found), false) = (&out, paused) {
            store(&r.item, found.clone(), r.started_at_ms);
        }
        let mut g = self.lock();
        g.running = None;
        match out {
            Ok(_) | Err(MeasureError::Stopped) if paused => {
                g.items.push_front(r.item.clone());
            }
            Ok(_) => {}
            Err(MeasureError::Stopped) => {}
            Err(MeasureError::Failed(e)) => {
                crate::engine_log::record(
                    "info",
                    format!(
                        "folder sizes: {} on {}: not measured: {e}",
                        r.item.path, r.item.addr
                    ),
                );
                g.last_error = Some((r.item.clone(), e, now_ms()));
            }
        }
        self.cv.notify_all();
    }

    /// The fresh sweep's folders, when it is on and the engine has nothing else to do.
    fn queue_fresh(&self, candidates: Vec<(String, String, String)>, now: u64) {
        let mut g = self.lock();
        if !g.items.is_empty() || g.running.is_some() {
            return;
        }
        g.fresh_tried
            .retain(|_, at| now.saturating_sub(*at) < DAY_MS);
        let mut n = 0;
        for (key, addr, path) in candidates {
            if n >= FRESH_BATCH {
                break;
            }
            let id = (key.clone(), path.clone());
            if g.fresh_tried.contains_key(&id) || (self.busy)(&addr) {
                continue;
            }
            g.fresh_tried.insert(id, now);
            g.items.push_back(Item {
                addr,
                path,
                key,
                fresh: true,
            });
            n += 1;
        }
        self.cv.notify_all();
    }
}

/// What a measure that started at `started_ms` found as kept sizes, but none PS5Upload wrote to
/// since it started.
fn keep(item: &Item, found: Measured, started_ms: u64) {
    let mut s = store();
    s.put_since(&item.key, &item.addr, &item.path, found.entry, started_ms);
    for (path, e) in found.inside {
        s.put_since(&item.key, &item.addr, &path, e, started_ms);
    }
    s.flush();
}

/// A transfer, console file operation or install runs on `addr` now: what a walk sees there may
/// be half written.
pub(crate) fn console_busy(addr: &str) -> bool {
    (queue().busy)(addr)
}

fn queue() -> &'static Arc<Queue> {
    static QUEUE: OnceLock<Arc<Queue>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let installs = INSTALLS.get().cloned();
        Arc::new(Queue::new(Box::new(move |addr: &str| {
            crate::helper_stop::jobs_running(addr)
                || installs.as_ref().is_some_and(|i| i.jobs.active_for(addr))
        })))
    })
}

static INSTALLS: OnceLock<crate::pkg_install::PkgInstallStateHandle> = OnceLock::new();

/// Starts the measuring worker (once): it sleeps until something is queued. `installs` tells it
/// when an install runs on a console.
pub(crate) fn start(installs: crate::pkg_install::PkgInstallStateHandle) {
    static STARTED: OnceLock<()> = OnceLock::new();
    let _ = INSTALLS.set(installs);
    STARTED.get_or_init(|| {
        let q = queue().clone();
        let rt = tokio::runtime::Handle::try_current().ok();
        let _ = std::thread::Builder::new()
            .name("folder-sizes".into())
            .spawn(move || {
                let _rt = rt.as_ref().map(|h| h.enter());
                worker(&q, &ConsoleMeasurer);
            });
    });
}

fn worker(q: &Queue, m: &dyn Measurer) {
    let mut last_tick = std::time::Instant::now();
    loop {
        if let Some(r) = q.take(IDLE_TICK) {
            q.run_one(r, m, &keep);
            continue;
        }
        if last_tick.elapsed() < IDLE_TICK / 2 {
            continue;
        }
        last_tick = std::time::Instant::now();
        let candidates = {
            let mut s = store();
            s.flush();
            if crate::settings_api::flag("keep_folder_sizes_fresh") && q.is_idle() {
                s.stale_opened(now_ms())
            } else {
                Vec::new()
            }
        };
        if !candidates.is_empty() {
            q.queue_fresh(candidates, now_ms());
        }
    }
}

/// The real measure: the helper's tree-size job, or the fs.list walk for a helper from before it.
struct ConsoleMeasurer;

static OP_IDS: AtomicU64 = AtomicU64::new(0xF5_0000_0000_0001);

impl Measurer for ConsoleMeasurer {
    fn measure(
        &self,
        item: &Item,
        stop: &AtomicBool,
        live: &Live,
    ) -> Result<Measured, MeasureError> {
        let op_id = OP_IDS.fetch_add(1, Ordering::Relaxed);
        let done = AtomicBool::new(false);
        let rt = tokio::runtime::Handle::try_current().ok();
        let r = std::thread::scope(|s| {
            s.spawn(|| {
                let _rt = rt.as_ref().map(|h| h.enter());
                while !done.load(Ordering::Relaxed) {
                    // Asked again each round until the job ends: one sent before the helper
                    // knew the job, or lost, would leave it counting through a transfer.
                    if stop.load(Ordering::Relaxed) {
                        let _ = ps5upload_core::mgmt::op_cancel(&item.addr, op_id);
                    }
                    if let Ok(Some(Some(p))) = ps5upload_core::mgmt::op_progress(&item.addr, op_id)
                    {
                        live.files.store(p.files_done, Ordering::Relaxed);
                        live.bytes.store(p.bytes_done, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
            let r =
                ps5upload_core::fs_ops::tree_size(&item.addr, &item.path, op_id, MEASURE_DEADLINE);
            done.store(true, Ordering::Relaxed);
            r
        });
        match r {
            Ok(t) => Ok(Measured {
                entry: Entry {
                    bytes: if t.alloc > 0 { t.alloc } else { t.bytes },
                    files: Some(t.files),
                    measured_at_ms: now_ms(),
                    source: Source::Measured,
                    stored: t.alloc > 0,
                    partial: t.skipped > 0,
                },
                inside: Vec::new(),
            }),
            Err(_) if stop.load(Ordering::Relaxed) => Err(MeasureError::Stopped),
            Err(e) if ps5upload_core::fs_ops::is_unknown_op(&e) => walk_measure(item, stop, live),
            Err(e) => Err(MeasureError::Failed(format!("{e:#}"))),
        }
    }
}

/// A helper from before `JOB_OP_TREE_SIZE`: the "What takes space?" walk, one list at a time.
fn walk_measure(item: &Item, stop: &AtomicBool, live: &Live) -> Result<Measured, MeasureError> {
    let progress = disk_usage::Progress::default();
    let limits = disk_usage::Limits {
        workers: 1,
        ..disk_usage::Limits::default()
    };
    let done = AtomicBool::new(false);
    let w = std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                live.files
                    .store(progress.files.load(Ordering::Relaxed), Ordering::Relaxed);
                live.bytes
                    .store(progress.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let w = disk_usage::walk_console(&item.addr, &item.path, &limits, &progress, &|| {
            stop.load(Ordering::Relaxed)
        });
        done.store(true, Ordering::Relaxed);
        w
    })
    .map_err(|e| MeasureError::Failed(format!("{e:#}")))?;
    if w.partial == Some("cancelled") {
        return Err(MeasureError::Stopped);
    }
    let at = now_ms();
    let limited = w.partial.is_some();
    let entry = |d: &disk_usage::DuDir| Entry {
        bytes: d.bytes,
        files: Some(d.files),
        measured_at_ms: at,
        source: Source::Measured,
        stored: false,
        partial: limited || d.skipped > 0,
    };
    let inside = w
        .tree
        .children
        .iter()
        .filter(|c| c.note.is_none())
        .map(|c| {
            let path = if item.path == "/" {
                format!("/{}", c.name)
            } else {
                format!("{}/{}", item.path, c.name)
            };
            (path, entry(c))
        })
        .collect();
    Ok(Measured {
        entry: entry(&w.tree),
        inside,
    })
}

// ─── Routes ─────────────────────────────────────────────────────────────────

fn queue_state(key: Option<&str>) -> Value {
    let rev = store().rev();
    queue().state(
        key,
        rev,
        crate::settings_api::flag("keep_folder_sizes_fresh"),
    )
}

#[derive(Deserialize)]
pub(crate) struct FolderSizesQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// The folder shown: its size and the sizes of the folders directly in it.
    dir: String,
}

/// GET /api/ps5/folder-sizes
pub(crate) async fn sizes(
    State(state): State<AppState>,
    Query(q): Query<FolderSizesQuery>,
) -> Response {
    if !valid_path(&q.dir) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "dir must be absolute, without '..'",
        )
        .into_response();
    }
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let key = key_of(&addr);
    let dir = normalize(&q.dir);
    let (own, children) = {
        let mut s = store();
        s.note_opened(&key, &addr, &dir, now_ms());
        s.sizes_in(&key, &dir)
    };
    Json(json!({
        "dir": dir,
        "self": own,
        "children": children,
        "queue": queue_state(Some(&key)),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct FolderQueueQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

/// GET /api/ps5/folder-sizes/queue
pub(crate) async fn queue_status(
    State(state): State<AppState>,
    Query(q): Query<FolderQueueQuery>,
) -> Response {
    let key = q
        .addr
        .map(|a| key_of(&console_addr_or_default(Some(a), &state.default_ps5_addr)));
    Json(queue_state(key.as_deref())).into_response()
}

#[derive(Deserialize)]
pub(crate) struct FolderMeasureReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// Folders to measure.
    #[serde(default)]
    paths: Vec<String>,
    /// Measure every folder directly in this one ("Measure everything here").
    #[serde(default)]
    dir: Option<String>,
}

/// POST /api/ps5/folder-sizes/measure
pub(crate) async fn measure(
    State(state): State<AppState>,
    Json(req): Json<FolderMeasureReq>,
) -> Response {
    let bad = req
        .paths
        .iter()
        .chain(req.dir.iter())
        .any(|p| !valid_path(p));
    if bad || (req.paths.is_empty() && req.dir.is_none()) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "give paths or dir: absolute folders, without '..'",
        )
        .into_response();
    }
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let mut items: Vec<Item> = req.paths.iter().map(|p| Item::new(&addr, p)).collect();
    if let Some(dir) = req.dir {
        let dir = normalize(&dir);
        let a = addr.clone();
        let listed = tokio::task::spawn_blocking(move || {
            disk_usage::list_folder(&a, &dir).map(|entries| {
                entries
                    .into_iter()
                    .filter(|e| e.kind == "dir")
                    .map(|e| {
                        if dir == "/" {
                            format!("/{}", e.name)
                        } else {
                            format!("{dir}/{}", e.name)
                        }
                    })
                    .filter(|p| !disk_usage::PSEUDO.contains(&p.as_str()))
                    .collect::<Vec<_>>()
            })
        })
        .await;
        match listed {
            Ok(Ok(mut paths)) => {
                paths.sort();
                items.extend(paths.iter().map(|p| Item::new(&addr, p)));
            }
            Ok(Err(e)) => {
                return json_err(StatusCode::BAD_GATEWAY, &format!("{e:#}")).into_response()
            }
            Err(e) => {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()).into_response()
            }
        }
    }
    let key = key_of(&addr);
    queue().enqueue(items);
    Json(queue_state(Some(&key))).into_response()
}

#[derive(Deserialize)]
pub(crate) struct FolderCancelReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// The folders to stop; every folder of the console without it.
    #[serde(default)]
    paths: Option<Vec<String>>,
}

/// POST /api/ps5/folder-sizes/cancel
pub(crate) async fn cancel(
    State(state): State<AppState>,
    Json(req): Json<FolderCancelReq>,
) -> Response {
    let key = req
        .addr
        .map(|a| key_of(&console_addr_or_default(Some(a), &state.default_ps5_addr)));
    queue().cancel(key.as_deref(), req.paths.as_deref());
    Json(queue_state(key.as_deref())).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn e(bytes: u64, at: u64) -> Entry {
        Entry {
            bytes,
            files: Some(1),
            measured_at_ms: at,
            source: Source::Measured,
            stored: true,
            partial: false,
        }
    }

    #[test]
    fn a_folder_shows_its_own_size_and_its_folders_but_nothing_deeper() {
        let mut s = Store::load(None);
        s.put("k", "1.2.3.4", "/data/games", e(10, 1));
        s.put("k", "1.2.3.4", "/data/games/A", e(4, 1));
        s.put("k", "1.2.3.4", "/data/games/B/", e(6, 1));
        s.put("k", "1.2.3.4", "/data/games/A/sub", e(1, 1));
        s.put("k", "1.2.3.4", "/data/gamesX", e(9, 1));
        s.put("other", "5.6.7.8", "/data/games/C", e(9, 1));
        let (own, children) = s.sizes_in("k", "/data/games/");
        assert_eq!(own.map(|e| e.bytes), Some(10));
        assert_eq!(children.keys().collect::<Vec<_>>(), ["A", "B"]);
        let (own, children) = s.sizes_in("k", "/");
        assert!(own.is_none());
        assert!(children.is_empty());
        assert!(s.sizes_in("nobody", "/data").1.is_empty());
    }

    #[test]
    fn our_own_change_drops_the_folder_what_is_inside_and_every_folder_above() {
        let mut s = Store::load(None);
        for p in [
            "/data",
            "/data/games",
            "/data/games/A",
            "/data/games/A/sub",
            "/data/games/B",
            "/data/other",
            "/mnt/usb0/games",
        ] {
            s.put("k", "a", p, e(1, 1));
        }
        let rev = s.rev();
        assert!(s.changed("k", &["/data/games/A/".to_string()]));
        assert!(s.rev() > rev);
        let left: Vec<String> = s.doc.consoles["k"].sizes.keys().cloned().collect();
        assert_eq!(left, ["/data/games/B", "/data/other", "/mnt/usb0/games"]);
        // A file written inside B drops B and what is above it, not B's neighbours.
        assert!(s.changed("k", &["/data/games/B/new.bin".to_string()]));
        let left: Vec<String> = s.doc.consoles["k"].sizes.keys().cloned().collect();
        assert_eq!(left, ["/data/other", "/mnt/usb0/games"]);
        // Nothing known there, another console, a bad path: nothing changes.
        let rev = s.rev();
        assert!(!s.changed("k", &["/user/app".to_string()]));
        assert!(!s.changed("nobody", &["/data".to_string()]));
        assert!(!s.changed("k", &["../x".to_string()]));
        assert_eq!(s.rev(), rev);
    }

    #[test]
    fn the_store_stays_small_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("ps5u-fsz-{}", uuid::Uuid::new_v4()));
        let path = dir.join("state").join("folder-sizes.json");
        let mut s = Store::load(Some(path.clone()));
        for i in 0..(MAX_PER_CONSOLE + 5) {
            s.put("k", "a", &format!("/data/f{i:05}"), e(i as u64, i as u64));
        }
        let sizes = &s.doc.consoles["k"].sizes;
        assert_eq!(sizes.len(), MAX_PER_CONSOLE);
        assert!(!sizes.contains_key("/data/f00000"), "the oldest went first");
        s.note_opened("k", "a", "/data", 5);
        s.flush();
        let back = Store::load(Some(path.clone()));
        assert_eq!(back.doc.consoles["k"].sizes.len(), MAX_PER_CONSOLE);
        assert_eq!(back.doc.consoles["k"].opened.get("/data"), Some(&5));
        assert_eq!(back.doc.consoles["k"].addr, "a");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_fresh_sweep_only_reaches_old_sizes_in_folders_the_person_opened() {
        let now = 100 * DAY_MS;
        let mut s = Store::load(None);
        s.put("k", "a", "/data/games/Old", e(1, now - 2 * DAY_MS));
        s.put("k", "a", "/data/games/New", e(1, now - DAY_MS / 2));
        s.put("k", "a", "/data/elsewhere/Old", e(1, now - 3 * DAY_MS));
        s.put("k", "a", "/data/games/Old/inner", e(1, now - 5 * DAY_MS));
        // Nothing opened: nothing to refresh.
        assert!(s.stale_opened(now).is_empty());
        s.note_opened("k", "a", "/data/games", now - DAY_MS);
        assert_eq!(
            s.stale_opened(now),
            [(
                "k".to_string(),
                "a".to_string(),
                "/data/games/Old".to_string()
            )]
        );
        // Opened too long ago: out of reach.
        s.note_opened("k", "a", "/data/games", now - 8 * DAY_MS);
        assert!(s.stale_opened(now).is_empty());
    }

    #[test]
    fn a_walk_and_a_cleanup_scan_give_sizes_for_free() {
        let child = |name: &str, bytes: u64, note: Option<&'static str>| disk_usage::DuDir {
            name: name.into(),
            bytes,
            files: 2,
            note,
            ..Default::default()
        };
        let w = disk_usage::Walked {
            tree: disk_usage::DuDir {
                name: "free".into(),
                bytes: 30,
                files: 4,
                children: vec![
                    child("A", 20, None),
                    child("B", 10, None),
                    child("Locked", 0, Some("unreadable")),
                ],
                ..Default::default()
            },
            partial: None,
        };
        record_walk("10.9.8.7", "/data/free-test", &w, now_ms());
        let key = key_of("10.9.8.7");
        let (own, children) = store().sizes_in(&key, "/data/free-test");
        assert_eq!(
            own.map(|e| (e.bytes, e.source, e.stored)),
            Some((30, Source::Space, false))
        );
        assert_eq!(children.keys().collect::<Vec<_>>(), ["A", "B"]);
        // A cancelled walk is not kept.
        let cancelled = disk_usage::Walked {
            partial: Some("cancelled"),
            ..w
        };
        record_walk("10.9.8.7", "/data/free-cancelled", &cancelled, now_ms());
        assert!(store().sizes_in(&key, "/data/free-cancelled").0.is_none());

        record_cleanup_scan(
            "10.9.8.7",
            &json!({"state": "done", "age_ms": 0, "items": [
                {"path": "/data/free-clean/X", "is_dir": true, "bytes": 5, "alloc_bytes": 8192, "files": 1},
                {"path": "/data/free-clean/y.pkg", "is_dir": false, "bytes": 5, "alloc_bytes": 8192},
            ]}),
        );
        let (_, children) = store().sizes_in(&key, "/data/free-clean");
        let x = &children["X"];
        assert_eq!((x.bytes, x.stored, x.source), (8192, true, Source::Cleanup));
        assert_eq!(children.len(), 1, "files are not folder sizes");
        changed("10.9.8.7", &["/data/free-test", "/data/free-clean"]);
        assert!(store().sizes_in(&key, "/data/free-test").1.is_empty());
    }

    /// A size measured from before PS5Upload wrote inside it (or above it) is not kept, even
    /// when the write came while no size was known yet; one of a folder elsewhere is.
    #[test]
    fn a_size_measured_across_a_write_is_not_kept() {
        let mut s = Store::load(None);
        let started = now_ms();
        s.changed("k", &["/data/games/X/eboot.bin".to_string()]);
        assert!(!s.put_since("k", "a", "/data/games", e(5, now_ms()), started));
        assert!(!s.put_since("k", "a", "/data/games/X", e(5, now_ms()), started));
        // A sibling of what was written is not touched by it.
        assert!(s.put_since("k", "a", "/data/games/X/sub", e(5, now_ms()), started));
        assert!(s.put_since("k", "a", "/data/other", e(5, now_ms()), started));
        assert!(s.sizes_in("k", "/data").1.contains_key("other"));
        assert!(!s.sizes_in("k", "/data").1.contains_key("games"));
        // A measure that started after the write keeps its size.
        std::thread::sleep(Duration::from_millis(2));
        assert!(s.put_since("k", "a", "/data/games", e(5, now_ms()), now_ms()));
        // Another spelling of the same folder is the same folder.
        s.changed("k", &["/data//games/./X".to_string()]);
        assert!(!s.sizes_in("k", "/data").1.contains_key("games"));
    }

    /// Writes are remembered for a bounded number of consoles and writes each.
    #[test]
    fn remembered_writes_stay_bounded() {
        let mut s = Store::load(None);
        for i in 0..MAX_CONSOLES * 3 {
            s.changed(&format!("console-{i}"), &["/data/x".to_string()]);
        }
        assert_eq!(s.marks.len(), MAX_CONSOLES);
        for i in 0..MAX_MARKS + 10 {
            s.changed("one", &[format!("/data/{i}")]);
        }
        assert_eq!(s.marks["one"].len(), MAX_MARKS);
    }

    /// An old clean-up scan read again (the page polls it) gives no sizes: a size dropped for a
    /// write since would come back.
    #[test]
    fn an_old_cleanup_scan_gives_no_sizes() {
        record_cleanup_scan(
            "10.9.8.6",
            &json!({"state": "done", "age_ms": CLEANUP_FRESH_MS + 1, "items": [
                {"path": "/data/old-clean/X", "is_dir": true, "bytes": 5, "alloc_bytes": 8192},
            ]}),
        );
        assert!(store()
            .sizes_in(&key_of("10.9.8.6"), "/data/old-clean")
            .1
            .is_empty());
    }

    /// A measure that finished just as its console got busy is measured again, not kept.
    #[test]
    fn a_measure_that_ends_as_its_console_gets_busy_is_not_kept() {
        let busy = Arc::new(AtomicBool::new(false));
        let b = busy.clone();
        let q = Queue::new(Box::new(move |_| b.load(Ordering::SeqCst)));
        q.enqueue(items("a", &["/data/1"]));
        let r = q.take(Duration::from_millis(10)).unwrap();
        // The watcher saw the console busy, but the measure came back with a size anyway.
        r.paused.store(true, Ordering::SeqCst);
        r.stop.store(true, Ordering::SeqCst);
        busy.store(true, Ordering::SeqCst);
        let kept = Mutex::new(Vec::new());
        q.run_one(r, &Slow::new(0), &|i, _, _| {
            kept.lock().unwrap().push(i.path.clone())
        });
        assert!(kept.lock().unwrap().is_empty());
        let st = q.state(None, 0, false);
        assert_eq!(st["queued"][0]["path"], "/data/1", "measured again later");
    }

    /// A measurer that takes `ms` per folder, counts how many run at once, and stops when told.
    struct Slow {
        ms: u64,
        now: AtomicUsize,
        most: AtomicUsize,
        done: Mutex<Vec<String>>,
    }

    impl Slow {
        fn new(ms: u64) -> Self {
            Slow {
                ms,
                now: AtomicUsize::new(0),
                most: AtomicUsize::new(0),
                done: Mutex::new(Vec::new()),
            }
        }
    }

    impl Measurer for Slow {
        fn measure(
            &self,
            item: &Item,
            stop: &AtomicBool,
            live: &Live,
        ) -> Result<Measured, MeasureError> {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(n, Ordering::SeqCst);
            let mut out = Ok(());
            for _ in 0..self.ms / 5 {
                if stop.load(Ordering::Relaxed) {
                    out = Err(MeasureError::Stopped);
                    break;
                }
                live.files.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
            self.now.fetch_sub(1, Ordering::SeqCst);
            out?;
            self.done.lock().unwrap().push(item.path.clone());
            Ok(Measured {
                entry: e(1, now_ms()),
                inside: Vec::new(),
            })
        }
    }

    fn items(addr: &str, paths: &[&str]) -> Vec<Item> {
        paths.iter().map(|p| Item::new(addr, p)).collect()
    }

    /// Runs the queue until it is empty (or `limit` passes), as the worker does.
    fn drain(q: &Queue, m: &Slow, limit: Duration) {
        let until = std::time::Instant::now() + limit;
        while std::time::Instant::now() < until {
            match q.take(Duration::from_millis(20)) {
                Some(r) => q.run_one(r, m, &|_, _, _| {}),
                None if q.is_idle() => return,
                None => {}
            }
        }
    }

    #[test]
    fn the_queue_measures_one_folder_at_a_time_in_order_without_repeats() {
        let q = Arc::new(Queue::new(Box::new(|_| false)));
        let m = Arc::new(Slow::new(20));
        q.enqueue(items("a", &["/data/1", "/data/2", "/data/1", "/data/3/"]));
        q.enqueue(items("a", &["/data/2"]));
        // Two workers at once still run one measure at a time per take: the queue hands each
        // folder out once.
        std::thread::scope(|s| {
            for _ in 0..2 {
                let (q, m) = (q.clone(), m.clone());
                s.spawn(move || drain(&q, &m, Duration::from_secs(5)));
            }
        });
        let mut done = m.done.lock().unwrap().clone();
        done.sort();
        assert_eq!(done, ["/data/1", "/data/2", "/data/3"]);
        let single = Queue::new(Box::new(|_| false));
        let one = Slow::new(10);
        single.enqueue(items("a", &["/x/1", "/x/2", "/x/3"]));
        drain(&single, &one, Duration::from_secs(5));
        assert_eq!(one.most.load(Ordering::SeqCst), 1, "never two at once");
        assert_eq!(*one.done.lock().unwrap(), ["/x/1", "/x/2", "/x/3"]);
    }

    #[test]
    fn cancel_stops_the_running_folder_and_drops_the_queued_ones() {
        let q = Arc::new(Queue::new(Box::new(|_| false)));
        let m = Arc::new(Slow::new(2_000));
        q.enqueue(items("a", &["/data/1", "/data/2"]));
        q.enqueue(items("b", &["/data/9"]));
        let t = {
            let (q, m) = (q.clone(), m.clone());
            std::thread::spawn(move || {
                let r = q.take(Duration::from_millis(10)).unwrap();
                q.run_one(r, &*m, &|_, _, _| {});
            })
        };
        while q.state(None, 0, false)["running"].is_null() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let st = q.state(Some(&key_of("a")), 7, true);
        assert_eq!(st["running"]["path"], "/data/1");
        assert_eq!(
            st["queued"].as_array().unwrap().len(),
            1,
            "only console a's"
        );
        assert_eq!(
            (st["rev"].as_u64(), st["keep_fresh"].as_bool()),
            (Some(7), Some(true))
        );
        let t0 = std::time::Instant::now();
        q.cancel(Some(&key_of("a")), None);
        t.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "stopped, not finished"
        );
        assert!(m.done.lock().unwrap().is_empty());
        let st = q.state(None, 0, false);
        assert!(st["running"].is_null());
        let left: Vec<&str> = st["queued"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap())
            .collect();
        assert_eq!(left, ["/data/9"], "the other console's folder stays");
        // One folder by path.
        q.enqueue(items("b", &["/data/8"]));
        q.cancel(Some(&key_of("b")), Some(&["/data/9/".to_string()]));
        let left: Vec<String> = q.state(None, 0, false)["queued"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(left, ["/data/8"]);
    }

    #[test]
    fn a_busy_console_pauses_the_queue_and_the_running_folder_goes_back_first_in_line() {
        let busy = Arc::new(AtomicBool::new(false));
        let b = busy.clone();
        let q = Arc::new(Queue::new(Box::new(move |addr| {
            addr == "a" && b.load(Ordering::SeqCst)
        })));
        let m = Arc::new(Slow::new(1_000));
        q.enqueue(items("a", &["/data/1", "/data/2"]));
        let t = {
            let (q, m) = (q.clone(), m.clone());
            std::thread::spawn(move || {
                let r = q.take(Duration::from_millis(10)).unwrap();
                q.run_one(r, &*m, &|_, _, _| {});
            })
        };
        while q.state(None, 0, false)["running"].is_null() {
            std::thread::sleep(Duration::from_millis(5));
        }
        busy.store(true, Ordering::SeqCst); // a transfer starts on console a
        t.join().unwrap();
        assert!(m.done.lock().unwrap().is_empty(), "the measure was stopped");
        // Nothing starts while it is busy, and the queue says why.
        assert!(q.take(Duration::from_millis(30)).is_none());
        let st = q.state(None, 0, false);
        assert_eq!(st["paused"], "busy");
        let left: Vec<&str> = st["queued"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap())
            .collect();
        assert_eq!(left, ["/data/1", "/data/2"], "put back first in line");
        // Another console's folder is not held up.
        q.enqueue(items("other", &["/x"]));
        let r = q
            .take(Duration::from_millis(30))
            .expect("the free console's folder");
        assert_eq!(r.item.path, "/x");
        q.run_one(r, &Slow::new(5), &|_, _, _| {});
        // The transfer ends: the queue goes on.
        busy.store(false, Ordering::SeqCst);
        let quick = Slow::new(5);
        drain(&q, &quick, Duration::from_secs(5));
        assert_eq!(*quick.done.lock().unwrap(), ["/data/1", "/data/2"]);
        assert!(q.state(None, 0, false)["paused"].is_null());
    }

    #[test]
    fn the_fresh_sweep_waits_for_an_idle_queue_and_tries_a_folder_once_a_day() {
        let q = Queue::new(Box::new(|addr| addr == "busy"));
        let now = 10 * DAY_MS;
        let c = |addr: &str, p: &str| (key_of(addr), addr.to_string(), p.to_string());
        q.enqueue(items("a", &["/mine"]));
        q.queue_fresh(vec![c("a", "/old")], now);
        assert_eq!(
            q.state(None, 0, false)["queued"].as_array().unwrap().len(),
            1,
            "the person's first"
        );
        q.cancel(None, None);
        q.queue_fresh(vec![c("a", "/old"), c("busy", "/b")], now);
        let st = q.state(None, 0, false);
        let left: Vec<&str> = st["queued"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["path"].as_str().unwrap())
            .collect();
        assert_eq!(left, ["/old"], "not on a busy console");
        q.cancel(None, None);
        q.queue_fresh(vec![c("a", "/old")], now + DAY_MS / 2);
        assert!(q.is_idle(), "tried today already");
        q.queue_fresh(vec![c("a", "/old")], now + DAY_MS + 1);
        assert!(!q.is_idle(), "a day later it may go again");
    }
}
