//! `/api/collection/...`: the Collection over HTTP. Everything the screen does goes through
//! here, so the desktop app, the web UI and scripts see the same thing.
//!
//! One scan at a time, in the background; the index is kept in memory and on disk. An automatic
//! refresh starts a scan every `refresh_secs` while roots are set. It never holds a power
//! assertion, and after the computer slept it starts a fresh interval instead of scanning at
//! once.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Path, Query},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::collection::{scan, store, Library};

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanStatus {
    pub running: bool,
    pub deep: bool,
    pub found: u64,
    pub done: u64,
    pub started_ms: u64,
    pub finished_ms: u64,
    /// Why the last scan stopped, when it failed.
    pub error: Option<String>,
    /// Games and locations the last good scan found.
    pub games: usize,
    pub locations: usize,
    /// While running: `finding` (walking the folders), `reading` (identifying each item),
    /// `naming` (titles and covers), `saving`.
    pub phase: String,
    /// The folder being searched, or the item or game being read.
    pub current: String,
    pub roots_total: u64,
    pub roots_done: u64,
    pub games_total: u64,
    pub games_done: u64,
    /// Folders the last good scan could not read (what it found there before is kept).
    pub unreadable: Vec<String>,
}

struct State {
    library: Mutex<Option<Arc<Library>>>,
    status: Mutex<ScanStatus>,
    cancel: AtomicBool,
    progress: scan::Progress,
    /// Unix ms of the last scan's end (or of a fresh interval after sleep), for auto refresh.
    last_ms: AtomicU64,
    /// Unix ms the index last reached the disk (0: not since this engine started, or it failed).
    saved_ms: AtomicU64,
}

fn state() -> &'static State {
    static S: OnceLock<State> = OnceLock::new();
    S.get_or_init(|| State {
        library: Mutex::new(
            store::dir()
                .and_then(|d| store::load_index_at(&d))
                .map(Arc::new),
        ),
        status: Mutex::new(ScanStatus::default()),
        cancel: AtomicBool::new(false),
        progress: scan::Progress::default(),
        last_ms: AtomicU64::new(0),
        saved_ms: AtomicU64::new(0),
    })
}

/// A scan that found no change does not write the index again (it can be megabytes, and scans run
/// every minute), but at least this often, so its date on disk does not fall far behind.
const SAVE_UNCHANGED_EVERY_MS: u64 = 60 * 60 * 1000;

/// Whether this scan's result needs writing, given what the last one left and when it was saved.
fn index_needs_saving(previous: Option<&Library>, lib: &Library, now: u64, saved: u64) -> bool {
    previous.is_none_or(|p| !p.same_index(lib))
        || now.saturating_sub(saved) >= SAVE_UNCHANGED_EVERY_MS
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// A refusal with a `code` the client turns into the user's language; `error` stays English.
pub(crate) fn err_coded(status: StatusCode, code: &str, msg: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": msg.into(), "code": code })),
    )
        .into_response()
}

pub(crate) fn library() -> Option<Arc<Library>> {
    state()
        .library
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub(crate) fn settings() -> store::Settings {
    store::dir()
        .map(|d| store::load_settings_at(&d))
        .unwrap_or_default()
}

/// Marks a scan as running in `st`, unless one already is. `false` = one was running: the
/// caller joins it rather than starting a second.
fn claim_scan(st: &mut ScanStatus, deep: bool) -> bool {
    if st.running {
        return false;
    }
    *st = ScanStatus {
        running: true,
        deep,
        started_ms: now_ms(),
        games: st.games,
        locations: st.locations,
        unreadable: std::mem::take(&mut st.unreadable),
        ..ScanStatus::default()
    };
    true
}

/// Starts a scan of the configured roots in the background. The Collection is one per
/// computer, so there is one scan: when one is already running (the automatic refresh, or
/// another window), this joins it and returns `Ok`. `Err` only when there is nothing to scan.
pub fn start_scan(deep: bool) -> Result<(), String> {
    let s = state();
    let roots: Vec<String> = settings().roots.clone();
    if roots.is_empty() {
        return Err("add a folder to the Collection first".into());
    }
    // A root on a saved server signs in and reads through the runtime: the scan thread enters
    // it (callers are handlers and the refresh timer, both on it).
    let rt = tokio::runtime::Handle::try_current().ok();
    {
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        if !claim_scan(&mut st, deep) {
            return Ok(());
        }
    }
    s.cancel.store(false, Ordering::Relaxed);
    s.progress.reset();
    std::thread::spawn(move || {
        let _rt = rt.as_ref().map(|h| h.enter());
        let s = state();
        let previous = library();
        let covers = store::covers_dir();
        let started = Instant::now();
        // A panic in the scan is its failure, not a scan that runs forever: `running` is
        // only cleared below, and every later start would join the dead one.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scan::scan(
                &roots,
                previous.as_deref(),
                deep,
                covers.as_deref(),
                true,
                &s.cancel,
                &s.progress,
                rt.as_ref(),
            )
        }))
        .unwrap_or_else(|p| {
            Err(format!(
                "the scan stopped unexpectedly: {}",
                crate::panic_text(&*p)
            ))
        });
        // "Remove new sidecars after each scan": AppleDouble files only, each one verified.
        s.progress
            .phase
            .store(scan::PHASE_SAVING, Ordering::Relaxed);
        if result.is_ok() && settings().sweep_sidecars {
            for root in roots.iter().filter(|r| !crate::remote::path::is_remote(r)) {
                let root = PathBuf::from(root);
                let swept = crate::collection::junk::sweep_sidecars(&root);
                if swept.removed > 0 || !swept.failed.is_empty() {
                    crate::engine_log::record(
                        "info",
                        format!(
                            "collection: removed {} sidecar(s) ({} bytes) under {}; {} failed",
                            swept.removed,
                            swept.freed,
                            root.display(),
                            swept.failed.len()
                        ),
                    );
                }
            }
        }
        // The index is written before the status lock is taken: the status is read by async
        // handlers, and a save of a large index to a slow disk would hold each of them (and
        // the worker it runs on) for as long as the write takes. `running` stays true until
        // the lock below, so no second scan starts meanwhile.
        if let Ok(lib) = &result {
            let saved = s.saved_ms.load(Ordering::Relaxed);
            if let Some(dir) = store::dir()
                .filter(|_| index_needs_saving(previous.as_deref(), lib, now_ms(), saved))
            {
                match store::save_index_at(&dir, lib) {
                    Ok(()) => s.saved_ms.store(now_ms(), Ordering::Relaxed),
                    Err(e) => {
                        s.saved_ms.store(0, Ordering::Relaxed);
                        crate::engine_log::record(
                            "warn",
                            format!("collection: index not saved: {e}"),
                        );
                    }
                }
            }
        }
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        st.running = false;
        st.finished_ms = now_ms();
        st.found = s.progress.found.load(Ordering::Relaxed);
        st.done = s.progress.done.load(Ordering::Relaxed);
        match result {
            Ok(lib) => {
                st.games = lib.summary.total_games;
                st.locations = lib.summary.total_locations;
                st.unreadable = lib.unreadable.clone();
                st.error = None;
                crate::engine_log::record(
                    "info",
                    format!(
                        "collection: scan finished in {:.1}s: {} games / {} locations",
                        started.elapsed().as_secs_f64(),
                        lib.summary.total_games,
                        lib.summary.total_locations
                    ),
                );
                *s.library.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(lib));
            }
            Err(e) => {
                crate::engine_log::record("warn", format!("collection: scan stopped: {e}"));
                st.error = Some(e);
            }
        }
        s.last_ms.store(now_ms(), Ordering::Relaxed);
    });
    Ok(())
}

/// The automatic refresh: checks every 15 s whether an interval has passed since the last scan.
/// A tick that arrives far later than 15 s means the computer slept: the interval starts over.
pub fn spawn_auto_refresh() {
    const TICK: Duration = Duration::from_secs(15);
    tokio::spawn(async move {
        let mut last_tick = Instant::now();
        state().last_ms.store(now_ms(), Ordering::Relaxed);
        loop {
            tokio::time::sleep(TICK).await;
            let gap = last_tick.elapsed();
            last_tick = Instant::now();
            if gap > TICK * 4 {
                state().last_ms.store(now_ms(), Ordering::Relaxed);
                continue;
            }
            let s = settings();
            let Some(every) = s.refresh_secs else {
                continue;
            };
            if s.roots.is_empty() {
                continue;
            }
            let since = now_ms().saturating_sub(state().last_ms.load(Ordering::Relaxed));
            if since >= every * 1000 {
                let _ = tokio::task::spawn_blocking(|| start_scan(false)).await;
                state().last_ms.store(now_ms(), Ordering::Relaxed);
            }
        }
    });
}

// ── Handlers ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SettingsReply {
    #[serde(flatten)]
    settings: store::Settings,
    refresh_choices: &'static [u64],
    /// Whether this engine can move files to a trash (false in Docker or on a headless server,
    /// where `allow_permanent_delete` decides).
    trash_available: bool,
}

pub async fn get_settings() -> Response {
    Json(SettingsReply {
        settings: settings(),
        refresh_choices: store::REFRESH_CHOICES,
        trash_available: crate::collection_tidy_api::trash_available(),
    })
    .into_response()
}

pub async fn put_settings(Json(s): Json<store::Settings>) -> Response {
    let s = match s.validated() {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let before = settings().roots;
    for r in &s.roots {
        // Only a folder being added is checked. One already in the list may have been deleted or
        // be on an unplugged drive; checking it again made every save fail, removing that very
        // folder included (#461). The scan skips it and says so instead.
        if before.contains(r) {
            continue;
        }
        if crate::remote::path::is_remote(r) {
            let (root, rt) = (r.clone(), tokio::runtime::Handle::current());
            let checked = tokio::task::spawn_blocking(move || {
                scan::Fsys::for_root(&root, Some(&rt)).map(|_| ())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            if let Err(e) = checked {
                return err(StatusCode::BAD_REQUEST, e);
            }
        } else {
            // A stat of a folder on a sleeping disk or a dead network mount can take long:
            // not on an async worker.
            let root = PathBuf::from(r);
            let is_dir = tokio::task::spawn_blocking(move || root.is_dir())
                .await
                .unwrap_or(false);
            if !is_dir {
                return err(
                    StatusCode::BAD_REQUEST,
                    format!("{r} is not a folder the engine can read"),
                );
            }
        }
    }
    let Some(dir) = store::dir() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no data folder for the engine",
        );
    };
    if let Err(e) = store::save_settings_at(&dir, &s) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    get_settings().await
}

pub async fn get_library() -> Response {
    match library() {
        Some(lib) => Json(store::public_json(&lib)).into_response(),
        None => Json(
            serde_json::json!({ "games": {}, "summary": crate::collection::Summary::default() }),
        )
        .into_response(),
    }
}

pub async fn get_game(Path(id): Path<String>) -> Response {
    let id = id.to_ascii_uppercase();
    match library().and_then(|l| l.games.get(&id).cloned()) {
        Some(g) => Json(g).into_response(),
        None => err(
            StatusCode::NOT_FOUND,
            format!("{id} is not in the collection"),
        ),
    }
}

#[derive(Deserialize)]
pub struct CoverQuery {
    /// A grid's size (128, 256 or 384): the cover scaled to it, as for the console's icons.
    /// Anything else, or none, is the full cover.
    size: Option<u32>,
}

/// `cover`'s thumbnail at `px`, from its own file beside the covers while that is not older than
/// the cover: a thumbnail is made once. None (use the full cover) when the cover is that small
/// already, or does not decode.
fn cover_thumbnail(cover: &std::path::Path, px: u32) -> Option<Vec<u8>> {
    let thumbs = cover.parent()?.join("thumbs");
    let name = cover.file_name()?.to_string_lossy().into_owned();
    let thumb = thumbs.join(format!("{name}@{px}"));
    let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let cover_at = modified(cover)?;
    if modified(&thumb).is_some_and(|at| at >= cover_at) {
        if let Ok(bytes) = std::fs::read(&thumb) {
            return Some(bytes);
        }
    }
    let small = crate::icon_thumbnail(&std::fs::read(cover).ok()?, px)?;
    // A cache: not being able to keep it only costs making it again.
    if std::fs::create_dir_all(&thumbs).is_ok() {
        let tmp = thumbs.join(format!("{name}@{px}.tmp"));
        if std::fs::write(&tmp, &small).is_ok() {
            let _ = std::fs::rename(&tmp, &thumb);
        }
    }
    Some(small)
}

/// What the bytes are: a thumbnail is a JPEG unless the cover had transparency.
fn cover_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xff, 0xd8]) {
        "image/jpeg"
    } else {
        "image/png"
    }
}

pub async fn get_cover(Path(id): Path<String>, Query(q): Query<CoverQuery>) -> Response {
    let id = id.to_ascii_uppercase();
    let file = library()
        .and_then(|l| l.games.get(&id).and_then(|g| g.local_cover.clone()))
        .filter(|name| crate::collection::is_bare_file_name(name))
        .and_then(|name| store::covers_dir().map(|d| d.join(name)));
    let px = crate::icon_thumb_px(q.size);
    let bytes = match file {
        Some(f) => tokio::task::spawn_blocking(move || {
            px.and_then(|px| cover_thumbnail(&f, px))
                .or_else(|| std::fs::read(&f).ok())
        })
        .await
        .ok()
        .flatten(),
        None => None,
    };
    match bytes {
        Some(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, cover_mime(&bytes))
            .header(header::CACHE_CONTROL, "max-age=300")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "cover")),
        None => err(StatusCode::NOT_FOUND, "no cover"),
    }
}

#[derive(Deserialize)]
pub struct ScanReq {
    #[serde(default)]
    deep: bool,
}

pub async fn post_scan(Json(req): Json<ScanReq>) -> Response {
    match tokio::task::spawn_blocking(move || start_scan(req.deep)).await {
        Ok(Ok(())) => (StatusCode::ACCEPTED, get_scan_status()).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn get_scan_status() -> Json<ScanStatus> {
    let s = state();
    let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if st.running {
        let p = &s.progress;
        st.found = p.found.load(Ordering::Relaxed);
        st.done = p.done.load(Ordering::Relaxed);
        st.phase = p.phase_name().to_string();
        st.current = p.current();
        st.roots_total = p.roots_total.load(Ordering::Relaxed);
        st.roots_done = p.roots_done.load(Ordering::Relaxed);
        st.games_total = p.games_total.load(Ordering::Relaxed);
        st.games_done = p.games_done.load(Ordering::Relaxed);
    }
    Json(st)
}

pub async fn get_scan() -> Response {
    get_scan_status().into_response()
}

pub async fn post_scan_cancel() -> Response {
    state().cancel.store(true, Ordering::Relaxed);
    get_scan_status().into_response()
}

#[derive(Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

pub async fn get_export(Query(q): Query<ExportQuery>) -> Response {
    let lib = library().map(|l| (*l).clone()).unwrap_or_default();
    let (body, ctype, ext) = match q.format.as_deref().unwrap_or("json") {
        "csv" => (store::to_csv(&lib), "text/csv; charset=utf-8", "csv"),
        "md" => (
            store::to_markdown(&lib, &crate::collection::iso_utc((now_ms() / 1000) as i64)),
            "text/markdown; charset=utf-8",
            "md",
        ),
        "json" => (
            serde_json::to_string_pretty(&store::public_json(&lib)).unwrap_or_default(),
            "application/json",
            "json",
        ),
        other => return err(StatusCode::BAD_REQUEST, format!("unknown format {other}")),
    };
    Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"game-collection.{ext}\""),
        )
        .body(Body::from(body))
        .unwrap_or_else(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "export"))
}

// ── The console overlay ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ConsoleQuery {
    #[serde(alias = "console", alias = "host")]
    addr: String,
}

/// What a console has installed, by the rule the installed list (`/api/ps5/apps/installed`) also
/// uses: upper-case title ID → the path it was registered from, if any.
pub(crate) struct ConsoleInstalled {
    pub(crate) titles: std::collections::HashMap<String, Option<String>>,
    /// The read was whole: a title not in `titles` is known not to be installed. When false
    /// (app.db or an app folder could not be read in full), a title missing from `titles` is
    /// unknown, not "not installed".
    pub(crate) complete: bool,
}

/// The console's installed titles. A title counts when the console records it: a
/// `/user/appmeta/<id>` folder, or an app.db row whose app folder exists on the internal or an
/// extended drive (an M.2 title has its folder there). An app folder alone is not an installed
/// game: an uninstall can leave `/user/app/<id>` behind, and the helper's registered list holds
/// every folder there. Games registered from a folder or an image are in app.db and appmeta.
pub(crate) fn installed_titles(addr: &str) -> Result<ConsoleInstalled, String> {
    let registered = ps5upload_core::fs_ops::app_list_registered(addr)
        .map_err(|e| format!("the console's titles could not be read: {e:#}"))?;
    let (appmeta, appmeta_whole) = crate::list_dir_whole(addr, "/user/appmeta")
        .map_err(|e| format!("the console's titles could not be read: {e:#}"))?;
    let mut app_dirs = std::collections::HashSet::new();
    let mut folders_whole = true;
    for root in crate::APP_FOLDER_ROOTS {
        // An absent drive is no folders; one that could not be read leaves the answer open.
        match console_names_all(addr, root) {
            Ok(names) => app_dirs.extend(names),
            Err(_) => folders_whole = false,
        }
    }
    // app.db is asked only when there are app folders to confirm its rows against.
    let appdb =
        (!app_dirs.is_empty()).then(|| match ps5upload_core::diagnostics::appdb_query(addr) {
            Ok(list) => {
                let whole = !list.truncated && list.err.is_none();
                let rows = list.apps.into_iter().map(|a| (a.title_id, a.name));
                Ok((rows.collect(), whole))
            }
            Err(e) => {
                crate::log_warn!("collection: app.db query failed: {e:#}");
                Err(())
            }
        });
    let read = InstalledRead {
        appmeta: appmeta.into_iter().map(|e| e.name).collect(),
        appmeta_whole,
        app_dirs,
        folders_whole,
        appdb,
        registered: (registered.apps.into_iter())
            .map(|a| (a.title_id, a.src))
            .collect(),
    };
    Ok(installed_from(read, |id| {
        crate::app_folder_has_param(addr, id)
    }))
}

/// app.db's (title ID, name) rows, and whether they are all of them.
type AppDbRows = (Vec<(String, String)>, bool);

/// What one installed-titles read found on the console.
struct InstalledRead {
    /// `/user/appmeta` entry names, and whether every page was read.
    appmeta: Vec<String>,
    appmeta_whole: bool,
    /// App folder names on every drive, and whether every drive's listing was read.
    app_dirs: std::collections::HashSet<String>,
    folders_whole: bool,
    /// app.db's (title ID, name) rows and whether they are all of them; `Err` when it could not
    /// be read; None when it was not asked (no app folders).
    appdb: Option<Result<AppDbRows, ()>>,
    /// The helper's list: every app folder, with the source our mount.lnk names (empty if none).
    registered: Vec<(String, String)>,
}

/// The installed rule, without the console (see [`installed_titles`]). `has_param` tells
/// whether an app folder holds a title, for a folder app.db could not be read for.
fn installed_from(read: InstalledRead, has_param: impl Fn(&str) -> bool) -> ConsoleInstalled {
    let mut ids: Vec<String> = (read.appmeta.into_iter())
        .filter(|id| crate::looks_like_title_id(id) && !crate::is_console_webui_tile(id))
        .collect();
    let mut complete = read.appmeta_whole && read.folders_whole && !read.app_dirs.is_empty();
    if let Some(appdb) = read.appdb {
        let appdb_whole = matches!(appdb, Ok((_, true)));
        complete &= appdb_whole;
        if let Ok((rows, _)) = appdb {
            let seen: std::collections::HashSet<String> = ids.iter().cloned().collect();
            let added = crate::appdb_installed_additions(&rows, &read.app_dirs, &seen);
            ids.extend(added.into_iter().map(|(id, _)| id));
        }
        // app.db not read in full: a registered title it did not get to still counts when its
        // folder holds a title (our mount.lnk source, or a param file), never a bare folder.
        let seen: std::collections::HashSet<String> = ids.iter().cloned().collect();
        for id in crate::registered_only_additions(
            read.registered.iter().map(|(id, _)| id.as_str()),
            &read.app_dirs,
            &seen,
            appdb_whole,
        ) {
            let has_src =
                (read.registered.iter()).any(|(r, src)| r.trim() == id && !src.trim().is_empty());
            if has_src || has_param(&id) {
                ids.push(id);
            }
        }
    }
    let sources: std::collections::HashMap<String, String> = (read.registered.into_iter())
        .filter(|(_, src)| !src.is_empty())
        .map(|(id, src)| (id.trim().to_ascii_uppercase(), src))
        .collect();
    let titles = (ids.into_iter())
        .map(|id| id.to_ascii_uppercase())
        .map(|id| {
            let from = sources.get(&id).cloned();
            (id, from)
        })
        .collect();
    ConsoleInstalled { titles, complete }
}

/// The console's update and DLC folders, listed once per drive and shared by every title of a
/// read: one listing of `<root>/user/patch` and `<root>/user/addcont` instead of two per title.
/// A listing that failed (not one that is absent) is kept as an error, so the titles it would
/// have answered for are marked unread rather than "no update, no DLC".
pub(crate) struct ConsoleExtras {
    /// Per drive root: the title IDs (upper case) with an update folder.
    patch: Vec<Result<std::collections::HashSet<String>, String>>,
    /// Per drive root: the root and the title IDs (upper case, as listed) with a DLC folder.
    addcont: Vec<(String, Result<Vec<String>, String>)>,
}

impl ConsoleExtras {
    pub(crate) fn read(addr: &str, roots: &[String]) -> Self {
        Self::from_lister(roots, |dir| console_names_all(addr, dir))
    }

    fn from_lister(
        roots: &[String],
        mut list: impl FnMut(&str) -> Result<Vec<String>, String>,
    ) -> Self {
        let mut patch = Vec::with_capacity(roots.len());
        let mut addcont = Vec::with_capacity(roots.len());
        for root in roots {
            patch.push(
                list(&format!("{root}/user/patch"))
                    .map(|names| names.into_iter().map(|n| n.to_ascii_uppercase()).collect()),
            );
            addcont.push((root.clone(), list(&format!("{root}/user/addcont"))));
        }
        Self { patch, addcont }
    }

    /// `(update installed, DLC labels)` of one title, or `None` when a listing it depends on
    /// could not be read. `list` reads a title's DLC folder (only for titles that have one).
    fn of(
        &self,
        id: &str,
        mut list: impl FnMut(&str) -> Result<Vec<String>, String>,
    ) -> Option<(bool, Vec<String>)> {
        let mut patch = false;
        for set in &self.patch {
            patch |= set.as_ref().ok()?.contains(&id.to_ascii_uppercase());
        }
        let mut labels = Vec::new();
        for (root, listed) in &self.addcont {
            for folder in listed.as_ref().ok()? {
                if folder.eq_ignore_ascii_case(id) {
                    labels.extend(list(&format!("{root}/user/addcont/{folder}")).ok()?);
                }
            }
        }
        Some((patch, labels))
    }
}

/// What the console has of one title: installed, its version, an update, its DLC. One version
/// query, plus a DLC folder listing for a title that has DLC; the drives' update and DLC
/// folders come from `extras`, listed once per read.
pub(crate) fn read_title(
    addr: &str,
    id: &str,
    extras: &ConsoleExtras,
    installed: &ConsoleInstalled,
) -> crate::collection::console::ConsoleTitle {
    let mut t = crate::collection::console::ConsoleTitle {
        // Not listed by a read that was not whole: unknown, so the last answer stands.
        installed_unread: !installed.complete && !installed.titles.contains_key(id),
        ..Default::default()
    };
    if let Some(from) = installed.titles.get(id) {
        t.installed = true;
        t.registered_from = from.clone();
        match installed_version(addr, id) {
            Ok(v) => t.version = v,
            Err(_) => t.version_unread = true,
        }
        match extras.of(id, |dir| console_names_all(addr, dir)) {
            Some((patch, labels)) => {
                t.patch_installed = patch;
                t.dlc_labels = labels;
            }
            None => t.extras_unread = true,
        }
    }
    t
}

/// The installed version, normalized: `CONTENT_VERSION` (PS5), else the higher of `VERSION` and
/// `APP_VER` (PS4: an update raises `VERSION` and leaves `APP_VER` at the base's, #934).
/// `Ok(None)` when the console has none; `Err` when it could not be asked. Any helper answers
/// these keys, so this needs no helper update.
fn installed_version(addr: &str, title_id: &str) -> Result<Option<String>, String> {
    use ps5upload_core::diagnostics::{appinfo_query, APPINFO_VERSION_KEYS};
    let rows =
        appinfo_query(addr, title_id, Some(APPINFO_VERSION_KEYS)).map_err(|e| format!("{e:#}"))?;
    Ok(rows
        .installed_version()
        .map(|v| ps5upload_pkg::kind::normalize_version(&v)))
}

/// Every entry of a console folder, page by page; empty when it does not exist, `Err` when it
/// could not be read (a busy or gone console must not read as "nothing there").
fn console_names_all(addr: &str, dir: &str) -> Result<Vec<String>, String> {
    use ps5upload_core::fs_ops::{is_not_found, list_dir, ListDirOptions};
    let mut out = Vec::new();
    let mut offset = 0u64;
    // 64 pages of 256: far more updates or DLC than one drive holds.
    for _ in 0..64 {
        let page = match list_dir(addr, dir, ListDirOptions { offset, limit: 256 }) {
            Ok(page) => page,
            Err(e) => {
                let msg = format!("{e:#}");
                return if is_not_found(&msg) {
                    Ok(out)
                } else {
                    Err(msg)
                };
            }
        };
        let n = page.entries.len() as u64;
        out.extend(
            page.entries
                .into_iter()
                .map(|e| e.name)
                .filter(|n| n != "." && n != ".."),
        );
        if !page.truncated || n == 0 {
            return Ok(out);
        }
        offset += n;
    }
    Ok(out)
}

/// What one console has, for every game in the collection: installed or not, at which
/// version, and what the collection could bring it. The console is read once for its
/// registered titles; installed ones are then asked for their update, DLC and version.
pub async fn get_console(Query(q): Query<ConsoleQuery>) -> Response {
    let Some(lib) = library() else {
        return Json(serde_json::json!({ "games": [] })).into_response();
    };
    let addr = q.addr;
    let result = tokio::task::spawn_blocking(
        move || -> Result<Vec<crate::collection::console::GameConsoleState>, String> {
            use crate::collection::console::state_for;
            let installed = installed_titles(&addr)?;
            let roots = crate::pkg_install::installed_storage_roots(&addr);
            let extras = ConsoleExtras::read(&addr, &roots);
            let games: Vec<&crate::collection::Game> = lib.games.values().collect();
            let mut out = Vec::with_capacity(games.len());
            let mut read = Vec::with_capacity(games.len());
            // A few titles at a time: each is a handful of small reads.
            for chunk in games.chunks(6) {
                let states: Vec<_> = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|g| {
                            let (addr, extras, installed) = (&addr, &extras, &installed);
                            scope
                                .spawn(move || (read_title(addr, &g.game_id, extras, installed), g))
                        })
                        .collect();
                    handles.into_iter().filter_map(|h| h.join().ok()).collect()
                });
                read.extend(states);
            }
            // Kept for the game page, which shows consoles other than the connected one. A
            // detail that could not be read keeps what the last read found, and the state is
            // worked out from the merged facts, so a busy console does not turn into "no
            // update, no DLC".
            let now = crate::console_snapshot::now_unix();
            crate::console_snapshot::with(|snaps| {
                for (t, g) in &read {
                    let facts = crate::console_snapshot::merge_detailed(
                        snaps, &addr, &g.game_id, t, &g.title, now,
                    );
                    out.push(state_for(g, &facts.console_title()));
                }
            });
            Ok(out)
        },
    )
    .await;
    match result {
        Ok(Ok(games)) => Json(serde_json::json!({ "games": games })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod installed_titles_tests {
    use super::{installed_from, read_title, ConsoleExtras, ConsoleInstalled, InstalledRead};
    use crate::collection::console::ConsoleTitle;
    use crate::console_snapshot::{merge_detailed, Snapshots};

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A console with these appmeta folders, app folders (every one in the helper's list, as it
    /// lists every app folder) and app.db rows; `sources` are our mount.lnk sources.
    fn read(
        appmeta: &[&str],
        folders: &[&str],
        appdb: Option<Result<(&[&str], bool), ()>>,
        sources: &[(&str, &str)],
    ) -> InstalledRead {
        InstalledRead {
            appmeta: strings(appmeta),
            appmeta_whole: true,
            app_dirs: strings(folders).into_iter().collect(),
            folders_whole: true,
            appdb: appdb.map(|r| {
                r.map(|(rows, whole)| {
                    let rows = rows.iter().map(|id| (id.to_string(), String::new()));
                    (rows.collect(), whole)
                })
            }),
            registered: folders
                .iter()
                .map(|id| {
                    let src = sources.iter().find(|(s, _)| s == id).map(|(_, p)| *p);
                    (id.to_string(), src.unwrap_or_default().to_string())
                })
                .collect(),
        }
    }

    fn no_param(_: &str) -> bool {
        false
    }

    /// The game page's answer for a title the read does not list (no console call is made).
    fn page_answer(installed: &ConsoleInstalled, id: &str) -> ConsoleTitle {
        let extras = ConsoleExtras::from_lister(&[], |_| Ok(Vec::new()));
        read_title("", id, &extras, installed)
    }

    #[test]
    fn a_title_app_db_lists_is_installed_ps5_or_ps4() {
        let r = read(
            &[],
            &["PPSA00001", "CUSA00002", "PPSA00009"],
            Some(Ok((&["PPSA00001", "CUSA00002", "PPSA00003"], true))),
            &[],
        );
        let m = installed_from(r, no_param);
        assert!(m.complete);
        assert!(m.titles.contains_key("PPSA00001"));
        assert!(m.titles.contains_key("CUSA00002"));
        // A row with no app folder (a preinstalled tile never downloaded) is not installed.
        assert!(!m.titles.contains_key("PPSA00003"));
    }

    #[test]
    fn an_app_folder_alone_is_not_an_installed_game() {
        // An uninstall left PPSA00009's folder behind; app.db, read whole, does not list it.
        let r = read(
            &["PPSA00001", "not-a-title"],
            // Nine bytes, a multi-byte character inside: skipped, never sliced mid-char.
            &["PPSA00001", "PPSA00009", "PPP\u{e9}0001"],
            Some(Ok((&["PPSA00001"], true))),
            &[],
        );
        let m = installed_from(r, |_| true);
        assert!(m.complete);
        assert_eq!(m.titles.len(), 1);
        assert!(!m.titles.contains_key("PPSA00009"));
        let t = page_answer(&m, "PPSA00009");
        assert!(!t.installed && !t.installed_unread);
    }

    #[test]
    fn an_unreadable_app_db_leaves_unlisted_titles_unknown_not_uninstalled() {
        for appdb in [Err(()), Ok((&["PPSA00001"][..], false))] {
            let r = read(
                &[],
                &["PPSA00001", "PPSA00002", "PPSA00005"],
                Some(appdb),
                &[("PPSA00002", "/data/games/b")],
            );
            let m = installed_from(r, no_param);
            assert!(!m.complete);
            // Our registered folder game still counts; a bare folder does not.
            assert_eq!(m.titles["PPSA00002"].as_deref(), Some("/data/games/b"));
            assert!(!m.titles.contains_key("PPSA00005"));
            let t = page_answer(&m, "PPSA00005");
            assert!(t.installed_unread);
            // The last read said installed: that stands.
            let mut snaps = Snapshots::new();
            let was = ConsoleTitle {
                installed: true,
                version: Some("01.000.000".into()),
                ..ConsoleTitle::default()
            };
            merge_detailed(&mut snaps, "10.0.0.2", "PPSA00005", &was, "", 1);
            let facts = merge_detailed(&mut snaps, "10.0.0.2", "PPSA00005", &t, "", 2);
            assert!(facts.installed);
            assert_eq!(facts.version.as_deref(), Some("01.000.000"));
        }
    }

    #[test]
    fn a_failed_folder_listing_makes_the_read_not_whole() {
        let mut r = read(&["PPSA00001"], &["PPSA00001"], None, &[]);
        r.folders_whole = false;
        assert!(!installed_from(r, no_param).complete);
    }

    #[test]
    fn a_game_registered_from_a_folder_or_image_reads_as_present() {
        let r = read(
            &["PPSA00002"],
            &["PPSA00002", "CUSA00004"],
            Some(Ok((&["PPSA00002", "CUSA00004"], true))),
            &[("PPSA00002", "/data/games/b"), ("CUSA00004", "/mnt/img/c")],
        );
        let m = installed_from(r, no_param);
        assert!(m.complete);
        assert_eq!(m.titles["PPSA00002"].as_deref(), Some("/data/games/b"));
        assert_eq!(m.titles["CUSA00004"].as_deref(), Some("/mnt/img/c"));
    }
}

#[cfg(test)]
mod extras_tests {
    use super::ConsoleExtras;
    use crate::collection::console::ConsoleTitle;
    use crate::console_snapshot::{merge_detailed, Snapshots};

    fn listing(dir: &str) -> Result<Vec<String>, String> {
        Ok(match dir {
            "/r1/user/patch" => vec!["PPSA00001".into()],
            "/r1/user/addcont" => vec!["PPSA00001".into(), "PPSA00002".into()],
            "/r1/user/addcont/PPSA00001" => vec!["DLC1".into(), "DLC2".into()],
            "/r1/user/addcont/PPSA00002" => vec!["X".into()],
            _ => vec![],
        })
    }

    /// The drives' update and DLC folders are listed once per read, not twice per title.
    #[test]
    fn update_and_dlc_folders_are_listed_once_for_every_title() {
        let roots = vec!["/r1".to_string(), "/r2".to_string()];
        let mut calls = Vec::new();
        let extras = ConsoleExtras::from_lister(&roots, |d| {
            calls.push(d.to_string());
            listing(d)
        });
        assert_eq!(calls.len(), 4, "{calls:?}");
        let mut per_title = 0;
        let mut of = |id: &str| {
            extras.of(id, |d| {
                per_title += 1;
                listing(d)
            })
        };
        assert_eq!(
            of("ppsa00001"),
            Some((true, vec!["DLC1".to_string(), "DLC2".to_string()]))
        );
        assert_eq!(of("PPSA00003"), Some((false, vec![])));
        // Only a title with DLC gets its own listing.
        assert_eq!(per_title, 1);
    }

    /// A listing that failed makes the title's update and DLC unknown, not absent.
    #[test]
    fn a_failed_listing_is_unknown_not_none() {
        let roots = vec!["/r1".to_string()];
        let extras = ConsoleExtras::from_lister(&roots, |d| {
            if d == "/r1/user/addcont" {
                Err("timed out".into())
            } else {
                listing(d)
            }
        });
        assert_eq!(extras.of("PPSA00001", listing), None);
        let extras = ConsoleExtras::from_lister(&roots, listing);
        assert_eq!(extras.of("PPSA00001", |_| Err("busy".into())), None);
    }

    /// What an earlier read found is kept when this read could not see it.
    #[test]
    fn an_unread_detail_keeps_the_recorded_one() {
        let mut s = Snapshots::new();
        let full = ConsoleTitle {
            installed: true,
            version: Some("01.020.000".into()),
            patch_installed: true,
            dlc_labels: vec!["DLC1".into()],
            ..ConsoleTitle::default()
        };
        merge_detailed(&mut s, "10.0.0.5:1", "PPSA00001", &full, "Game", 1);
        let unread = ConsoleTitle {
            installed: true,
            version_unread: true,
            extras_unread: true,
            ..ConsoleTitle::default()
        };
        let facts = merge_detailed(&mut s, "10.0.0.5", "PPSA00001", &unread, "Game", 2);
        assert_eq!(facts.version.as_deref(), Some("01.020.000"));
        assert_eq!(facts.patch_installed, Some(true));
        assert_eq!(facts.dlc_labels.as_deref(), Some(&["DLC1".to_string()][..]));
        assert_eq!(facts.read_at, 2);
        // A read that did see them replaces them.
        let read = ConsoleTitle {
            installed: true,
            ..ConsoleTitle::default()
        };
        let facts = merge_detailed(&mut s, "10.0.0.5", "PPSA00001", &read, "Game", 3);
        assert_eq!(facts.patch_installed, Some(false));
        assert_eq!(facts.version, None);
    }
}

#[cfg(test)]
mod scan_claim_tests {
    use super::*;

    #[test]
    fn an_unchanged_scan_writes_nothing_until_the_date_is_due() {
        let old = Library {
            generated_at: "t1".into(),
            ..Library::default()
        };
        let same = Library {
            generated_at: "t2".into(),
            ..Library::default()
        };
        let other = Library {
            generated_at: "t2".into(),
            unreadable: vec!["/x".into()],
            ..Library::default()
        };
        let now = SAVE_UNCHANGED_EVERY_MS * 3;
        assert!(!index_needs_saving(Some(&old), &same, now, now - 1000));
        assert!(index_needs_saving(Some(&old), &other, now, now - 1000));
        assert!(index_needs_saving(None, &same, now, now - 1000));
        assert!(index_needs_saving(
            Some(&old),
            &same,
            now,
            now - SAVE_UNCHANGED_EVERY_MS
        ));
        // Never saved by this engine, or the last save failed: it is due.
        assert!(index_needs_saving(Some(&old), &same, now, 0));
    }

    #[test]
    fn a_second_scan_joins_the_running_one() {
        let mut st = ScanStatus::default();
        assert!(claim_scan(&mut st, false));
        let started = st.started_ms;
        assert!(!claim_scan(&mut st, true));
        assert!(st.running);
        assert!(!st.deep, "the running scan's settings stay");
        assert_eq!(st.started_ms, started);
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    fn png(side: u32, shade: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(side, side, image::Rgb([shade, shade, shade]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn a_thumbnail_is_made_once_and_again_when_the_cover_changes() {
        let dir =
            crate::test_tmp::temp_path(format!("ps5upload-cover-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cover = dir.join("PPSA01234.png");
        std::fs::write(&cover, png(512, 10)).unwrap();
        let first = cover_thumbnail(&cover, 128).expect("thumbnail");
        assert_eq!(cover_mime(&first), "image/jpeg");
        let kept = dir.join("thumbs").join("PPSA01234.png@128");
        assert_eq!(std::fs::read(&kept).unwrap(), first);
        // From the file: what is on disk is what comes back.
        std::fs::write(&kept, b"marker").unwrap();
        assert_eq!(cover_thumbnail(&cover, 128).unwrap(), b"marker");
        // A newer cover makes a new one.
        let old = std::time::SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&kept)
            .unwrap()
            .set_modified(old)
            .unwrap();
        std::fs::write(&cover, png(512, 200)).unwrap();
        assert_ne!(cover_thumbnail(&cover, 128).unwrap(), b"marker");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_small_cover_has_no_thumbnail() {
        let dir =
            crate::test_tmp::temp_path(format!("ps5upload-cover-small-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cover = dir.join("PPSA00001.png");
        std::fs::write(&cover, png(64, 1)).unwrap();
        assert!(cover_thumbnail(&cover, 128).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
