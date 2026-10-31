//! Copy a file or folder from a saved server to this computer, as a job with progress and
//! Cancel — for the jobs that need the bytes locally (Convert, small inputs like payloads and
//! save zips). And a cheap look at a remote game folder: copy only its metadata files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::pool::Remote;
use super::{Entry, RemoteError, RemoteFs};
use crate::JobState;

/// Chunk size for each server read while copying.
const CHUNK: u64 = 8 * 1024 * 1024;
/// Most files one fetch copies.
const MAX_FILES: usize = 200_000;
/// Space left free after a copy, so it never fills the disk to the last byte.
const HEADROOM: u64 = 1024 * 1024 * 1024;

/// The server, every file to copy (relative path, entry), and whether the pick was a folder.
/// The copy's files, whether it is a folder, and a RAR set's other volumes (server path, entry).
type Listed = (
    Arc<dyn RemoteFs>,
    Vec<(String, Entry)>,
    bool,
    Vec<(String, Entry)>,
);

pub type Jobs = Arc<Mutex<HashMap<Uuid, JobState>>>;

/// What a fetch needs from the engine; tests pass their own.
pub struct FetchDeps {
    pub jobs: Jobs,
    pub events_tx: broadcast::Sender<String>,
    pub free_bytes: fn(&Path) -> Option<u64>,
    pub backoff: super::pool::Backoff,
}

#[derive(Deserialize)]
pub struct FetchBody {
    pub path: String,
    #[serde(default)]
    pub dest_dir: Option<String>,
}

/// Copies this engine made, and what to delete to remove each: the copy itself (plus the
/// other volumes of a RAR set copied beside it), or the temp folder the engine created for it.
/// Only these can be removed through `cleanup`.
fn fetched() -> &'static Mutex<HashMap<PathBuf, Vec<PathBuf>>> {
    static F: OnceLock<Mutex<HashMap<PathBuf, Vec<PathBuf>>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The other volumes of the RAR set `name` starts, among `names` (the same folder), in volume
/// order: `game.part1.rar` → `game.part2.rar`, …; `game.rar` → `game.r00` … `game.r99`, then
/// `game.s00` … `game.s99`, `game.t00` … (WinRAR's old naming continues with the next letter
/// past `.r99`). Empty when `name` is not a set's first volume. UnRAR finds the rest of a set
/// beside the first volume, so a copy of the first alone cannot be extracted.
pub(crate) fn rar_set_siblings(name: &str, names: &[String]) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let Some(base) = lower.strip_suffix(".rar") else {
        return Vec::new();
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut out: Vec<(u32, String)> = if let Some(at) = base.rfind(".part") {
        let num = &base[at + 5..];
        if !digits(num) || num.parse::<u32>().ok() != Some(1) {
            return Vec::new();
        }
        let prefix = format!("{}.part", &base[..at]);
        names
            .iter()
            .filter_map(|n| {
                let l = n.to_ascii_lowercase();
                if l == lower {
                    return None;
                }
                let num = l.strip_prefix(&prefix)?.strip_suffix(".rar")?;
                if !digits(num) {
                    return None;
                }
                Some((num.parse::<u32>().ok()?, n.clone()))
            })
            .collect()
    } else {
        names
            .iter()
            .filter_map(|n| {
                let (stem, index) = ps5upload_core::archive_extract::rar_old_volume_index(n)?;
                stem.eq_ignore_ascii_case(base).then(|| (index, n.clone()))
            })
            .collect()
    };
    out.sort();
    out.into_iter().map(|(_, n)| n).collect()
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

fn running(started_at_ms: u64, bytes_sent: u64, total_bytes: u64) -> JobState {
    JobState::Running {
        stage: None,
        started_at_ms,
        bytes_sent,
        total_bytes,
        files: Default::default(),
        skipped_files: 0,
        skipped_bytes: 0,
        files_processing: 0,
        files_finalized: 0,
        files_finalizing_total: 0,
        bytes_finalized: 0,
    }
}

/// `POST /api/remote/fetch` — copy `path` into `dest_dir` (a new temp folder by default) as a job.
pub(crate) async fn start_fetch(r: Arc<Remote>, deps: FetchDeps, body: FetchBody) -> Response {
    let p = match super::path::parse(&body.path) {
        Ok(p) => p,
        Err(e) => return super::api::remote_err(&e),
    };
    let listed: Result<Listed, RemoteError> = async {
        r.pool
            .with_fs(&r.store, &p.connection_id, |fs| {
                let path = p.path.clone();
                async move {
                    let top = fs.stat(&path).await?;
                    if top.is_dir {
                        let files = fs.walk(&path, MAX_FILES).await?;
                        Ok((fs, files, true, Vec::new()))
                    } else {
                        // A RAR set's first volume brings the rest of the set with it.
                        let name = path.rsplit('/').next().unwrap_or("").to_string();
                        let mut siblings = Vec::new();
                        if name.to_ascii_lowercase().ends_with(".rar") {
                            let dir = match path.rfind('/') {
                                Some(0) | None => "/".to_string(),
                                Some(i) => path[..i].to_string(),
                            };
                            let mut entries = Vec::new();
                            let mut cursor = None;
                            loop {
                                let page = fs.list(&dir, cursor).await?;
                                entries.extend(page.entries);
                                match page.next_cursor {
                                    Some(c) => cursor = Some(c),
                                    None => break,
                                }
                            }
                            let names: Vec<String> = entries
                                .iter()
                                .filter(|e| !e.is_dir)
                                .map(|e| e.name.clone())
                                .collect();
                            for sib in rar_set_siblings(&name, &names) {
                                if let Some(e) = entries.iter().find(|e| e.name == sib) {
                                    siblings.push((super::path::join(&dir, &sib)?, e.clone()));
                                }
                            }
                        }
                        Ok((fs, vec![(String::new(), top)], false, siblings))
                    }
                }
            })
            .await
    }
    .await;
    let (fs, files, is_dir, siblings) = match listed {
        Ok(v) => v,
        Err(e) => return super::api::remote_err(&e),
    };
    if files.len() >= MAX_FILES {
        return err(
            StatusCode::BAD_REQUEST,
            format!("That folder has more than {MAX_FILES} files."),
        );
    }
    // The sizes come from the server: a sum that overflows is refused, not wrapped.
    let Some(total) = total_size(files.iter().chain(siblings.iter()).map(|(_, e)| e.size)) else {
        return err(
            StatusCode::BAD_REQUEST,
            "The server reports sizes too large to add up.",
        );
    };

    let (dest_dir, made_dir) = match body.dest_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(d) => (crate::fpkg_api::resolve_engine_path(d), false),
        None => (default_copy_root().join(Uuid::new_v4().to_string()), true),
    };
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        return err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", dest_dir.display()),
        );
    }
    if let Some(free) = (deps.free_bytes)(&dest_dir) {
        if free < total.saturating_add(HEADROOM) {
            if made_dir {
                let _ = std::fs::remove_dir_all(&dest_dir);
            }
            return err(
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "Not enough space: needs {}, {} free.",
                    ps5upload_core::units::iec_bytes(total.saturating_add(HEADROOM)),
                    ps5upload_core::units::iec_bytes(free)
                ),
            );
        }
    }
    let name = p
        .path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("server")
        .to_string();
    let dest = dest_dir.join(&name);
    // The copy and a RAR set's other volumes alike: never write over (and later delete) a file
    // that was already there.
    if let Some(taken) = std::iter::once(dest.clone())
        .chain(siblings.iter().map(|(_, e)| dest_dir.join(&e.name)))
        .find(|p| p.exists())
    {
        return err(
            StatusCode::CONFLICT,
            format!("{} already exists", taken.display()),
        );
    }

    let job_id = Uuid::new_v4();
    let cancel = crate::register_transfer_cancel(job_id);
    let started_at_ms = crate::now_ms();
    crate::set_job(
        &deps.jobs,
        &deps.events_tx,
        job_id,
        running(started_at_ms, 0, total),
    );

    // A RAR set's other volumes land beside the first.
    let sibling_copies: Vec<(String, Entry, PathBuf)> = siblings
        .into_iter()
        .map(|(remote, e)| {
            let local = dest_dir.join(&e.name);
            (remote, e, local)
        })
        .collect();
    let remove_roots: Vec<PathBuf> = if made_dir {
        vec![dest_dir.clone()]
    } else {
        std::iter::once(dest.clone())
            .chain(sibling_copies.iter().map(|(_, _, l)| l.clone()))
            .collect()
    };
    let pool = Arc::clone(&r.pool);
    let store = Arc::clone(&r.store);
    let id = p.connection_id.clone();
    let base = p.path.clone();
    tokio::spawn(async move {
        let result = copy_all(CopyJob {
            _fs: fs,
            pool,
            store,
            id,
            base,
            is_dir,
            files,
            siblings: sibling_copies,
            dest: dest.clone(),
            cancel,
            backoff: deps.backoff,
            report: &|done| {
                crate::set_job(
                    &deps.jobs,
                    &deps.events_tx,
                    job_id,
                    running(started_at_ms, done, total),
                )
            },
        })
        .await;
        let completed_at_ms = crate::now_ms();
        match result {
            Ok(file_count) => {
                fetched()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(dest.clone(), remove_roots);
                crate::set_job(
                    &deps.jobs,
                    &deps.events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: String::new(),
                        bytes_sent: total,
                        dest: dest.display().to_string(),
                        files_sent: file_count,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(e) => {
                // A single file dies as `<name>.partial`; a folder takes its partials with it.
                // Every handle `copy_all` opened is closed by now; the removal still retries,
                // since Windows can keep a just-closed file locked for a moment.
                let roots = remove_roots.clone();
                let _ = tokio::task::spawn_blocking(move || remove_copies(&roots)).await;
                crate::set_job(
                    &deps.jobs,
                    &deps.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: e,
                        error_reason: None,
                        error_detail: None,
                        error_console: None,
                    },
                );
            }
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job_id": job_id.to_string() })),
    )
        .into_response()
}

struct CopyJob<'a> {
    /// Held for the job's lifetime so the pooled session it was listed on stays open.
    _fs: Arc<dyn RemoteFs>,
    pool: Arc<super::pool::Pool>,
    store: Arc<super::store::Store>,
    id: String,
    base: String,
    is_dir: bool,
    files: Vec<(String, Entry)>,
    /// Extra single files copied after `files`: (server path, entry, local path).
    siblings: Vec<(String, Entry, PathBuf)>,
    dest: PathBuf,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    backoff: super::pool::Backoff,
    report: &'a (dyn Fn(u64) + Send + Sync),
}

/// `dest` + a relative path from a server listing, refusing anything that would land outside
/// `dest`: `..`, an absolute path, a drive prefix, or a backslash (a separator on Windows).
fn safe_join(dest: &Path, rel: &str) -> Result<PathBuf, String> {
    use std::path::Component;
    let unsafe_name =
        || format!("The server listed an unsafe name, {rel:?}; nothing was copied past it.");
    if rel.is_empty() || rel.contains('\\') || rel.contains('\0') {
        return Err(unsafe_name());
    }
    let mut out = dest.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(part) => out.push(part),
            _ => return Err(unsafe_name()),
        }
    }
    Ok(out)
}

/// Copy every file, each to `<name>.partial` then renamed. Returns the file count.
async fn copy_all(job: CopyJob<'_>) -> Result<u64, String> {
    let mut done = 0u64;
    let mut work: Vec<(String, PathBuf, &Entry)> = Vec::new();
    for (rel, entry) in &job.files {
        let (remote, local) = if job.is_dir {
            (
                format!("{}/{rel}", job.base.trim_end_matches('/')),
                safe_join(&job.dest, rel)?,
            )
        } else {
            (job.base.clone(), job.dest.clone())
        };
        work.push((remote, local, entry));
    }
    for (remote, entry, local) in &job.siblings {
        work.push((remote.clone(), local.clone(), entry));
    }
    for (remote, local, entry) in work {
        let file = job
            .pool
            .with_fs(&job.store, &job.id, |fs| {
                let remote = remote.clone();
                async move { fs.open(&remote).await }
            })
            .await
            .map_err(|e| e.to_string())?;
        let file = super::pool::retrying(
            Arc::clone(&job.pool),
            Arc::clone(&job.store),
            job.id.clone(),
            remote.clone(),
            file,
            job.backoff.clone(),
        );
        let partial = PathBuf::from(format!("{}.partial", local.display()));
        // Local disk work runs on the blocking pool, never on the async workers.
        let mut out = {
            let (local, partial) = (local.clone(), partial.clone());
            blocking(move || {
                if let Some(parent) = local.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                std::fs::File::create(&partial).map_err(|e| format!("{}: {e}", partial.display()))
            })
            .await?
        };
        let mut off = 0u64;
        while off < entry.size {
            if job.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let len = CHUNK.min(entry.size - off);
            let bytes = file
                .read_at(off, len)
                .await
                .map_err(|e| format!("Lost the connection while copying {}: {e}", entry.name))?;
            if bytes.is_empty() {
                return Err(format!("{} ended early on the server", entry.name));
            }
            let n = bytes.len() as u64;
            out = {
                let partial = partial.clone();
                blocking(move || {
                    use std::io::Write;
                    out.write_all(&bytes)
                        .map(|()| out)
                        .map_err(|e| format!("{}: {e}", partial.display()))
                })
                .await?
            };
            off += n;
            done += n;
            (job.report)(done);
        }
        // A copy that did not reach the disk is not a copy: a failed sync fails the job.
        blocking(move || {
            out.sync_all()
                .map_err(|e| format!("{}: {e}", partial.display()))?;
            drop(out);
            std::fs::rename(&partial, &local).map_err(|e| format!("{}: {e}", local.display()))
        })
        .await?;
    }
    Ok((job.files.len() + job.siblings.len()) as u64)
}

/// Runs `f` on the blocking pool; a panic there is an error, not a crash of the job's task.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("disk task failed: {e}"))?
}

/// `POST /api/remote/fetch/cleanup` — remove a copy this engine made. Nothing else.
pub(crate) async fn cleanup(dest: &str) -> Response {
    let key = PathBuf::from(dest);
    let root = fetched()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    let Some(roots) = root else {
        return err(StatusCode::BAD_REQUEST, "That is not a copy this app made.");
    };
    let left = tokio::task::spawn_blocking(move || {
        let failed = remove_copies(&roots);
        // What could not be removed (a file still locked on Windows) stays this copy's, so
        // another cleanup can finish it instead of refusing it as unknown.
        if !failed.is_empty() {
            let paths = failed.iter().map(|(p, _)| p.clone()).collect();
            fetched()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, paths);
        }
        failed
    })
    .await;
    match left {
        Ok(failed) if failed.is_empty() => Json(json!({ "ok": true })).into_response(),
        Ok(failed) => {
            let (root, e) = &failed[0];
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{}: {e}", root.display()),
            )
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("disk task failed: {e}"),
        ),
    }
}

/// Remove each copy root (and a single file's `<name>.partial`), retrying past a short-lived
/// lock. Returns the roots that could not be removed, with why.
fn remove_copies(roots: &[PathBuf]) -> Vec<(PathBuf, std::io::Error)> {
    use ps5upload_core::archive_extract::remove_path_retrying;
    let mut failed = Vec::new();
    for root in roots {
        let mut partial = root.clone().into_os_string();
        partial.push(".partial");
        let _ = remove_path_retrying(Path::new(&partial));
        if let Err(e) = remove_path_retrying(root) {
            failed.push((root.clone(), e));
        }
    }
    failed
}

/// Folder the engine copies server files into when the caller names none.
fn default_copy_root() -> PathBuf {
    std::env::temp_dir().join("ps5upload-remote")
}

/// Copies older than this are left from an earlier run when the engine starts.
const STALE_COPY_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Remove copies a previous run left in the default copy folder: an extract that failed and the
/// app closed before its copy was cleaned up, or a crash. No copy survives a restart in
/// `fetched()`, so nothing could remove them later; only folders untouched for an hour go, so
/// another engine's copy in progress is safe.
pub fn sweep_stale_copies() {
    sweep_stale_copies_in(&default_copy_root(), STALE_COPY_AGE);
}

fn sweep_stale_copies_in(root: &Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= max_age);
        if old && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = ps5upload_core::archive_extract::remove_path_retrying(&e.path());
        }
    }
}

/// `POST /api/remote/inspect-folder` — what `/api/local/inspect-folder` says, for a folder on a
/// server: only its metadata files are copied (into a local mirror), and the size and file count
/// come from the server's own listing.
pub(crate) async fn inspect_folder(r: &Remote, remote: &str) -> Response {
    let result: Result<serde_json::Value, String> = async {
        let p = super::path::parse(remote).map_err(|e| e.to_string())?;
        let fs = r
            .pool
            .fs(&r.store, &p.connection_id)
            .await
            .map_err(|e| e.to_string())?;
        let name = p
            .path
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("game")
            .to_string();
        let mirror_root = std::env::temp_dir()
            .join("ps5upload-inspect")
            .join(&blake3::hash(remote.as_bytes()).to_hex().as_str()[..16]);
        let mirror = mirror_root.join(&name);
        let _ = std::fs::remove_dir_all(&mirror);
        std::fs::create_dir_all(mirror.join("sce_sys")).map_err(|e| e.to_string())?;
        for meta in ["param.json", "param.sfo", "icon0.png"] {
            let src = format!("{}/sce_sys/{meta}", p.path.trim_end_matches('/'));
            let Ok(file) = fs.open(&src).await else {
                continue;
            };
            let bytes = file
                .read_at(0, file.size().min(16 * 1024 * 1024))
                .await
                .map_err(|e| e.to_string())?;
            std::fs::write(mirror.join("sce_sys").join(meta), bytes).map_err(|e| e.to_string())?;
        }
        let walked = fs
            .walk(&p.path, MAX_FILES)
            .await
            .map_err(|e| e.to_string())?;
        let at = mirror.clone();
        let mut inspected =
            tokio::task::spawn_blocking(move || ps5upload_core::game_meta::inspect_folder(&at))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("{e:#}"))?;
        inspected.path = remote.to_string();
        inspected.total_size = walked.iter().map(|(_, e)| e.size).sum();
        inspected.file_count = walked.len() as u64;
        inspected.skipped_paths = Vec::new();
        Ok(json!({ "ok": true, "result": inspected, "wrapped_hint": null }))
    }
    .await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

/// The sum of `sizes`, or `None` when it overflows.
fn total_size(sizes: impl IntoIterator<Item = u64>) -> Option<u64> {
    sizes.into_iter().try_fold(0u64, u64::checked_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::pool::testing::remote_with;

    #[test]
    fn sizes_that_overflow_are_refused_not_wrapped() {
        assert_eq!(total_size([1, 2, 3]), Some(6));
        assert_eq!(total_size([u64::MAX, 1]), None);
        assert_eq!(total_size([]), Some(0));
    }
    use crate::remote::store::{conn, Protocol, Secret};
    use crate::remote::MemFs;

    async fn json_of(resp: Response) -> (StatusCode, serde_json::Value) {
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            code,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn deps(free: fn(&Path) -> Option<u64>) -> (FetchDeps, Jobs) {
        let jobs: Jobs = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel(64);
        (
            FetchDeps {
                jobs: Arc::clone(&jobs),
                events_tx,
                free_bytes: free,
                backoff: crate::remote::pool::Backoff::instant(),
            },
            jobs,
        )
    }

    fn plenty(_: &Path) -> Option<u64> {
        Some(u64::MAX / 2)
    }
    fn tiny(_: &Path) -> Option<u64> {
        Some(1024)
    }

    async fn wait(jobs: &Jobs, id: Uuid) -> JobState {
        for _ in 0..500 {
            let st = jobs.lock().unwrap().get(&id).cloned();
            if let Some(s @ (JobState::Done { .. } | JobState::Failed { .. })) = st {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("job never finished");
    }

    fn setup(files: &[(&str, &[u8])]) -> (Arc<Remote>, String) {
        let r = remote_with(MemFs::new(files), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        (r, id)
    }

    #[tokio::test]
    async fn copies_a_folder_byte_for_byte_and_cleans_it_up() {
        let big: Vec<u8> = (0..(9 * 1024 * 1024)).map(|i| (i % 249) as u8).collect();
        let (r, id) = setup(&[("/g/eboot.bin", &big), ("/g/sce_sys/param.json", b"{}")]);
        let (d, jobs) = deps(plenty);
        let dest_dir = crate::remote::store::test_dir();
        let (code, out) = json_of(
            start_fetch(
                Arc::clone(&r),
                d,
                FetchBody {
                    path: format!("remote://{id}/g"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        assert_eq!(code, StatusCode::ACCEPTED, "{out}");
        let job: Uuid = out["job_id"].as_str().unwrap().parse().unwrap();
        let JobState::Done {
            dest, bytes_sent, ..
        } = wait(&jobs, job).await
        else {
            panic!("fetch failed")
        };
        assert_eq!(PathBuf::from(&dest), dest_dir.join("g"));
        assert_eq!(bytes_sent, big.len() as u64 + 2);
        assert!(std::fs::read(dest_dir.join("g/eboot.bin")).unwrap() == big);
        assert_eq!(
            std::fs::read(dest_dir.join("g/sce_sys/param.json")).unwrap(),
            b"{}"
        );
        assert!(!dest_dir.join("g/eboot.bin.partial").exists());

        let (code, _) = json_of(cleanup("/etc").await).await;
        assert_eq!(
            code,
            StatusCode::BAD_REQUEST,
            "only fetched copies can be removed"
        );
        let (code, _) = json_of(cleanup(&dest).await).await;
        assert_eq!(code, StatusCode::OK);
        assert!(!dest_dir.join("g").exists());
    }

    #[tokio::test]
    async fn refuses_before_copying_when_the_disk_is_too_small() {
        let (r, id) = setup(&[("/a.pkg", &[1u8; 4096])]);
        let (d, _jobs) = deps(tiny);
        let dest_dir = crate::remote::store::test_dir();
        let (code, out) = json_of(
            start_fetch(
                r,
                d,
                FetchBody {
                    path: format!("remote://{id}/a.pkg"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        assert_eq!(code, StatusCode::INSUFFICIENT_STORAGE);
        assert!(out["error"].as_str().unwrap().contains("Not enough space"));
        assert!(!dest_dir.join("a.pkg").exists());
    }

    #[tokio::test]
    async fn a_failed_copy_leaves_nothing_behind() {
        let mem = Arc::new(MemFs::new(&[("/a.pkg", &[1u8; 100])]));
        let r = crate::remote::pool::testing::remote_with_shared(Arc::clone(&mem), None);
        let id = r
            .store
            .add(conn("NAS", Protocol::Smb), Secret::None)
            .unwrap()
            .conn
            .id;
        mem.fail_next_reads(100);
        let (d, jobs) = deps(plenty);
        let dest_dir = crate::remote::store::test_dir();
        let (_, out) = json_of(
            start_fetch(
                r,
                d,
                FetchBody {
                    path: format!("remote://{id}/a.pkg"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        let job: Uuid = out["job_id"].as_str().unwrap().parse().unwrap();
        assert!(matches!(wait(&jobs, job).await, JobState::Failed { .. }));
        assert_eq!(std::fs::read_dir(&dest_dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn inspects_a_remote_game_folder_from_its_metadata() {
        let param = br#"{"titleId":"PPSA01234","contentId":"UP0000-PPSA01234_00-GAME000000000000","localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Test Game"}}}"#;
        let (r, id) = setup(&[
            ("/g/sce_sys/param.json", param),
            ("/g/eboot.bin", &[0u8; 1000]),
        ]);
        let (code, out) = json_of(inspect_folder(&r, &format!("remote://{id}/g")).await).await;
        assert_eq!(code, StatusCode::OK, "{out}");
        assert_eq!(out["ok"], true);
        assert_eq!(out["result"]["title_id"], "PPSA01234");
        assert_eq!(out["result"]["meta_source"], "param.json");
        assert_eq!(out["result"]["file_count"], 2);
        assert_eq!(out["result"]["path"], format!("remote://{id}/g"));
    }

    #[test]
    fn safe_join_keeps_everything_inside() {
        let d = Path::new("/out");
        assert_eq!(safe_join(d, "a/b.bin").unwrap(), Path::new("/out/a/b.bin"));
        for bad in ["../x", "a/../../x", "/etc/x", "a\\..\\x", "", "./../x"] {
            assert!(safe_join(d, bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_hostile_listing_cannot_write_outside_the_destination() {
        // A server that lists "../escape" (or an absolute name) inside the folder being copied.
        let (r, id) = setup(&[("/g/ok.bin", b"fine"), ("/g/../escape", b"owned")]);
        let (d, jobs) = deps(plenty);
        let dest_dir = crate::remote::store::test_dir().join("inner");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let (_, out) = json_of(
            start_fetch(
                r,
                d,
                FetchBody {
                    path: format!("remote://{id}/g"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        let job: Uuid = out["job_id"].as_str().unwrap().parse().unwrap();
        let end = wait(&jobs, job).await;
        assert!(
            matches!(end, JobState::Failed { .. }),
            "an unsafe name must fail the copy"
        );
        assert!(!dest_dir.join("escape").exists());
        assert!(
            !dest_dir.parent().unwrap().join("escape").exists(),
            "wrote outside the destination"
        );
    }

    #[test]
    fn a_first_rar_volume_names_its_set() {
        let names: Vec<String> = [
            "g.part1.rar",
            "g.part2.rar",
            "g.part10.rar",
            "g.part2.rar.txt",
            "h.part2.rar",
            "old.rar",
            "old.r00",
            "old.r01",
            "old.r0x",
            "x.zip",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            rar_set_siblings("g.part1.rar", &names),
            ["g.part2.rar", "g.part10.rar"]
        );
        assert_eq!(
            rar_set_siblings("G.PART01.RAR", &["G.PART02.RAR".into()]),
            ["G.PART02.RAR"]
        );
        assert_eq!(rar_set_siblings("old.rar", &names), ["old.r00", "old.r01"]);
        // Not a first volume, or not a set: nothing extra.
        assert!(rar_set_siblings("g.part2.rar", &names).is_empty());
        assert!(rar_set_siblings("x.zip", &names).is_empty());
    }

    /// The reported set: `.rar`, `.r00`…`.r99`, `.s00`…`.s99`, `.t00`…. Only the `.rNN` third
    /// used to be copied, so UnRAR stopped at the missing `.s00`.
    #[test]
    fn an_old_style_set_brings_every_letter_in_order() {
        let mut names: Vec<String> = vec!["Game.rar".into(), "Other.s00".into(), "Game.q00".into()];
        // Shuffled on purpose: the listing order must not matter.
        for i in (0..250).rev() {
            names.push(
                ps5upload_core::archive_extract::rar_old_volume_name("Game", i, false).unwrap(),
            );
        }
        let got = rar_set_siblings("Game.rar", &names);
        assert_eq!(got.len(), 250);
        assert_eq!(got[0], "Game.r00");
        assert_eq!(got[99], "Game.r99");
        assert_eq!(got[100], "Game.s00");
        assert_eq!(got[199], "Game.s99");
        assert_eq!(got[200], "Game.t00");
        assert_eq!(got[249], "Game.t49");
        // The stem matches without regard to case, as on the disks these sets come from.
        assert_eq!(
            rar_set_siblings("game.RAR", &["GAME.R00".into(), "game.S00".into()]),
            ["GAME.R00", "game.S00"]
        );
    }

    #[tokio::test]
    async fn a_multi_part_rar_arrives_with_every_volume_and_leaves_with_them() {
        let (r, id) = setup(&[
            ("/dl/g.part1.rar", b"one"),
            ("/dl/g.part2.rar", b"two"),
            ("/dl/g.part3.rar", b"three"),
            ("/dl/other.rar", b"x"),
        ]);
        let (d, jobs) = deps(plenty);
        let dest_dir = crate::remote::store::test_dir();
        let (code, out) = json_of(
            start_fetch(
                Arc::clone(&r),
                d,
                FetchBody {
                    path: format!("remote://{id}/dl/g.part1.rar"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        assert_eq!(code, StatusCode::ACCEPTED, "{out}");
        let job: Uuid = out["job_id"].as_str().unwrap().parse().unwrap();
        let JobState::Done {
            dest, bytes_sent, ..
        } = wait(&jobs, job).await
        else {
            panic!("fetch failed")
        };
        assert_eq!(PathBuf::from(&dest), dest_dir.join("g.part1.rar"));
        assert_eq!(bytes_sent, 11);
        assert_eq!(
            std::fs::read(dest_dir.join("g.part3.rar")).unwrap(),
            b"three"
        );
        assert!(!dest_dir.join("other.rar").exists());
        let (code, _) = json_of(cleanup(&dest).await).await;
        assert_eq!(code, StatusCode::OK);
        for v in ["g.part1.rar", "g.part2.rar", "g.part3.rar"] {
            assert!(!dest_dir.join(v).exists(), "{v}");
        }
    }

    /// An old-style set past `.r99` (`.s00`, `.t00` …) arrives whole, and leaves whole.
    #[tokio::test]
    async fn an_old_style_rar_set_arrives_with_its_s_and_t_volumes() {
        let (r, id) = setup(&[
            ("/dl/g.rar", b"0"),
            ("/dl/g.r00", b"1"),
            ("/dl/g.r99", b"2"),
            ("/dl/g.s00", b"3"),
            ("/dl/g.s99", b"4"),
            ("/dl/g.t00", b"5"),
        ]);
        let (d, jobs) = deps(plenty);
        let dest_dir = crate::remote::store::test_dir();
        let (code, out) = json_of(
            start_fetch(
                Arc::clone(&r),
                d,
                FetchBody {
                    path: format!("remote://{id}/dl/g.rar"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        assert_eq!(code, StatusCode::ACCEPTED, "{out}");
        let job: Uuid = out["job_id"].as_str().unwrap().parse().unwrap();
        let JobState::Done {
            dest, bytes_sent, ..
        } = wait(&jobs, job).await
        else {
            panic!("fetch failed")
        };
        assert_eq!(bytes_sent, 6);
        for v in ["g.rar", "g.r00", "g.r99", "g.s00", "g.s99", "g.t00"] {
            assert!(dest_dir.join(v).exists(), "{v} was not copied");
        }
        let (code, _) = json_of(cleanup(&dest).await).await;
        assert_eq!(code, StatusCode::OK);
        for v in ["g.rar", "g.r00", "g.r99", "g.s00", "g.s99", "g.t00"] {
            assert!(!dest_dir.join(v).exists(), "{v} was left behind");
        }
    }

    /// Copies a previous run left in the copy folder go when the engine starts; a fresh one
    /// (another engine's, still copying) stays.
    #[test]
    fn stale_copies_are_swept_and_fresh_ones_kept() {
        let root = crate::remote::store::test_dir();
        let old = root.join("old-copy");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("g.r00"), b"x").unwrap();
        sweep_stale_copies_in(&root, std::time::Duration::from_secs(3600));
        assert!(old.exists(), "a fresh copy was removed");
        sweep_stale_copies_in(&root, std::time::Duration::ZERO);
        assert!(!old.exists(), "a stale copy was left behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_copies_takes_a_single_files_partial_with_it() {
        let dir = crate::remote::store::test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("g.rar");
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(dir.join("g.s00.partial"), b"y").unwrap();
        let failed = remove_copies(&[file.clone(), dir.join("g.s00")]);
        assert!(failed.is_empty());
        assert!(!file.exists() && !dir.join("g.s00.partial").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_rar_volume_already_in_the_folder_is_never_overwritten() {
        let (r, id) = setup(&[("/dl/g.part1.rar", b"one"), ("/dl/g.part2.rar", b"two")]);
        let (d, _jobs) = deps(plenty);
        let dest_dir = crate::remote::store::test_dir();
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(dest_dir.join("g.part2.rar"), b"mine").unwrap();
        let (code, out) = json_of(
            start_fetch(
                Arc::clone(&r),
                d,
                FetchBody {
                    path: format!("remote://{id}/dl/g.part1.rar"),
                    dest_dir: Some(dest_dir.display().to_string()),
                },
            )
            .await,
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{out}");
        assert_eq!(
            std::fs::read(dest_dir.join("g.part2.rar")).unwrap(),
            b"mine"
        );
        assert!(!dest_dir.join("g.part1.rar").exists());
    }
}
