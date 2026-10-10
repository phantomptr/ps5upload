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
}

struct State {
    library: Mutex<Option<Arc<Library>>>,
    status: Mutex<ScanStatus>,
    cancel: AtomicBool,
    progress: scan::Progress,
    /// Unix ms of the last scan's end (or of a fresh interval after sleep), for auto refresh.
    last_ms: AtomicU64,
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
    })
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

/// Starts a scan of the configured roots in the background. `Err` when one is running or there
/// is nothing to scan.
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
        if st.running {
            return Err("a scan is already running".into());
        }
        *st = ScanStatus {
            running: true,
            deep,
            started_ms: now_ms(),
            games: st.games,
            locations: st.locations,
            ..ScanStatus::default()
        };
    }
    s.cancel.store(false, Ordering::Relaxed);
    s.progress.found.store(0, Ordering::Relaxed);
    s.progress.done.store(0, Ordering::Relaxed);
    std::thread::spawn(move || {
        let _rt = rt.as_ref().map(|h| h.enter());
        let s = state();
        let previous = library();
        let covers = store::covers_dir();
        let started = Instant::now();
        let result = scan::scan(
            &roots,
            previous.as_deref(),
            deep,
            covers.as_deref(),
            true,
            &s.cancel,
            &s.progress,
            rt.as_ref(),
        );
        // "Remove new sidecars after each scan": AppleDouble files only, each one verified.
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
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        st.running = false;
        st.finished_ms = now_ms();
        st.found = s.progress.found.load(Ordering::Relaxed);
        st.done = s.progress.done.load(Ordering::Relaxed);
        match result {
            Ok(lib) => {
                if let Some(dir) = store::dir() {
                    if let Err(e) = store::save_index_at(&dir, &lib) {
                        crate::engine_log::record(
                            "warn",
                            format!("collection: index not saved: {e}"),
                        );
                    }
                }
                st.games = lib.summary.total_games;
                st.locations = lib.summary.total_locations;
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
    /// Whether PS Game Library's index is on this computer to import.
    ps_game_library_found: bool,
    /// Whether this engine can move files to a trash (false in Docker or on a headless server,
    /// where `allow_permanent_delete` decides).
    trash_available: bool,
}

pub async fn get_settings() -> Response {
    Json(SettingsReply {
        settings: settings(),
        refresh_choices: store::REFRESH_CHOICES,
        ps_game_library_found: store::ps_game_library_index().is_some(),
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
        if crate::remote::path::is_remote(r) {
            // A folder on a saved server: checked when it is added (signing in and listing it),
            // not on every save of the other settings.
            if before.contains(r) {
                continue;
            }
            let (root, rt) = (r.clone(), tokio::runtime::Handle::current());
            let checked = tokio::task::spawn_blocking(move || {
                scan::Fsys::for_root(&root, Some(&rt)).map(|_| ())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            if let Err(e) = checked {
                return err(StatusCode::BAD_REQUEST, e);
            }
        } else if !std::path::Path::new(r).is_dir() {
            return err(
                StatusCode::BAD_REQUEST,
                format!("{r} is not a folder the engine can read"),
            );
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

pub async fn get_cover(Path(id): Path<String>) -> Response {
    let id = id.to_ascii_uppercase();
    let file = library()
        .and_then(|l| l.games.get(&id).and_then(|g| g.local_cover.clone()))
        .and_then(|name| store::covers_dir().map(|d| d.join(name)));
    match file.and_then(|f| std::fs::read(f).ok()) {
        Some(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, "image/png")
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
        st.found = s.progress.found.load(Ordering::Relaxed);
        st.done = s.progress.done.load(Ordering::Relaxed);
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

#[derive(Deserialize)]
pub struct ImportReq {
    /// A PS Game Library `library.json`; its default macOS location when absent.
    #[serde(default)]
    path: Option<String>,
}

/// Imports PS Game Library's index as the starting point, adds its library folder to the roots
/// when it is readable here, and copies its covers. Nothing is written to its files.
pub async fn post_import(Json(req): Json<ImportReq>) -> Response {
    let src = match req
        .path
        .map(PathBuf::from)
        .or_else(store::ps_game_library_index)
    {
        Some(p) => p,
        None => {
            return err(
                StatusCode::NOT_FOUND,
                "PS Game Library's index was not found on this computer",
            )
        }
    };
    let result = tokio::task::spawn_blocking(move || -> Result<Library, String> {
        let bytes = std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        let mut lib = store::import_ps_game_library(&bytes)?;
        let dir = store::dir().ok_or("no data folder for the engine")?;
        // Its covers sit beside its index, as <GAMEID>.png.
        if let (Some(from), Some(to)) =
            (src.parent().map(|p| p.join("covers")), store::covers_dir())
        {
            let _ = std::fs::create_dir_all(&to);
            for g in lib.games.values_mut() {
                let name = format!("{}.png", g.game_id);
                if std::fs::copy(from.join(&name), to.join(&name)).is_ok() {
                    g.local_cover = Some(name);
                }
            }
        }
        let mut s = store::load_settings_at(&dir);
        for r in &lib.roots {
            if std::path::Path::new(r).is_dir() && !s.roots.contains(r) {
                s.roots.push(r.clone());
            }
        }
        store::save_settings_at(&dir, &s)?;
        store::save_index_at(&dir, &lib)?;
        Ok(lib)
    })
    .await;
    match result {
        Ok(Ok(lib)) => {
            let summary = lib.summary.clone();
            *state().library.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(lib));
            Json(serde_json::json!({ "ok": true, "summary": summary })).into_response()
        }
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ── The console overlay ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ConsoleQuery {
    addr: String,
}

/// A title's installed version, normalized like the collection's: `APP_VER` (PS4) or
/// `CONTENT_VERSION` (PS5). `None` when it could not be read.
/// The console's titles: upper-case title ID → the path it was registered from, if any. A title
/// whose app folder is on the internal or an extended drive counts too: the installed-apps list
/// counts those, and two reads that disagreed made a game on the M.2 drive flip between
/// installed and not.
pub(crate) fn installed_titles(
    addr: &str,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    let registered = ps5upload_core::fs_ops::app_list_registered(addr)
        .map_err(|e| format!("the console's titles could not be read: {e:#}"))?;
    let mut folders = Vec::new();
    for root in ["/user/app", "/mnt/ext0/user/app", "/mnt/ext1/user/app"] {
        folders.extend(console_names(addr, root));
    }
    Ok(with_app_folders(
        registered
            .apps
            .into_iter()
            .map(|a| (a.title_id, (!a.src.is_empty()).then_some(a.src))),
        folders,
    ))
}

/// Registered titles plus the title-ID-shaped app folders, keys upper case.
fn with_app_folders(
    registered: impl IntoIterator<Item = (String, Option<String>)>,
    folders: impl IntoIterator<Item = String>,
) -> std::collections::HashMap<String, Option<String>> {
    let mut out: std::collections::HashMap<String, Option<String>> = registered
        .into_iter()
        .map(|(id, from)| (id.to_ascii_uppercase(), from))
        .collect();
    for f in folders {
        let id = f.to_ascii_uppercase();
        let shaped = id.len() == 9
            && id[..4].bytes().all(|b| b.is_ascii_uppercase())
            && id[4..].bytes().all(|b| b.is_ascii_digit());
        if shaped {
            out.entry(id).or_insert(None);
        }
    }
    out
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
    installed: &std::collections::HashMap<String, Option<String>>,
) -> crate::collection::console::ConsoleTitle {
    let mut t = crate::collection::console::ConsoleTitle::default();
    if let Some(from) = installed.get(id) {
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

/// The installed version: `APP_VER` (PS4) or `CONTENT_VERSION` (PS5), normalized. `Ok(None)`
/// when the console has neither; `Err` when it could not be asked.
fn installed_version(addr: &str, title_id: &str) -> Result<Option<String>, String> {
    let rows =
        ps5upload_core::diagnostics::appinfo_query(addr, title_id, Some("APP_VER,CONTENT_VERSION"))
            .map_err(|e| format!("{e:#}"))?;
    let get = |k: &str| {
        rows.rows
            .iter()
            .find(|r| r.key == k)
            .map(|r| r.val.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    Ok(get("APP_VER")
        .or_else(|| get("CONTENT_VERSION"))
        .map(|v| ps5upload_pkg::kind::normalize_version(&v)))
}

/// Names of the entries in a console folder; empty when it does not exist.
fn console_names(addr: &str, dir: &str) -> Vec<String> {
    ps5upload_core::fs_ops::list_dir(addr, dir, ps5upload_core::fs_ops::ListDirOptions::default())
        .map(|l| {
            l.entries
                .into_iter()
                .map(|e| e.name)
                .filter(|n| n != "." && n != "..")
                .collect()
        })
        .unwrap_or_default()
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
    use super::with_app_folders;

    #[test]
    fn an_app_folder_counts_as_installed_and_keeps_the_registered_source() {
        let m = with_app_folders(
            [("ppsa00001".to_string(), Some("/data/a".to_string()))],
            [
                "PPSA00001".to_string(),
                "PPSA00002".to_string(),
                "not-a-title".to_string(),
            ],
        );
        assert_eq!(m.len(), 2);
        assert_eq!(m["PPSA00001"].as_deref(), Some("/data/a"));
        assert_eq!(m["PPSA00002"], None);
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
