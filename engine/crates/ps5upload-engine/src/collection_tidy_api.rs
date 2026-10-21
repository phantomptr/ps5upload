//! `/api/collection/...` housekeeping: Move to Trash, the package organizer (with undo), and
//! Finder's junk files. PS Game Library's maintenance tools, behind the same preview-then-apply
//! rule it used: nothing changes on disk without a preview the user saw, and an apply quotes
//! that preview's token. Paths are never taken on trust: a trashed path must be a copy the
//! index knows, an organizer move must be one the organizer itself plans again, and junk is
//! checked again file by file at the moment it is removed.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{http::StatusCode, response::Response, Json};
use serde::{Deserialize, Serialize};

use crate::collection::organize_fs::{path_string, raw_root, split_raw, tree_for, Tree};
use crate::collection::{junk, organize, store};
use crate::collection_api::{err, library, settings};

/// How long a preview's token stays good.
const TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

fn new_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn ok<T: Serialize>(v: &T) -> Response {
    axum::response::IntoResponse::into_response(Json(serde_json::to_value(v).unwrap_or_default()))
}

/// Rescans after a change on disk, so the screen shows what is there now. A scan already
/// running will be followed by the automatic refresh, or the user's next press of Scan.
fn rescan() {
    let _ = crate::collection_api::start_scan(false);
}

// ─── Move to Trash ─────────────────────────────────────────────────────

/// `paths` in order, each once: a path named twice is one copy, counted and removed once.
fn unique(paths: &[String]) -> Vec<&String> {
    let mut seen = std::collections::HashSet::new();
    paths.iter().filter(|p| seen.insert(p.as_str())).collect()
}

/// Whether `path` sits below (never at) one of the current local library folders, reached
/// only through ordinary names. The index can be imported from elsewhere, so a copy's path is
/// checked against the folders the user has now before anything is removed.
fn strictly_under_a_root(path: &str, roots: &[String]) -> bool {
    use std::path::Component;
    let p = FsPath::new(path);
    roots
        .iter()
        .filter(|r| !r.is_empty() && !crate::remote::path::is_remote(r))
        .any(|r| match p.strip_prefix(r) {
            Ok(rest) => {
                rest.components().next().is_some()
                    && rest.components().all(|c| matches!(c, Component::Normal(_)))
            }
            Err(_) => false,
        })
}

#[derive(Debug, Clone, Serialize)]
pub struct TrashItem {
    pub path: String,
    pub name: String,
    pub game_id: String,
    pub title: String,
    pub size_bytes: u64,
}

struct PendingTrash {
    items: Vec<TrashItem>,
    made: Instant,
    /// The removal the user was shown and confirmed.
    mode: &'static str,
}

fn pending_trash() -> &'static Mutex<HashMap<String, PendingTrash>> {
    static P: OnceLock<Mutex<HashMap<String, PendingTrash>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether this engine can move files to a trash at all: not on a phone, not in a container
/// (a trash inside it frees nothing and is gone with it), and on Linux only with a desktop
/// session to own one.
pub(crate) fn trash_available() -> bool {
    if cfg!(any(target_os = "android", target_os = "ios")) || crate::pkg_install::in_container() {
        return false;
    }
    if cfg!(target_os = "linux") {
        return ["DISPLAY", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP"]
            .iter()
            .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
    }
    true
}

/// How Move to Trash removes a copy here: `trash`, `delete` (for good, where there is no trash
/// and the user allowed it), or `none`.
fn removal_mode() -> &'static str {
    if trash_available() {
        "trash"
    } else if settings().allow_permanent_delete {
        "delete"
    } else {
        "none"
    }
}

/// Deletes a copy for good: a file, or a game folder and everything in it. A symlink is
/// removed itself, never followed.
fn delete_permanently(path: &FsPath) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct TrashPreviewReq {
    pub paths: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TrashPreview {
    pub token: String,
    pub items: Vec<TrashItem>,
    pub total_bytes: u64,
    /// False when the copies cannot be removed here (no trash, and permanent delete is off).
    pub available: bool,
    /// `trash` (recoverable) or `delete` (for good); `none` when unavailable.
    pub mode: &'static str,
}

/// POST /api/collection/trash/preview: what moving these copies to the trash would take.
/// Every path must be a copy in the index.
pub async fn post_trash_preview(Json(req): Json<TrashPreviewReq>) -> Response {
    let Some(lib) = library() else {
        return err(
            StatusCode::CONFLICT,
            "the collection has not been scanned yet",
        );
    };
    let roots = settings().roots;
    let mut items = Vec::new();
    for p in unique(&req.paths) {
        let found = lib.games.values().find_map(|g| {
            g.locations
                .iter()
                .find(|l| &l.absolute_path == p)
                .map(|l| (g, l))
        });
        let Some((g, l)) = found else {
            return err(
                StatusCode::BAD_REQUEST,
                format!("not a copy in the collection: {p}"),
            );
        };
        if crate::remote::path::is_remote(&l.absolute_path) {
            return err(
                StatusCode::BAD_REQUEST,
                format!("{} is on a server: remove it there", l.name),
            );
        }
        if !strictly_under_a_root(&l.absolute_path, &roots) {
            return err(
                StatusCode::BAD_REQUEST,
                format!("{} is not inside a library folder", l.absolute_path),
            );
        }
        if !FsPath::new(&l.absolute_path).exists() {
            return err(
                StatusCode::CONFLICT,
                format!("{} is no longer there; scan again", l.absolute_path),
            );
        }
        items.push(TrashItem {
            path: l.absolute_path.clone(),
            name: l.name.clone(),
            game_id: g.game_id.clone(),
            title: g.title.clone(),
            size_bytes: l.size_bytes,
        });
    }
    if items.is_empty() {
        return err(StatusCode::BAD_REQUEST, "nothing to move to the trash");
    }
    let token = new_token();
    let total_bytes = items.iter().map(|i| i.size_bytes).sum();
    let mode = removal_mode();
    {
        let mut p = pending_trash().lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|_, v| v.made.elapsed() < TOKEN_TTL);
        p.insert(
            token.clone(),
            PendingTrash {
                items: items.clone(),
                made: Instant::now(),
                mode,
            },
        );
    }
    ok(&TrashPreview {
        token,
        items,
        total_bytes,
        available: mode != "none",
        mode,
    })
}

#[derive(Debug, Deserialize)]
pub struct TokenReq {
    pub token: String,
}

#[derive(Debug, Default, Serialize)]
pub struct TrashResult {
    pub moved: Vec<String>,
    pub failed: Vec<String>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn move_to_trash(path: &FsPath) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn move_to_trash(_path: &FsPath) -> Result<(), String> {
    Err("this device has no trash".into())
}

/// POST /api/collection/trash/apply: moves a preview's copies to the OS trash (recoverable).
pub async fn post_trash_apply(Json(req): Json<TokenReq>) -> Response {
    let pending = pending_trash()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&req.token);
    let Some(pending) = pending.filter(|p| p.made.elapsed() < TOKEN_TTL) else {
        return err(
            StatusCode::BAD_REQUEST,
            "this preview has expired; choose the copies again",
        );
    };
    let out = tokio::task::spawn_blocking(move || {
        let mut out = TrashResult::default();
        for it in pending.items {
            let p = PathBuf::from(&it.path);
            if !p.exists() {
                out.failed.push(format!("{}: no longer there", it.path));
                continue;
            }
            let res = match pending.mode {
                "trash" => move_to_trash(&p),
                "delete" => delete_permanently(&p),
                _ => Err("this engine has no trash, and permanent delete is off".into()),
            };
            match res {
                Ok(()) => {
                    crate::engine_log::record(
                        "info",
                        if pending.mode == "delete" {
                            format!("collection: deleted permanently: {}", it.path)
                        } else {
                            format!("collection: moved to trash: {}", it.path)
                        },
                    );
                    out.moved.push(it.path);
                }
                Err(e) => out.failed.push(format!("{}: {e}", it.path)),
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    if !out.moved.is_empty() {
        rescan();
    }
    ok(&out)
}

// ─── Organizer ─────────────────────────────────────────────────────────

struct PendingPlan {
    made: Instant,
}

fn pending_plans() -> &'static Mutex<HashMap<String, PendingPlan>> {
    static P: OnceLock<Mutex<HashMap<String, PendingPlan>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Every collection root, on this computer or on a saved server: (the root as the organizer's
/// trees take it, the root as the settings and the index name it).
fn roots() -> Vec<(PathBuf, String)> {
    settings()
        .roots
        .iter()
        .filter(|r| !r.trim().is_empty())
        .map(|r| (raw_root(r), r.clone()))
        .collect()
}

fn path_str(p: &FsPath) -> String {
    path_string(p)
}

/// The saved connection a path is on, if it is on one.
fn connection_of(p: &FsPath) -> Option<String> {
    split_raw(&path_str(p)).map(|(id, _)| id)
}

/// The saved connection with id `id`, as saved now.
fn saved(id: &str) -> Option<crate::remote::store::Connection> {
    crate::remote::pool::global()
        .ok()
        .and_then(|r| r.store.get(id))
        .map(|(c, _)| c)
}

/// The saved server's name for a `remote://` path ("" on this computer).
fn server_name(p: &FsPath) -> String {
    let Some(id) = connection_of(p) else {
        return String::new();
    };
    saved(&id).map(|c| c.name).unwrap_or(id)
}

/// How a folder is shown: its path here, or "Server: /path" on a saved server.
fn label(p: &FsPath) -> String {
    let s = path_str(p);
    match split_raw(&s) {
        Some((_, path)) => format!("{}: {path}", server_name(p)),
        None => s,
    }
}

/// Where a server folder is, for the undo log: which server, and the folder on it.
fn origin_of(p: &FsPath) -> Option<organize::Origin> {
    let (id, path) = split_raw(&path_str(p))?;
    let c = saved(&id)?;
    Some(organize::Origin {
        protocol: format!("{:?}", c.protocol).to_ascii_lowercase(),
        host: c.host.trim().to_ascii_lowercase(),
        port: c.port,
        share: c.share.trim_matches(['/', '\\']).to_ascii_lowercase(),
        path,
    })
}

/// One [`Tree`] per place (this computer, or each server) for one request, so a server's
/// folders are listed once however many containers share it.
#[derive(Default)]
struct Trees(HashMap<String, Result<Box<dyn Tree>, String>>);

impl Trees {
    fn get(&mut self, p: &FsPath) -> Result<&dyn Tree, String> {
        let k = connection_of(p).unwrap_or_default();
        self.0
            .entry(k)
            .or_insert_with(|| tree_for(p))
            .as_ref()
            .map(|t| t.as_ref())
            .map_err(Clone::clone)
    }
}

/// The configured roots are independent organization boundaries. Discover current contents
/// when planning instead of filtering roots through a possibly stale, package-only index.
fn all_containers(trees: &mut Trees) -> (Vec<PathBuf>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for (root, _) in roots() {
        if out.contains(&root) {
            continue;
        }
        match trees.get(&root) {
            Ok(tree) if tree.list(&root).is_some() => out.push(root),
            Ok(tree) => problems.push(format!(
                "{}: {}",
                label(&root),
                tree.trouble()
                    .unwrap_or_else(|| "directory could not be listed".into())
            )),
            Err(e) => problems.push(format!("{}: {e}", label(&root))),
        }
    }
    out.sort();
    (out, problems)
}

fn organize_dir() -> Option<PathBuf> {
    store::dir().map(|d| d.join("organize"))
}

#[derive(Debug, Serialize)]
pub struct PlanResponse {
    pub token: String,
    pub containers: Vec<String>,
    /// How to show each container: its path, or "Server: /path" on a saved server.
    pub labels: std::collections::BTreeMap<String, String>,
    pub moves: Vec<organize::Move>,
    pub skipped: Vec<organize::Skipped>,
    pub unsure_count: usize,
    pub total_bytes: u64,
}

/// POST /api/collection/organize/plan: what the organizer would do. Touches nothing.
pub async fn post_organize_plan() -> Response {
    // Off the async workers: it lists every package folder, perhaps over the network.
    let (containers, problems, plans) = tokio::task::spawn_blocking(|| {
        let mut trees = Trees::default();
        let (containers, problems) = all_containers(&mut trees);
        let lib = library();
        let roots = roots();
        let plans = containers
            .iter()
            .map(|c| match trees.get(c) {
                Ok(t) => organize::plan_cached(t, c, lib.as_deref(), &roots),
                Err(_) => organize::Plan::default(),
            })
            .collect::<Vec<_>>();
        (containers, problems, plans)
    })
    .await
    .unwrap_or_default();
    if containers.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            if problems.is_empty() {
                "no collection root could be read".to_string()
            } else {
                format!("no collection root could be read: {}", problems.join("; "))
            },
        );
    }
    let mut resp = PlanResponse {
        token: new_token(),
        containers: containers.iter().map(|c| path_str(c)).collect(),
        labels: containers.iter().map(|c| (path_str(c), label(c))).collect(),
        moves: Vec::new(),
        skipped: problems
            .into_iter()
            .map(|detail| organize::Skipped {
                container: String::new(),
                path: String::new(),
                reason: organize::SKIP_UNREADABLE.into(),
                detail,
            })
            .collect(),
        unsure_count: 0,
        total_bytes: 0,
    };
    for p in plans {
        resp.unsure_count += p.unsure.len();
        resp.moves.extend(p.moves);
        resp.skipped.extend(p.skipped);
    }
    resp.total_bytes = resp.moves.iter().map(|m| m.size).sum();
    {
        let mut p = pending_plans().lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|_, v| v.made.elapsed() < TOKEN_TTL);
        p.insert(
            resp.token.clone(),
            PendingPlan {
                made: Instant::now(),
            },
        );
    }
    ok(&resp)
}

#[derive(Debug, Deserialize)]
pub struct ChosenMove {
    pub container: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
pub struct OrganizeApplyReq {
    pub token: String,
    /// The moves the user kept ticked in the preview.
    pub moves: Vec<ChosenMove>,
}

#[derive(Debug, Default, Serialize)]
pub struct OrganizeApplyResult {
    pub moved: Vec<organize::Move>,
    pub failed: Vec<organize::FailedMove>,
    /// Moves dropped because the package changed since the preview.
    pub dropped: Vec<String>,
    pub kept_dirs: Vec<String>,
    pub logs: Vec<String>,
}

/// `dir/stem`, or `dir/stem_2`, `_3`, ... when a log by that name exists: two folders with
/// the same name organized in the same second must not share an undo log.
fn unique_log_base(dir: &FsPath, stem: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            stem.to_string()
        } else {
            format!("{stem}_{n}")
        };
        if !dir.join(format!("{name}.json")).exists() {
            return dir.join(name);
        }
        n += 1;
    }
}

fn slug(container: &FsPath) -> String {
    let base = container
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "packages".into()
    } else {
        s
    }
}

fn write_unsure_report(path: &FsPath, unsure: &[organize::Move]) {
    let mut lines = vec![
        "Packages whose content kind was inferred rather than read.".to_string(),
        "Each entry shows the chosen kind and the evidence behind it.".to_string(),
        String::new(),
    ];
    for m in unsure {
        lines.push(m.from.clone());
        lines.push(format!("    guessed : {}", m.kind.to_uppercase()));
        lines.push(format!(
            "    title   : {} [{}] v{}",
            if m.title.is_empty() {
                "(unknown)"
            } else {
                &m.title
            },
            m.title_id,
            if m.version.is_empty() {
                "?"
            } else {
                &m.version
            }
        ));
        lines.push(format!("    evidence: {}", m.reason));
        lines.push(String::new());
    }
    if unsure.is_empty() {
        lines.push("None: every package's kind was read directly.".into());
    }
    let _ = std::fs::write(path, lines.join("\n"));
}

/// POST /api/collection/organize/apply: runs the chosen moves, each re-planned first.
pub async fn post_organize_apply(Json(req): Json<OrganizeApplyReq>) -> Response {
    let known = pending_plans()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&req.token)
        .is_some_and(|p| p.made.elapsed() < TOKEN_TTL);
    if !known {
        return err(
            StatusCode::BAD_REQUEST,
            "this preview has expired; preview again",
        );
    }
    let Some(log_dir) = organize_dir() else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no data folder for the undo log",
        );
    };
    // Off the async workers: it lists every package folder, perhaps over the network.
    let result = tokio::task::spawn_blocking(move || {
        let mut trees = Trees::default();
        let (containers, _) = all_containers(&mut trees);
        let lib = library();
        let roots = roots();
        let mut out = OrganizeApplyResult::default();
        let mut wanted: HashMap<String, Vec<ChosenMove>> = HashMap::new();
        for m in req.moves {
            wanted.entry(m.container.clone()).or_default().push(m);
        }
        for (container, chosen) in wanted {
            let c = PathBuf::from(&container);
            // Only folders the collection itself would organize.
            if !containers.contains(&c) {
                out.dropped.extend(chosen.into_iter().map(|m| m.from));
                continue;
            }
            let tree = match trees.get(&c) {
                Ok(t) => t,
                Err(e) => {
                    out.dropped.push(format!("{}: {e}", label(&c)));
                    continue;
                }
            };
            let plan = organize::plan_cached(tree, &c, lib.as_deref(), &roots);
            let live: HashMap<&str, &organize::Move> =
                plan.moves.iter().map(|m| (m.from.as_str(), m)).collect();
            let mut picked = Vec::new();
            for want in chosen {
                match live.get(want.from.as_str()) {
                    Some(m) if m.to == want.to => picked.push((*m).clone()),
                    _ => out.dropped.push(want.from),
                }
            }
            if picked.is_empty() {
                continue;
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = std::fs::create_dir_all(&log_dir);
            let base = unique_log_base(&log_dir, &format!("organize_{stamp}_{}", slug(&c)));
            let log_path = base.with_file_name(format!(
                "{}.json",
                base.file_name().unwrap_or_default().to_string_lossy()
            ));
            write_unsure_report(
                &base.with_file_name(format!(
                    "{}_unsure.txt",
                    base.file_name().unwrap_or_default().to_string_lossy()
                )),
                &plan.unsure,
            );
            match organize::apply(tree, &c, picked, &log_path, &server_name(&c), origin_of(&c)) {
                Ok(log) => {
                    crate::engine_log::record(
                        "info",
                        format!(
                            "collection: organized {}: {} moved, {} failed (undo log {})",
                            container,
                            log.completed.len(),
                            log.failed.len(),
                            log_path.display()
                        ),
                    );
                    out.moved.extend(log.completed);
                    out.failed.extend(log.failed);
                    // Where each is: one name ("old") could be in several containers.
                    let at = label(&c);
                    out.kept_dirs
                        .extend(log.kept_dirs.iter().map(|d| format!("{at}/{d}")));
                    out.logs.push(
                        log_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                Err(e) => out.dropped.push(format!(
                    "{container}: the undo log could not be written ({e})"
                )),
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    if !result.moved.is_empty() {
        rescan();
    }
    ok(&result)
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub id: String,
    pub container_root: String,
    /// How to show the folder: its path, or "Server: /path" on a saved server.
    pub label: String,
    pub created_at: String,
    pub moved: usize,
    pub failed: usize,
    pub reverted_at: Option<String>,
}

/// GET /api/collection/organize/runs: past organizer runs, newest first, for Undo.
pub async fn get_organize_runs() -> Response {
    let Some(dir) = organize_dir() else {
        return ok(&Vec::<RunSummary>::new());
    };
    let mut runs: Vec<RunSummary> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("organize_") || !name.ends_with(".json") {
                return None;
            }
            let log: organize::UndoLog =
                serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            let root = FsPath::new(&log.container_root);
            let shown = match (
                connection_of(root),
                crate::remote::path::parse(&log.container_root),
            ) {
                (Some(id), Ok(r)) => {
                    // The server's name now; the one it had then, should it since be gone.
                    let name = crate::remote::pool::global()
                        .ok()
                        .and_then(|g| g.store.get(&id))
                        .map(|(c, _)| c.name)
                        .unwrap_or_else(|| {
                            if log.server.is_empty() {
                                id.clone()
                            } else {
                                log.server.clone()
                            }
                        });
                    format!("{name}: {}", r.path)
                }
                _ => log.container_root.clone(),
            };
            Some(RunSummary {
                id: name,
                label: shown,
                container_root: log.container_root,
                created_at: log.created_at,
                moved: log.completed.len(),
                failed: log.failed.len(),
                reverted_at: log.reverted_at,
            })
        })
        .collect();
    runs.sort_by(|a, b| b.id.cmp(&a.id));
    ok(&runs)
}

/// Whether a run's folder is still where the run was made: the saved connection it names must
/// still point at the same server. An older log, from before that was recorded, is undone on
/// the connection with the same id (Undo itself checks the folder is there), with a note.
fn same_place(log_path: &FsPath) -> Result<Option<String>, String> {
    let bytes = std::fs::read(log_path).map_err(|e| e.to_string())?;
    let log: organize::UndoLog = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let root = FsPath::new(&log.container_root);
    let Some((id, _)) = split_raw(&log.container_root) else {
        return Ok(None);
    };
    let Some(now) = origin_of(root) else {
        return Err(format!(
            "the saved connection this run was made on ({id}) is gone"
        ));
    };
    match &log.origin {
        Some(then) if then.same_server(&now) => Ok(None),
        Some(then) => Err(format!(
            "the saved connection now points to another server ({} {}:{} instead of {} {}:{}); \
             undo refused so nothing is renamed there",
            now.protocol, now.host, now.port, then.protocol, then.host, then.port
        )),
        None => Ok(Some(format!(
            "This run was logged before ps5upload recorded which server it was on: it was undone \
             on the saved connection \"{}\" as it is set up now.",
            server_name(root)
        ))),
    }
}

#[derive(Debug, Deserialize)]
pub struct RevertReq {
    pub id: String,
}

/// POST /api/collection/organize/revert: undoes one run from its log.
pub async fn post_organize_revert(Json(req): Json<RevertReq>) -> Response {
    // A log name only: never a path.
    if req.id.contains(['/', '\\'])
        || !req.id.starts_with("organize_")
        || !req.id.ends_with(".json")
    {
        return err(StatusCode::BAD_REQUEST, "not an organizer run");
    }
    let Some(path) = organize_dir().map(|d| d.join(&req.id)) else {
        return err(StatusCode::NOT_FOUND, "no such run");
    };
    let res = tokio::task::spawn_blocking(move || {
        let note = same_place(&path).map_err(std::io::Error::other)?;
        organize::revert(&path, &tree_for).map(|mut r| {
            r.note = note;
            r
        })
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))
    .and_then(|r| r);
    match res {
        Ok(r) => {
            crate::engine_log::record(
                "info",
                format!(
                    "collection: undid organizer run {}: {} restored, {} problems",
                    req.id,
                    r.reverted.len(),
                    r.problems.len()
                ),
            );
            if !r.reverted.is_empty() {
                rescan();
            }
            ok(&r)
        }
        Err(e) => err(StatusCode::CONFLICT, e.to_string()),
    }
}

// ─── Finder's junk files ───────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct JunkStatus {
    pub running: bool,
    pub cancelled: bool,
    pub checked: u64,
    pub found: usize,
    pub sidecars: usize,
    /// Space the files take on disk.
    pub allocated: u64,
    /// Token for the clean-up of what this scan found; present once it finished.
    pub token: Option<String>,
    /// Up to 200 of the files found, for the preview.
    pub sample: Vec<junk::JunkFile>,
    pub finished_ms: u64,
}

struct JunkState {
    status: Mutex<JunkStatus>,
    files: Mutex<Vec<PathBuf>>,
    cancel: AtomicBool,
    checked: AtomicU64,
}

fn junk_state() -> &'static Arc<JunkState> {
    static S: OnceLock<Arc<JunkState>> = OnceLock::new();
    S.get_or_init(|| {
        Arc::new(JunkState {
            status: Mutex::new(JunkStatus::default()),
            files: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
            checked: AtomicU64::new(0),
        })
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// POST /api/collection/junk/scan: looks for Finder's files under every root, in the background.
pub async fn post_junk_scan() -> Response {
    // Finder's files on a server are the server's business: only this computer's folders.
    let roots: Vec<PathBuf> = settings()
        .roots
        .iter()
        .filter(|r| !crate::remote::path::is_remote(r))
        .map(PathBuf::from)
        .collect();
    if roots.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "add a folder to the Collection first",
        );
    }
    let s = junk_state().clone();
    {
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        if st.running {
            return ok(&*st);
        }
        *st = JunkStatus {
            running: true,
            ..JunkStatus::default()
        };
    }
    s.cancel.store(false, Ordering::Relaxed);
    s.checked.store(0, Ordering::Relaxed);
    std::thread::spawn(move || {
        let mut all = junk::JunkReport::default();
        for root in &roots {
            let r = junk::find(root, &|| s.cancel.load(Ordering::Relaxed));
            all.checked += r.checked;
            s.checked.fetch_add(r.checked, Ordering::Relaxed);
            all.allocated += r.allocated;
            all.files.extend(r.files);
            if r.cancelled {
                all.cancelled = true;
                break;
            }
        }
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        st.running = false;
        st.cancelled = all.cancelled;
        st.checked = all.checked;
        st.found = all.files.len();
        st.sidecars = all.files.iter().filter(|f| f.sidecar).count();
        st.allocated = all.allocated;
        st.finished_ms = now_ms();
        st.sample = all.files.iter().take(200).cloned().collect();
        st.token = (!all.cancelled && !all.files.is_empty()).then(new_token);
        *s.files.lock().unwrap_or_else(|e| e.into_inner()) = all
            .files
            .into_iter()
            .map(|f| PathBuf::from(f.path))
            .collect();
    });
    ok(&*junk_state()
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}

/// GET /api/collection/junk: the scan's progress, then what it found.
pub async fn get_junk() -> Response {
    let s = junk_state();
    let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if st.running {
        st.checked = s.checked.load(Ordering::Relaxed);
    }
    ok(&st)
}

pub async fn post_junk_cancel() -> Response {
    junk_state().cancel.store(true, Ordering::Relaxed);
    get_junk().await
}

/// POST /api/collection/junk/clean: removes what the finished scan found.
pub async fn post_junk_clean(Json(req): Json<TokenReq>) -> Response {
    let s = junk_state().clone();
    {
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        if st.running || st.token.as_deref() != Some(req.token.as_str()) {
            return err(StatusCode::BAD_REQUEST, "scan for junk files again first");
        }
        st.token = None;
    }
    let files = std::mem::take(&mut *s.files.lock().unwrap_or_else(|e| e.into_inner()));
    let out = tokio::task::spawn_blocking(move || junk::remove(&files))
        .await
        .unwrap_or_default();
    crate::engine_log::record(
        "info",
        format!(
            "collection: junk clean-up removed {} file(s), {} bytes on disk; {} failed",
            out.removed,
            out.freed,
            out.failed.len()
        ),
    );
    {
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        *st = JunkStatus::default();
    }
    ok(&out)
}

// ─── macOS: Finder's network/USB .DS_Store, and Spotlight on the library's drive ─────────

#[derive(Debug, Serialize)]
pub struct MacSettings {
    /// This engine runs on a Mac, so these can be read and changed.
    pub available: bool,
    /// Finder writes no `.DS_Store` on network shares / USB drives.
    pub finder_network_off: bool,
    pub finder_usb_off: bool,
    pub volumes: Vec<SpotlightVolume>,
}

#[derive(Debug, Serialize)]
pub struct SpotlightVolume {
    pub volume: String,
    /// `on`, `off` or `unknown`, as `mdutil -s` reports it.
    pub indexing: String,
    /// An old index (`.Spotlight-V100`) is on the drive.
    pub has_index: bool,
}

/// The drive a path is on, when it is not the startup disk (Spotlight is offered only there:
/// turning it off for the startup disk would affect the whole Mac).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn external_volume(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/Volumes/")?;
    let name = rest.split('/').next().filter(|n| !n.is_empty())?;
    Some(format!("/Volumes/{name}"))
}

#[cfg(target_os = "macos")]
fn defaults_bool(key: &str) -> bool {
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.apple.desktopservices", key])
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
}

#[cfg(target_os = "macos")]
fn spotlight(volume: &str) -> SpotlightVolume {
    let out = std::process::Command::new("/usr/bin/mdutil")
        .args(["-s", volume])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();
    let indexing = if out.contains("indexing enabled") {
        "on"
    } else if out.contains("indexing disabled") || out.contains("indexing and searching disabled") {
        "off"
    } else {
        "unknown"
    };
    SpotlightVolume {
        volume: volume.into(),
        indexing: indexing.into(),
        has_index: FsPath::new(volume).join(".Spotlight-V100").exists(),
    }
}

fn library_volumes() -> Vec<String> {
    let mut v: Vec<String> = settings()
        .roots
        .iter()
        .filter_map(|r| external_volume(r))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// GET /api/collection/macos
pub async fn get_macos() -> Response {
    #[cfg(target_os = "macos")]
    {
        let volumes = tokio::task::spawn_blocking(|| {
            library_volumes().iter().map(|v| spotlight(v)).collect()
        })
        .await
        .unwrap_or_default();
        ok(&MacSettings {
            available: true,
            finder_network_off: defaults_bool("DSDontWriteNetworkStores"),
            finder_usb_off: defaults_bool("DSDontWriteUSBStores"),
            volumes,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = library_volumes;
        ok(&MacSettings {
            available: false,
            finder_network_off: false,
            finder_usb_off: false,
            volumes: Vec::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct FinderReq {
    /// Stop Finder writing `.DS_Store` on network shares and USB drives (true), or let it again.
    pub off: bool,
}

/// PUT /api/collection/macos/finder: Finder's own setting for this Mac user. Takes effect for
/// folders Finder opens after it restarts (log out and in, or relaunch Finder).
pub async fn put_macos_finder(Json(req): Json<FinderReq>) -> Response {
    #[cfg(target_os = "macos")]
    {
        let v = if req.off { "true" } else { "false" };
        for key in ["DSDontWriteNetworkStores", "DSDontWriteUSBStores"] {
            let st = std::process::Command::new("/usr/bin/defaults")
                .args(["write", "com.apple.desktopservices", key, "-bool", v])
                .status();
            if !st.is_ok_and(|s| s.success()) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not set {key}"),
                );
            }
        }
        get_macos().await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = req.off;
        err(StatusCode::BAD_REQUEST, "only on a Mac")
    }
}

#[derive(Debug, Deserialize)]
pub struct SpotlightReq {
    pub volume: String,
    /// `off`: stop indexing the drive and remove its index. `remove_index`: remove the old index.
    pub action: String,
}

/// POST /api/collection/macos/spotlight: needs an administrator, so macOS asks for the
/// password itself (the engine never sees it).
pub async fn post_macos_spotlight(Json(req): Json<SpotlightReq>) -> Response {
    #[cfg(target_os = "macos")]
    {
        // Only a drive the collection is on.
        if !library_volumes().contains(&req.volume) {
            return err(StatusCode::BAD_REQUEST, "not a drive the collection is on");
        }
        let quoted = format!("'{}'", req.volume.replace('\'', "'\\''"));
        let (command, prompt) = match req.action.as_str() {
            "off" => (
                format!("/usr/bin/mdutil -i off {quoted} && /usr/bin/mdutil -X {quoted}"),
                format!("ps5upload wants to turn off Spotlight for {}.", req.volume),
            ),
            "remove_index" => (
                format!("/usr/bin/mdutil -X {quoted}"),
                format!(
                    "ps5upload wants to remove the old Spotlight index from {}.",
                    req.volume
                ),
            ),
            _ => return err(StatusCode::BAD_REQUEST, "unknown action"),
        };
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "do shell script \"{}\" with prompt \"{}\" with administrator privileges",
            esc(&command),
            esc(&prompt)
        );
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new("/usr/bin/osascript")
                .args(["-e", &script])
                .output()
        })
        .await;
        match out {
            Ok(Ok(o)) if o.status.success() => get_macos().await,
            Ok(Ok(o)) => err(
                StatusCode::CONFLICT,
                format!("not changed: {}", String::from_utf8_lossy(&o.stderr).trim()),
            ),
            _ => err(StatusCode::INTERNAL_SERVER_ERROR, "could not ask macOS"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (req.volume, req.action);
        err(StatusCode::BAD_REQUEST, "only on a Mac")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotlight_is_offered_only_for_an_external_drive() {
        assert_eq!(
            external_volume("/Volumes/Storage/PS5/games").as_deref(),
            Some("/Volumes/Storage")
        );
        assert_eq!(external_volume("/Users/me/games"), None);
        assert_eq!(external_volume("/Volumes/"), None);
    }

    #[test]
    fn a_permanent_delete_takes_a_file_or_a_whole_folder_and_never_follows_a_link() {
        let d = std::env::temp_dir().join(format!("ps5u-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Game-app/sce_sys")).unwrap();
        std::fs::write(d.join("Game-app/sce_sys/param.json"), b"{}").unwrap();
        std::fs::write(d.join("x.pkg"), b"pkg").unwrap();
        std::fs::create_dir_all(d.join("keep")).unwrap();
        std::fs::write(d.join("keep/precious"), b"!").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.join("keep"), d.join("link")).unwrap();
        delete_permanently(&d.join("Game-app")).unwrap();
        delete_permanently(&d.join("x.pkg")).unwrap();
        #[cfg(unix)]
        delete_permanently(&d.join("link")).unwrap();
        assert!(!d.join("Game-app").exists() && !d.join("x.pkg").exists());
        assert!(d.join("keep/precious").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_paths_strictly_inside_a_current_local_root_can_be_trashed() {
        let roots = vec!["/Volumes/S/games".to_string(), "remote://abc/x".to_string()];
        assert!(strictly_under_a_root("/Volumes/S/games/a/b.pkg", &roots));
        assert!(!strictly_under_a_root("/Volumes/S/games", &roots));
        assert!(!strictly_under_a_root("/Volumes/S/games/../secret", &roots));
        assert!(!strictly_under_a_root("/Volumes/S/gamesX/a.pkg", &roots));
        assert!(!strictly_under_a_root("/etc/passwd", &roots));
        assert!(!strictly_under_a_root("remote://abc/x/y.pkg", &roots));
        assert!(!strictly_under_a_root("/a.pkg", &[]));
    }

    #[test]
    fn a_path_named_twice_is_previewed_once() {
        let paths = vec!["/a".to_string(), "/b".to_string(), "/a".to_string()];
        assert_eq!(unique(&paths), vec!["/a", "/b"]);
    }

    #[test]
    fn same_named_folders_get_their_own_undo_logs() {
        let d = std::env::temp_dir().join(format!("ps5u-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = unique_log_base(&d, "organize_1_fpkg");
        std::fs::write(d.join("organize_1_fpkg.json"), b"{}").unwrap();
        let b = unique_log_base(&d, "organize_1_fpkg");
        assert_ne!(a, b);
        assert_eq!(b, d.join("organize_1_fpkg_2"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn undo_is_refused_when_the_saved_connection_now_points_elsewhere() {
        use crate::remote::store::{conn, Protocol, Secret};
        let r = crate::remote::pool::testing::install_global(&[]);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        let root = format!("remote://{id}/games/fpkg");
        let now = origin_of(FsPath::new(&root)).unwrap();
        let d = std::env::temp_dir().join(format!("ps5u-place-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let log = |origin: Option<organize::Origin>, root: &str| {
            let p = d.join(format!("organize_{}.json", uuid::Uuid::new_v4()));
            let v = serde_json::json!({
                "version": 1, "container_root": root, "created_at": "x", "moves": [],
                "origin": origin,
            });
            std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap();
            p
        };
        assert_eq!(same_place(&log(Some(now.clone()), &root)), Ok(None));
        let moved = organize::Origin {
            host: "another-nas".into(),
            ..now.clone()
        };
        let refused = same_place(&log(Some(moved), &root)).unwrap_err();
        assert!(refused.contains("another server"), "{refused}");
        let other_share = organize::Origin {
            share: "elsewhere".into(),
            ..now
        };
        assert!(same_place(&log(Some(other_share), &root)).is_err());
        // An older log: undone on the same connection id, with a note.
        assert!(same_place(&log(None, &root)).unwrap().is_some());
        // A connection that is gone: refused.
        assert!(same_place(&log(None, "remote://gone-0000/games")).is_err());
        // This computer's folders have nothing to compare.
        assert_eq!(same_place(&log(None, "/Volumes/x/fpkg")), Ok(None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn log_slugs_are_plain() {
        assert_eq!(slug(FsPath::new("/x/game fpkgs (old)")), "game-fpkgs--old");
        assert_eq!(slug(FsPath::new("/x/★")), "packages");
    }
}
