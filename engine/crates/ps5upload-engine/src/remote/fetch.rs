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
type Listed = (Arc<dyn RemoteFs>, Vec<(String, Entry)>, bool);

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

/// Copies this engine made, and what to delete to remove each: the copy itself, or the temp
/// folder the engine created for it. Only these can be removed through `cleanup`.
fn fetched() -> &'static Mutex<HashMap<PathBuf, PathBuf>> {
    static F: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(HashMap::new()))
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

fn gib(n: u64) -> String {
    format!("{:.1} GB", n as f64 / 1e9)
}

fn running(started_at_ms: u64, bytes_sent: u64, total_bytes: u64) -> JobState {
    JobState::Running {
        stage: None,
        started_at_ms,
        bytes_sent,
        total_bytes,
        files: Vec::new(),
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
        let fs = r.pool.fs(&r.store, &p.connection_id).await?;
        let top = fs.stat(&p.path).await?;
        if top.is_dir {
            let files = fs.walk(&p.path, MAX_FILES).await?;
            Ok((fs, files, true))
        } else {
            Ok((fs, vec![(String::new(), top)], false))
        }
    }
    .await;
    let (fs, files, is_dir) = match listed {
        Ok(v) => v,
        Err(e) => return super::api::remote_err(&e),
    };
    if files.len() >= MAX_FILES {
        return err(
            StatusCode::BAD_REQUEST,
            format!("That folder has more than {MAX_FILES} files."),
        );
    }
    let total: u64 = files.iter().map(|(_, e)| e.size).sum();

    let (dest_dir, made_dir) = match body.dest_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(d) => (crate::fpkg_api::resolve_engine_path(d), false),
        None => (
            std::env::temp_dir()
                .join("ps5upload-remote")
                .join(Uuid::new_v4().to_string()),
            true,
        ),
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
                    gib(total + HEADROOM),
                    gib(free)
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
    if dest.exists() {
        return err(
            StatusCode::CONFLICT,
            format!("{} already exists", dest.display()),
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

    let remove_root = if made_dir {
        dest_dir.clone()
    } else {
        dest.clone()
    };
    let pool = Arc::clone(&r.pool);
    let store = Arc::clone(&r.store);
    let id = p.connection_id.clone();
    let base = p.path.clone();
    tokio::spawn(async move {
        let result = copy_all(CopyJob {
            fs,
            pool,
            store,
            id,
            base,
            is_dir,
            files,
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
                    .insert(dest.clone(), remove_root);
                crate::set_job(
                    &deps.jobs,
                    &deps.events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: String::new(),
                        shards_sent: 0,
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
                let _ = std::fs::remove_file(format!("{}.partial", dest.display()));
                let _ = if remove_root.is_dir() {
                    std::fs::remove_dir_all(&remove_root)
                } else {
                    std::fs::remove_file(&remove_root)
                };
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
    fs: Arc<dyn RemoteFs>,
    pool: Arc<super::pool::Pool>,
    store: Arc<super::store::Store>,
    id: String,
    base: String,
    is_dir: bool,
    files: Vec<(String, Entry)>,
    dest: PathBuf,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    backoff: super::pool::Backoff,
    report: &'a (dyn Fn(u64) + Send + Sync),
}

/// Copy every file, each to `<name>.partial` then renamed. Returns the file count.
async fn copy_all(job: CopyJob<'_>) -> Result<u64, String> {
    use std::io::Write;
    let mut done = 0u64;
    for (rel, entry) in &job.files {
        let (remote, local) = if job.is_dir {
            (
                format!("{}/{rel}", job.base.trim_end_matches('/')),
                job.dest.join(rel),
            )
        } else {
            (job.base.clone(), job.dest.clone())
        };
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let file = job.fs.open(&remote).await.map_err(|e| e.to_string())?;
        let file = super::pool::retrying(
            Arc::clone(&job.pool),
            Arc::clone(&job.store),
            job.id.clone(),
            remote.clone(),
            file,
            job.backoff.clone(),
        );
        let partial = PathBuf::from(format!("{}.partial", local.display()));
        let mut out =
            std::fs::File::create(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
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
            out.write_all(&bytes)
                .map_err(|e| format!("{}: {e}", partial.display()))?;
            off += bytes.len() as u64;
            done += bytes.len() as u64;
            (job.report)(done);
        }
        out.sync_all().ok();
        drop(out);
        std::fs::rename(&partial, &local).map_err(|e| format!("{}: {e}", local.display()))?;
    }
    Ok(job.files.len() as u64)
}

/// `POST /api/remote/fetch/cleanup` — remove a copy this engine made. Nothing else.
pub(crate) async fn cleanup(dest: &str) -> Response {
    let key = PathBuf::from(dest);
    let root = fetched()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    let Some(root) = root else {
        return err(StatusCode::BAD_REQUEST, "That is not a copy this app made.");
    };
    let r = if root.is_dir() {
        std::fs::remove_dir_all(&root)
    } else {
        std::fs::remove_file(&root)
    };
    match r {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", root.display()),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::pool::testing::remote_with;
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
}
