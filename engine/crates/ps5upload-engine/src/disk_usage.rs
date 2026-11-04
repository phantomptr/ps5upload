//! Disk usage (#495): a console folder's recursive sizes, walked with `fs.list`.
//!
//! * `POST /api/ps5/disk-usage`             `{addr, path}` -> the walk's status (with its `id`)
//! * `GET  /api/ps5/disk-usage/{id}`        -> `{id, path, state, dirs_scanned, files, bytes,
//!   skipped, elapsed_ms, partial?, error?, tree?}`
//! * `POST /api/ps5/disk-usage/{id}/cancel` -> the status; the walk stops at its next folder
//!
//! The helper has no recursive size, so the engine lists every folder below `path`, [`WORKERS`]
//! lists in flight: a share of the helper's management slots that leaves room for the app's
//! own pollers. `state` is `running`, then `done`, `cancelled` or `failed`. `tree` comes with
//! the final status: every folder below `path` with its recursive totals and its largest
//! files, so the app can drill in without walking again.
//!
//! A walk never follows a symbolic link (the helper `lstat`s; a link is kind `link`), never
//! enters the console's system trees ([`PSEUDO`]) or another drive mounted deeper inside the
//! folder, and does not fail on a folder it cannot read: each of those is counted as skipped
//! and named in the folder's `note`. It stops at [`MAX_DIRS`] folders or after [`MAX_WALL`];
//! the result is then `partial` and the folders it never read are noted `not_scanned`.
//! A walk nobody has polled for [`UNWATCHED`] is abandoned (a closed browser tab never cancels).
//!
//! One walk per console: a second start for the folder being walked joins that walk (another tab,
//! a retried request), and one for another folder is refused with 409 `disk_usage_busy` and the
//! running walk's `id`. [`WORKERS`] is sized so one walk leaves the pollers their slots; two would
//! take all of the helper's.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::fs_ops::{list_dir_with_timeout, DirEntry, ListDirOptions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{console_addr_or_default, json_err, AppState};

/// Lists in flight per walk. The helper takes eight management calls at once, six for
/// everyone; four leaves the status and volume pollers theirs.
pub(crate) const WORKERS: usize = 4;
/// The helper's own ceiling for one `fs.list` page.
const PAGE: u64 = 256;
/// A folder with more pages than this (a million entries) is read no further.
const MAX_PAGES: u64 = 4096;
pub(crate) const MAX_DIRS: u64 = 250_000;
pub(crate) const MAX_WALL: Duration = Duration::from_secs(20 * 60);
/// One page should come back in well under a second; a helper that is wedged must not hold
/// a worker for the default 30 s per call.
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Files kept by name per folder; the rest are summed. The app's treemap shows thirty blocks.
pub(crate) const TOP_FILES: usize = 30;
const UNWATCHED: Duration = Duration::from_secs(60);
/// How long a finished walk's result stays to be fetched.
const KEEP_FINISHED: Duration = Duration::from_secs(10 * 60);
/// Pseudo filesystems, and the per-app sandboxes that mount (nullfs) folders found elsewhere:
/// sizing them would count devices, or count an installed game twice.
pub(crate) const PSEUDO: &[&str] = &["/dev", "/proc", "/mnt/sandbox"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct DuFile {
    pub name: String,
    pub size: u64,
}

/// One folder of the walk. `bytes`, `files` and `dirs` count everything below it.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct DuDir {
    pub name: String,
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Folders at or below this one that were not read (see `note`).
    pub skipped: u64,
    /// Why this folder was not read: `unreadable`, `other_drive`, `system` or `not_scanned`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    /// Largest first.
    pub children: Vec<DuDir>,
    /// The largest files directly inside, largest first; `more_*` sums the others.
    pub top_files: Vec<DuFile>,
    pub more_files: u64,
    pub more_bytes: u64,
}

/// Live counts while a walk runs.
#[derive(Debug, Default)]
pub(crate) struct Progress {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub skipped: AtomicU64,
}

pub(crate) struct Limits {
    pub workers: usize,
    pub max_dirs: u64,
    pub max_wall: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            workers: WORKERS,
            max_dirs: MAX_DIRS,
            max_wall: MAX_WALL,
        }
    }
}

pub(crate) struct Walked {
    pub tree: DuDir,
    /// Why the walk stopped early: `cancelled`, `folder_limit` or `time_limit`.
    pub partial: Option<&'static str>,
}

/// A folder as the walk finds it; turned into a [`DuDir`] once every folder is in.
struct Node {
    name: String,
    path: String,
    note: Option<&'static str>,
    children: Vec<usize>,
    top_files: Vec<DuFile>,
    more_files: u64,
    more_bytes: u64,
}

impl Node {
    fn new(name: String, path: String, note: Option<&'static str>) -> Self {
        Self {
            name,
            path,
            note,
            children: Vec::new(),
            top_files: Vec::new(),
            more_files: 0,
            more_bytes: 0,
        }
    }
}

struct Shared {
    nodes: Vec<Node>,
    queue: VecDeque<usize>,
    in_flight: usize,
    /// Folders handed to a lister so far, the root included.
    listed: u64,
    stop: Option<&'static str>,
}

/// `/a/b/` -> `/a/b`; `/` stays.
pub(crate) fn normalize(path: &str) -> String {
    let t = path.trim_end_matches('/');
    if t.is_empty() {
        "/".to_string()
    } else {
        t.to_string()
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// A listing's entries into folder `idx`: sub-folders queued (or noted, when `skip` says
/// so), files counted. Links, sockets and entries the helper could not stat are left out.
fn absorb(
    sh: &mut Shared,
    idx: usize,
    entries: Vec<DirEntry>,
    skip: &(dyn Fn(&str) -> Option<&'static str> + Sync),
    progress: &Progress,
) {
    let mut files: Vec<DuFile> = Vec::new();
    for e in entries {
        match e.kind.as_str() {
            "dir" => {
                let path = join(&sh.nodes[idx].path, &e.name);
                let note = skip(&path);
                let child = sh.nodes.len();
                sh.nodes.push(Node::new(e.name, path, note));
                sh.nodes[idx].children.push(child);
                if note.is_some() {
                    progress.skipped.fetch_add(1, Ordering::Relaxed);
                } else {
                    sh.queue.push_back(child);
                }
            }
            "file" => {
                progress.files.fetch_add(1, Ordering::Relaxed);
                progress.bytes.fetch_add(e.size, Ordering::Relaxed);
                files.push(DuFile {
                    name: e.name,
                    size: e.size,
                });
            }
            _ => {}
        }
    }
    files.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    let node = &mut sh.nodes[idx];
    for f in files.drain(TOP_FILES.min(files.len())..) {
        node.more_files += 1;
        node.more_bytes += f.size;
    }
    node.top_files = files;
}

/// Walks `root` with `list` (one folder's full listing), `limits.workers` lists at a time.
/// The root's own listing failing is the walk's error; any folder below that cannot be read
/// is noted and skipped.
pub(crate) fn walk(
    root: &str,
    list: &(dyn Fn(&str) -> anyhow::Result<Vec<DirEntry>> + Sync),
    skip: &(dyn Fn(&str) -> Option<&'static str> + Sync),
    limits: &Limits,
    progress: &Progress,
    cancel: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<Walked> {
    let root = normalize(root);
    let started = Instant::now();
    let entries = list(&root)?;
    progress.dirs.fetch_add(1, Ordering::Relaxed);
    let name = root.rsplit('/').next().unwrap_or_default().to_string();
    let mut sh = Shared {
        nodes: vec![Node::new(name, root, None)],
        queue: VecDeque::new(),
        in_flight: 0,
        listed: 1,
        stop: None,
    };
    absorb(&mut sh, 0, entries, skip, progress);
    let shared = Mutex::new(sh);
    let cv = Condvar::new();
    // The walk usually runs on a blocking thread of the engine's runtime; the listers reach
    // the helper through it, so each worker enters it too.
    let rt = tokio::runtime::Handle::try_current().ok();
    std::thread::scope(|s| {
        for _ in 0..limits.workers.max(1) {
            let rt = rt.clone();
            let (shared, cv) = (&shared, &cv);
            s.spawn(move || {
                let _rt = rt.as_ref().map(|h| h.enter());
                worker(shared, cv, list, skip, limits, progress, cancel, started);
            });
        }
    });
    let mut sh = shared.into_inner().unwrap_or_else(|e| e.into_inner());
    // Whatever is still queued was never read.
    while let Some(i) = sh.queue.pop_front() {
        sh.nodes[i].note = Some("not_scanned");
        progress.skipped.fetch_add(1, Ordering::Relaxed);
    }
    Ok(Walked {
        tree: build(sh.nodes),
        partial: sh.stop,
    })
}

#[allow(clippy::too_many_arguments)]
fn worker(
    shared: &Mutex<Shared>,
    cv: &Condvar,
    list: &(dyn Fn(&str) -> anyhow::Result<Vec<DirEntry>> + Sync),
    skip: &(dyn Fn(&str) -> Option<&'static str> + Sync),
    limits: &Limits,
    progress: &Progress,
    cancel: &(dyn Fn() -> bool + Sync),
    started: Instant,
) {
    loop {
        let (idx, path) = {
            let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if g.stop.is_none() {
                    g.stop = if cancel() {
                        Some("cancelled")
                    } else if g.listed >= limits.max_dirs && !g.queue.is_empty() {
                        Some("folder_limit")
                    } else if started.elapsed() >= limits.max_wall && !g.queue.is_empty() {
                        Some("time_limit")
                    } else {
                        None
                    };
                }
                if g.stop.is_some() {
                    cv.notify_all();
                    return;
                }
                if let Some(i) = g.queue.pop_front() {
                    g.in_flight += 1;
                    g.listed += 1;
                    let path = g.nodes[i].path.clone();
                    break (i, path);
                }
                if g.in_flight == 0 {
                    cv.notify_all();
                    return;
                }
                // Timed, so a cancel is seen while the others' lists are out.
                g = cv
                    .wait_timeout(g, Duration::from_millis(200))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        };
        let r = list(&path);
        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
        g.in_flight -= 1;
        match r {
            Ok(entries) => {
                progress.dirs.fetch_add(1, Ordering::Relaxed);
                absorb(&mut g, idx, entries, skip, progress);
            }
            Err(_) => {
                g.nodes[idx].note = Some("unreadable");
                progress.skipped.fetch_add(1, Ordering::Relaxed);
            }
        }
        cv.notify_all();
    }
}

/// The arena into a tree, totals summed bottom-up. A child is always pushed after its
/// parent, so walking the arena backwards meets every child first.
fn build(mut nodes: Vec<Node>) -> DuDir {
    let mut built: Vec<Option<DuDir>> = (0..nodes.len()).map(|_| None).collect();
    for i in (0..nodes.len()).rev() {
        let n = &mut nodes[i];
        let own_bytes: u64 = n.top_files.iter().map(|f| f.size).sum::<u64>() + n.more_bytes;
        let mut d = DuDir {
            name: std::mem::take(&mut n.name),
            bytes: own_bytes,
            files: n.top_files.len() as u64 + n.more_files,
            dirs: 0,
            skipped: u64::from(n.note.is_some()),
            note: n.note,
            children: Vec::with_capacity(n.children.len()),
            top_files: std::mem::take(&mut n.top_files),
            more_files: n.more_files,
            more_bytes: n.more_bytes,
        };
        for &c in &n.children {
            let ch = built[c].take().expect("a child is built before its parent");
            d.bytes += ch.bytes;
            d.files += ch.files;
            d.dirs += 1 + ch.dirs;
            d.skipped += ch.skipped;
            d.children.push(ch);
        }
        d.children
            .sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        built[i] = Some(d);
    }
    built
        .into_iter()
        .next()
        .flatten()
        .expect("the root is always there")
}

/// Every entry of one folder, page by page (`page(offset)` -> entries, more-to-come).
pub(crate) fn list_all(
    mut page: impl FnMut(u64) -> anyhow::Result<(Vec<DirEntry>, bool)>,
) -> anyhow::Result<Vec<DirEntry>> {
    let mut all = Vec::new();
    for _ in 0..MAX_PAGES {
        let (entries, more) = page(all.len() as u64)?;
        let n = entries.len() as u64;
        all.extend(entries);
        if n == 0 || (!more && n < PAGE) {
            break;
        }
    }
    Ok(all)
}

/// What the walk leaves out below `root`: the system trees, and a drive mounted deeper than
/// `root`'s own entries. A drive that IS one of `root`'s entries (`/mnt/ext0` under `/mnt`)
/// is what the person is looking at, so it is sized.
pub(crate) fn skip_rule(
    root: &str,
    mounts: Vec<String>,
) -> impl Fn(&str) -> Option<&'static str> + Sync {
    let root = normalize(root);
    move |p: &str| {
        if PSEUDO.contains(&p) {
            return Some("system");
        }
        if parent_of(p) != root && mounts.iter().any(|m| m == p) {
            return Some("other_drive");
        }
        None
    }
}

// ─── Jobs ───────────────────────────────────────────────────────────────────

enum Outcome {
    Walked(Walked),
    Failed(String),
}

struct Job {
    id: Uuid,
    addr: String,
    path: String,
    started: Instant,
    progress: Progress,
    cancel: AtomicBool,
    last_seen: Mutex<Instant>,
    /// Set once, when the walk ends, with how long it took.
    outcome: Mutex<Option<(Outcome, Duration, Instant)>>,
}

impl Job {
    fn abandoned(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
            || self
                .last_seen
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed()
                >= UNWATCHED
    }

    fn status(&self) -> serde_json::Value {
        *self.last_seen.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        let mut v = serde_json::json!({
            "id": self.id.to_string(),
            "path": self.path,
            "dirs_scanned": self.progress.dirs.load(Ordering::Relaxed),
            "files": self.progress.files.load(Ordering::Relaxed),
            "bytes": self.progress.bytes.load(Ordering::Relaxed),
            "skipped": self.progress.skipped.load(Ordering::Relaxed),
        });
        let g = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
        let (state, elapsed) = match &*g {
            None => ("running", self.started.elapsed()),
            Some((Outcome::Failed(e), took, _)) => {
                v["error"] = e.clone().into();
                ("failed", *took)
            }
            Some((Outcome::Walked(w), took, _)) => {
                if let Some(p) = w.partial {
                    v["partial"] = p.into();
                }
                v["tree"] = serde_json::to_value(&w.tree).unwrap_or_default();
                let state = if w.partial == Some("cancelled") {
                    "cancelled"
                } else {
                    "done"
                };
                (state, *took)
            }
        };
        v["state"] = state.into();
        v["elapsed_ms"] = (elapsed.as_millis() as u64).into();
        v
    }
}

fn jobs() -> &'static Mutex<HashMap<Uuid, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<Uuid, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn find(id: &str) -> Option<Arc<Job>> {
    let id = Uuid::parse_str(id).ok()?;
    jobs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
}

#[derive(Deserialize)]
pub(crate) struct StartReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
}

/// The walk a new one for `addr` has to wait for or join: one still running that nobody has told
/// to stop.
fn running_walk(jobs: &HashMap<Uuid, Arc<Job>>, addr: &str) -> Option<Arc<Job>> {
    jobs.values()
        .find(|j| {
            j.addr == addr
                && !j.abandoned()
                && j.outcome
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_none()
        })
        .cloned()
}

/// POST /api/ps5/disk-usage
pub(crate) async fn start(State(state): State<AppState>, Json(req): Json<StartReq>) -> Response {
    if !req.path.starts_with('/') || req.path.split('/').any(|s| s == "..") {
        return json_err(
            StatusCode::BAD_REQUEST,
            "path must be absolute, without '..'",
        )
        .into_response();
    }
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let job = Arc::new(Job {
        id: Uuid::new_v4(),
        addr: addr.clone(),
        path: normalize(&req.path),
        started: Instant::now(),
        progress: Progress::default(),
        cancel: AtomicBool::new(false),
        last_seen: Mutex::new(Instant::now()),
        outcome: Mutex::new(None),
    });
    {
        let mut g = jobs().lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|_, j| {
            j.outcome
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_none_or(|(_, _, at)| at.elapsed() < KEEP_FINISHED)
        });
        if let Some(running) = running_walk(&g, &addr) {
            if running.path == job.path {
                return Json(running.status()).into_response();
            }
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "disk_usage_busy",
                    "id": running.id.to_string(),
                    "path": running.path,
                })),
            )
                .into_response();
        }
        g.insert(job.id, job.clone());
    }
    let j = job.clone();
    tokio::task::spawn_blocking(move || {
        let root = j.path.clone();
        // Best effort: without the volume list a deeper drive is walked like any folder.
        let mounts: Vec<String> = ps5upload_core::volumes::list_volumes(&addr)
            .map(|l| {
                l.volumes
                    .into_iter()
                    .map(|v| normalize(&v.path))
                    .filter(|p| p.starts_with(&format!("{}/", root.trim_end_matches('/'))))
                    .collect()
            })
            .unwrap_or_default();
        let skip = skip_rule(&root, mounts);
        let list = |p: &str| {
            list_all(|offset| {
                list_dir_with_timeout(
                    &addr,
                    p,
                    ListDirOptions {
                        offset,
                        limit: PAGE,
                    },
                    Some(LIST_TIMEOUT),
                )
                .map(|l| (l.entries, l.truncated))
            })
        };
        let r = walk(
            &root,
            &list,
            &skip,
            &Limits::default(),
            &j.progress,
            &|| j.abandoned(),
        );
        let took = j.started.elapsed();
        let outcome = match r {
            Ok(w) => {
                crate::engine_log::record(
                    "info",
                    format!(
                    "disk usage: {root} on {addr}: {} folder(s), {} file(s), {} bytes, {} skipped in {} ms{}",
                    w.tree.dirs + 1,
                    w.tree.files,
                    w.tree.bytes,
                    w.tree.skipped,
                    took.as_millis(),
                    w.partial.map(|p| format!(" (partial: {p})")).unwrap_or_default(),
                ));
                Outcome::Walked(w)
            }
            Err(e) => Outcome::Failed(format!("{e:#}")),
        };
        *j.outcome.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((outcome, took, Instant::now()));
    });
    Json(job.status()).into_response()
}

/// GET /api/ps5/disk-usage/{id}
pub(crate) async fn status(Path(id): Path<String>) -> Response {
    match find(&id) {
        Some(j) => Json(j.status()).into_response(),
        None => json_err(StatusCode::NOT_FOUND, "no such disk usage walk").into_response(),
    }
}

/// POST /api/ps5/disk-usage/{id}/cancel
pub(crate) async fn cancel(Path(id): Path<String>) -> Response {
    match find(&id) {
        Some(j) => {
            j.cancel.store(true, Ordering::Relaxed);
            Json(j.status()).into_response()
        }
        None => json_err(StatusCode::NOT_FOUND, "no such disk usage walk").into_response(),
    }
}

#[cfg(test)]
mod one_walk_per_console_tests {
    use super::*;

    fn job(addr: &str, path: &str) -> Arc<Job> {
        Arc::new(Job {
            id: Uuid::new_v4(),
            addr: addr.into(),
            path: path.into(),
            started: Instant::now(),
            progress: Progress::default(),
            cancel: AtomicBool::new(false),
            last_seen: Mutex::new(Instant::now()),
            outcome: Mutex::new(None),
        })
    }

    #[test]
    fn only_a_live_walk_on_that_console_holds_the_slot() {
        let live = job("a", "/data");
        let jobs: HashMap<Uuid, Arc<Job>> = [(live.id, live.clone())].into_iter().collect();
        assert_eq!(running_walk(&jobs, "a").map(|j| j.id), Some(live.id));
        assert!(running_walk(&jobs, "b").is_none(), "another console");

        // Told to stop (a new folder picked): the next walk need not wait for its last listing.
        live.cancel.store(true, Ordering::Relaxed);
        assert!(running_walk(&jobs, "a").is_none());
        live.cancel.store(false, Ordering::Relaxed);

        // Finished.
        *live.outcome.lock().unwrap() =
            Some((Outcome::Failed("x".into()), Duration::ZERO, Instant::now()));
        assert!(running_walk(&jobs, "a").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn file(name: &str, size: u64) -> DirEntry {
        DirEntry {
            name: name.into(),
            kind: "file".into(),
            size,
            mtime: 0,
        }
    }

    fn entry(name: &str, kind: &str) -> DirEntry {
        DirEntry {
            name: name.into(),
            kind: kind.into(),
            size: 0,
            mtime: 0,
        }
    }

    /// A console tree as path -> listing; a path that is missing fails to list.
    struct Fake(BTreeMap<String, Vec<DirEntry>>);

    impl Fake {
        fn list(&self, p: &str) -> anyhow::Result<Vec<DirEntry>> {
            self.0
                .get(p)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("fs_list_dir_opendir_errno_13"))
        }
    }

    fn tree() -> Fake {
        let mut m = BTreeMap::new();
        m.insert(
            "/data".to_string(),
            vec![
                entry("games", "dir"),
                entry("locked", "dir"),
                entry("ext", "dir"),
                file("top.bin", 5),
                entry("link", "link"),
                entry("gone", "unknown"),
            ],
        );
        m.insert(
            "/data/games".into(),
            vec![entry("a", "dir"), entry("b", "dir"), file("list.txt", 1)],
        );
        m.insert(
            "/data/games/a".into(),
            vec![file("eboot.bin", 100), entry("sce_sys", "dir")],
        );
        m.insert("/data/games/a/sce_sys".into(), vec![file("param.json", 10)]);
        m.insert("/data/games/b".into(), vec![file("x", 1000)]);
        // /data/locked is missing: it cannot be read.
        m.insert("/data/ext".into(), vec![entry("drive", "dir")]);
        m.insert("/data/ext/drive".into(), vec![file("huge", 1 << 40)]);
        Fake(m)
    }

    fn run(f: &Fake, root: &str, mounts: Vec<String>, limits: &Limits) -> (Walked, Progress) {
        let progress = Progress::default();
        let skip = skip_rule(root, mounts);
        let w = walk(root, &|p| f.list(p), &skip, limits, &progress, &|| false).unwrap();
        (w, progress)
    }

    fn child<'a>(d: &'a DuDir, name: &str) -> &'a DuDir {
        d.children.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn sums_every_folder_below_and_leaves_out_links() {
        let (w, p) = run(&tree(), "/data", vec![], &Limits::default());
        let t = &w.tree;
        assert_eq!(w.partial, None);
        assert_eq!(t.name, "data");
        let games = child(t, "games");
        assert_eq!(games.bytes, 1 + 100 + 10 + 1000);
        assert_eq!(games.files, 4);
        assert_eq!(games.dirs, 3);
        // Largest first.
        assert_eq!(games.children[0].name, "b");
        assert_eq!(child(games, "a").bytes, 110);
        assert_eq!(t.bytes, 5 + 1111 + (1 << 40));
        assert_eq!(
            t.top_files,
            vec![DuFile {
                name: "top.bin".into(),
                size: 5
            }]
        );
        assert_eq!(p.bytes.load(Ordering::Relaxed), t.bytes);
        assert_eq!(p.files.load(Ordering::Relaxed), t.files);
        // The root and every folder that could be read.
        assert_eq!(p.dirs.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn an_unreadable_folder_is_skipped_not_fatal() {
        let (w, p) = run(&tree(), "/data", vec![], &Limits::default());
        let locked = child(&w.tree, "locked");
        assert_eq!(locked.note, Some("unreadable"));
        assert_eq!(locked.bytes, 0);
        assert_eq!(w.tree.skipped, 1);
        assert_eq!(p.skipped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn the_root_failing_is_the_walks_error() {
        let p = Progress::default();
        let skip = skip_rule("/nope", vec![]);
        let r = walk(
            "/nope",
            &|q| tree().list(q),
            &skip,
            &Limits::default(),
            &p,
            &|| false,
        );
        assert!(r.is_err());
    }

    #[test]
    fn a_drive_mounted_deeper_is_not_entered_but_one_at_the_top_is() {
        let (w, _) = run(
            &tree(),
            "/data",
            vec!["/data/ext/drive".into()],
            &Limits::default(),
        );
        let drive = child(child(&w.tree, "ext"), "drive");
        assert_eq!(drive.note, Some("other_drive"));
        assert_eq!(drive.bytes, 0);
        // Sizing /data/ext itself: the drive is one of its entries, so it is sized.
        let (w, _) = run(
            &tree(),
            "/data/ext",
            vec!["/data/ext/drive".into()],
            &Limits::default(),
        );
        assert_eq!(child(&w.tree, "drive").bytes, 1 << 40);
    }

    #[test]
    fn system_trees_are_left_out() {
        let skip = skip_rule("/mnt", vec![]);
        assert_eq!(skip("/mnt/sandbox"), Some("system"));
        assert_eq!(skip("/mnt/ext0"), None);
        let skip = skip_rule("/", vec![]);
        assert_eq!(skip("/dev"), Some("system"));
        assert_eq!(skip("/proc"), Some("system"));
        assert_eq!(skip("/data"), None);
    }

    #[test]
    fn a_folder_limit_marks_the_rest_not_scanned() {
        let limits = Limits {
            workers: 1,
            max_dirs: 2,
            max_wall: MAX_WALL,
        };
        let (w, _) = run(&tree(), "/data", vec![], &limits);
        assert_eq!(w.partial, Some("folder_limit"));
        assert!(w.tree.skipped > 0);
        fn notes(d: &DuDir, out: &mut Vec<&'static str>) {
            out.extend(d.note);
            d.children.iter().for_each(|c| notes(c, out));
        }
        let mut n = Vec::new();
        notes(&w.tree, &mut n);
        assert!(n.contains(&"not_scanned"));
    }

    #[test]
    fn a_cancel_stops_the_walk() {
        let p = Progress::default();
        let skip = skip_rule("/data", vec![]);
        let f = tree();
        let w = walk(
            "/data",
            &|q| f.list(q),
            &skip,
            &Limits::default(),
            &p,
            &|| true,
        )
        .unwrap();
        assert_eq!(w.partial, Some("cancelled"));
        // Only the root was read.
        assert_eq!(p.dirs.load(Ordering::Relaxed), 1);
        assert_eq!(w.tree.bytes, 5);
    }

    #[test]
    fn many_workers_add_up_the_same_as_one() {
        // A wide, deep tree: 40 folders of 30 folders of 3 files.
        let mut m = BTreeMap::new();
        m.insert(
            "/w".to_string(),
            (0..40).map(|i| entry(&format!("d{i}"), "dir")).collect(),
        );
        for i in 0..40 {
            m.insert(
                format!("/w/d{i}"),
                (0..30).map(|j| entry(&format!("e{j}"), "dir")).collect(),
            );
            for j in 0..30 {
                m.insert(
                    format!("/w/d{i}/e{j}"),
                    (0..3).map(|k| file(&format!("f{k}"), k + 1)).collect(),
                );
            }
        }
        let f = Fake(m);
        let one = run(
            &f,
            "/w",
            vec![],
            &Limits {
                workers: 1,
                ..Limits::default()
            },
        )
        .0;
        let many = run(
            &f,
            "/w",
            vec![],
            &Limits {
                workers: 8,
                ..Limits::default()
            },
        )
        .0;
        assert_eq!(one.tree.bytes, 40 * 30 * 6);
        assert_eq!(many.tree.bytes, one.tree.bytes);
        assert_eq!(many.tree.files, 40 * 30 * 3);
        assert_eq!(many.tree.dirs, 40 + 40 * 30);
    }

    #[test]
    fn keeps_the_largest_files_by_name_and_sums_the_rest() {
        let mut m = BTreeMap::new();
        m.insert(
            "/f".to_string(),
            (0..(TOP_FILES as u64 + 5))
                .map(|i| file(&format!("f{i}"), i + 1))
                .collect(),
        );
        let (w, _) = run(&Fake(m), "/f/", vec![], &Limits::default());
        assert_eq!(w.tree.top_files.len(), TOP_FILES);
        assert_eq!(w.tree.top_files[0].size, TOP_FILES as u64 + 5);
        assert_eq!(w.tree.more_files, 5);
        assert_eq!(w.tree.more_bytes, 1 + 2 + 3 + 4 + 5);
        assert_eq!(w.tree.files, TOP_FILES as u64 + 5);
    }

    #[test]
    fn list_all_pages_until_a_short_page() {
        let total = 600u64;
        let mut calls = 0;
        let all = list_all(|off| {
            calls += 1;
            let n = (total - off).min(PAGE);
            Ok((
                (0..n).map(|i| file(&format!("{}", off + i), 1)).collect(),
                false,
            ))
        })
        .unwrap();
        assert_eq!(all.len(), 600);
        assert_eq!(calls, 3);
        // A full last page asks once more and gets nothing.
        let mut calls = 0;
        let all = list_all(|off| {
            calls += 1;
            let n = (512 - off).min(PAGE);
            Ok(((0..n).map(|_| file("x", 1)).collect(), false))
        })
        .unwrap();
        assert_eq!(all.len(), 512);
        assert_eq!(calls, 3);
    }

    #[test]
    fn paths_join_and_normalize() {
        assert_eq!(normalize("/data/"), "/data");
        assert_eq!(normalize("/"), "/");
        assert_eq!(join("/", "data"), "/data");
        assert_eq!(parent_of("/data"), "/");
        assert_eq!(parent_of("/mnt/ext0"), "/mnt");
    }
}
