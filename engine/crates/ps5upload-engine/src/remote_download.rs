//! Download a package from a link to local disk, then hand the file to the
//! ordinary local-file install path.
//!
//! This is the third of the three ways a link can become an installed game,
//! and the only one that finishes the transfer before the console is involved
//! at all:
//!
//! 1. the console fetches the link itself (`dpi-install` with a URL),
//! 2. the engine streams it through RAM as the console asks for it
//!    (`remote_url` + `serve_only`, see [`crate::remote_pkg`]),
//! 3. *this*: the engine downloads the whole package to disk first.
//!
//! Why it has to exist. (2) holds the link open for the entire install, so a
//! link that expires, a laptop that sleeps, or an origin that drops long
//! connections takes the install down with it — and the console sits idle
//! whenever the origin stalls. Downloading first decouples the two legs: the
//! slow, failure-prone part finishes on its own, can be retried without the
//! console, and what reaches the console is a local file that installs at LAN
//! speed. It costs disk space the other two modes do not.
//!
//! The download reuses [`crate::remote_pkg::RemoteSource`], so it gets the
//! same parallel ranged fetch, adaptive connection count and stall handling
//! as streaming — the difference is only where the bytes land.

use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// Bytes pulled from the origin per write. Matches the fetcher's window so a
/// download is one window fetch followed by one sequential write.
const COPY_WINDOW: u64 = 32 * 1024 * 1024;

/// Where a link-download goes when the caller does not say. Downloads is
/// where a 100 GiB file is easiest to find — and to delete — afterwards.
fn default_download_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PS5UPLOAD_LINK_DOWNLOAD_DIR") {
        return PathBuf::from(dir);
    }
    // Desktop: ~/Downloads/ps5upload. No home folder (a phone): the engine's data folder,
    // which the app points at its private storage (#379); temp only as a last resort.
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
    {
        return PathBuf::from(home).join("Downloads").join("ps5upload");
    }
    crate::remote::store::data_dir()
        .map(|d| d.join("downloads"))
        .unwrap_or_else(|| std::env::temp_dir().join("ps5upload"))
}

/// A name that is safe as one file in the download directory. The name comes from a URL, so it is
/// attacker-influenced in the sense that a hostile link could carry `../../` or a NUL; it must never
/// escape the directory. Only what is unsafe goes: `ゲーム.part1.rar` must keep its stem, which is
/// how the parts of a set find each other, and two differently named packages must not collapse
/// into one name (the second would then read as already downloaded).
fn safe_file_name(raw: &str) -> String {
    let name = crate::link::sanitize_name(raw);
    // A trailing dot or space is dropped by Windows, naming another file.
    let mut name = name.trim_end_matches(['.', ' ']).to_string();
    // `CON.pkg` is a device there, whatever follows the dot.
    if ps5upload_core::transfer::host_unsafe_component(&name).is_some() {
        name.insert(0, '_');
    }
    if name.is_empty() {
        "package.pkg".to_string()
    } else {
        name
    }
}

#[derive(Debug, Default)]
pub struct Download {
    pub url_host: String,
    pub path: PathBuf,
    /// Where the bytes land until the download is complete (`<name>.partial`); renamed to
    /// `path` at the end, so a killed download never leaves a short file under the real name.
    pub partial: PathBuf,
    pub total: u64,
    pub written: AtomicU64,
    pub done: AtomicBool,
    pub cancelled: AtomicBool,
    pub error: Mutex<Option<String>>,
    /// True while a worker writes the partial. A cancelled download stays live until its
    /// worker has exited: the worker, not the cancel request, removes its own partial, so a
    /// new download of the same file never has its partial deleted under it.
    pub worker_running: Mutex<bool>,
    /// When the download stopped (finished, failed or cancelled); the registry forgets
    /// entries some time after this.
    pub ended: Mutex<Option<std::time::Instant>>,
}

impl Download {
    fn mark_ended(&self) {
        *self.ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }
}

/// How long a stopped download's status stays readable.
const ENDED_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// At most this many stopped downloads are remembered.
const ENDED_CAP: usize = 64;

/// What a `.partial` was downloaded from, written beside it as `<name>.partial.json`. A
/// partial is resumed only when this matches the link being downloaded now. The URL's query
/// is left out: signed links carry credentials there and change on every request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PartialMeta {
    host: String,
    path: String,
    total: u64,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

impl PartialMeta {
    /// Same origin path and size, and validators that agree wherever both sides have one.
    fn same_file(&self, now: &PartialMeta) -> bool {
        let agree = |a: &Option<String>, b: &Option<String>| match (a, b) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        self.host == now.host
            && self.path == now.path
            && self.total == now.total
            && agree(&self.etag, &now.etag)
            && agree(&self.last_modified, &now.last_modified)
    }
}

fn sidecar_path(partial: &std::path::Path) -> PathBuf {
    let mut name = partial.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    partial.with_file_name(name)
}

fn read_sidecar(partial: &std::path::Path) -> Option<PartialMeta> {
    serde_json::from_slice(&std::fs::read(sidecar_path(partial)).ok()?).ok()
}

fn write_sidecar(partial: &std::path::Path, meta: &PartialMeta) -> std::io::Result<()> {
    std::fs::write(
        sidecar_path(partial),
        serde_json::to_vec(meta).unwrap_or_default(),
    )
}

/// Removes a partial and its sidecar (either may be missing).
fn remove_partial(partial: &std::path::Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(sidecar_path(partial));
    match std::fs::remove_file(partial) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// The worker is done with the partial. Under the same lock the cancel request takes, so
/// exactly one of them removes a cancelled download's partial.
fn finish_worker(dl: &Download) {
    let mut running = dl.worker_running.lock().unwrap_or_else(|e| e.into_inner());
    *running = false;
    if dl.cancelled.load(Ordering::Relaxed) && !dl.done.load(Ordering::Relaxed) {
        match remove_partial(&dl.partial) {
            Ok(()) => crate::log_info!("link-download cancelled: removed the partial file"),
            Err(e) => crate::log_warn!("link-download cancelled: could not remove partial: {e}"),
        }
    }
    dl.mark_ended();
}

#[derive(Default)]
pub struct DownloadRegistry {
    pub items: Mutex<std::collections::HashMap<String, Arc<Download>>>,
}

pub type DownloadStateHandle = Arc<DownloadRegistry>;

#[derive(Debug, Deserialize)]
pub struct DownloadStartRequest {
    pub url: String,
    /// Skip TLS verification for THIS download. Same meaning as the streaming
    /// path: it governs only what this computer accepts, never the console.
    #[serde(default)]
    pub insecure_tls: bool,
    /// Directory to download into. Defaults to `~/Downloads/ps5upload`.
    #[serde(default)]
    pub dest_dir: Option<String>,
    /// Keep the name the link gives the file. Off (the default) for a package, whose local
    /// file must end in `.pkg` to be installed; on for an archive part, whose name is how the
    /// parts of a set find each other (`game.part2.rar`).
    #[serde(default)]
    pub keep_name: bool,
    /// What to do with a different file already under the final name (the 409 below):
    /// `resume` treats it as an earlier unfinished download of this link (it becomes the
    /// `.partial`, whose tail is checked against the link before any byte is kept),
    /// `replace` deletes it and downloads afresh. Absent: refuse, as before.
    #[serde(default)]
    pub existing: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DownloadStartResponse {
    pub download_id: String,
    pub path: String,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub struct DownloadStatus {
    pub download_id: String,
    pub path: String,
    pub total: u64,
    pub written: u64,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DownloadIdQuery {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct DownloadCancelRequest {
    pub id: String,
}

/// The local file name for a download. A link that serves a package need not say so in its
/// URL (a share link, a redirect, an extensionless path), and the file the install reads back
/// must look like one, so `.pkg` is added unless the caller asked to keep the name.
fn local_name_for(probed: &str, keep_name: bool) -> String {
    let fallback = if keep_name { "download" } else { "package.pkg" };
    let mut name = safe_file_name(if probed.is_empty() { fallback } else { probed });
    if !keep_name && !name.to_ascii_lowercase().ends_with(".pkg") {
        name.push_str(".pkg");
    }
    name
}

/// The in-progress name of a download: `<name>.partial` beside it.
fn partial_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    path.with_file_name(name)
}

/// Whether a download is still writing: its worker runs (even when cancelled), or it has not
/// finished, failed or been cancelled.
fn is_live(dl: &Download) -> bool {
    if *dl.worker_running.lock().unwrap_or_else(|e| e.into_inner()) {
        return true;
    }
    !dl.done.load(Ordering::Relaxed)
        && !dl.cancelled.load(Ordering::Relaxed)
        && dl.error.lock().unwrap_or_else(|e| e.into_inner()).is_none()
}

/// Registers `dl` under `id` unless another live download writes the same file: two
/// downloads of one link would write one file. `Err` carries the live one's id.
fn register_unless_busy(
    registry: &DownloadRegistry,
    id: &str,
    dl: Arc<Download>,
) -> Result<(), String> {
    let mut items = registry.items.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut items);
    if let Some((busy, _)) = items
        .iter()
        .find(|(_, other)| other.path == dl.path && is_live(other))
    {
        return Err(busy.clone());
    }
    items.insert(id.to_string(), dl);
    Ok(())
}

/// Forgets stopped downloads older than [`ENDED_TTL`], and the oldest beyond [`ENDED_CAP`].
fn prune(items: &mut std::collections::HashMap<String, Arc<Download>>) {
    let ended = |d: &Download| *d.ended.lock().unwrap_or_else(|e| e.into_inner());
    items.retain(|_, d| ended(d).is_none_or(|t| t.elapsed() < ENDED_TTL));
    let mut stopped: Vec<(std::time::Instant, String)> = items
        .iter()
        .filter_map(|(id, d)| ended(d).map(|t| (t, id.clone())))
        .collect();
    if stopped.len() > ENDED_CAP {
        stopped.sort();
        for (_, id) in stopped.iter().take(stopped.len() - ENDED_CAP) {
            items.remove(id);
        }
    }
}

/// The id of a live download writing `path`, if there is one.
fn live_download_for(registry: &DownloadRegistry, path: &std::path::Path) -> Option<String> {
    let items = registry.items.lock().unwrap_or_else(|e| e.into_inner());
    items
        .iter()
        .find(|(_, other)| other.path == path && is_live(other))
        .map(|(id, _)| id.clone())
}

fn already_downloading(path: &std::path::Path, busy: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": format!("{} is already being downloaded.", path.display()),
            "download_id": busy,
        })),
    )
        .into_response()
}

fn json_err(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

pub fn router(state: DownloadStateHandle) -> Router {
    Router::new()
        .route("/api/pkg/remote/download/start", post(start_handler))
        .route("/api/pkg/remote/download/status", get(status_handler))
        .route("/api/pkg/remote/download/cancel", post(cancel_handler))
        .with_state(state)
}

async fn start_handler(
    State(state): State<DownloadStateHandle>,
    Json(req): Json<DownloadStartRequest>,
) -> Response {
    let url = req.url.trim().to_string();
    if url.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "url is required");
    }
    if !crate::valid_link_url(&url) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "url must be an http(s) link with no fragment or control characters",
        );
    }

    let insecure = req.insecure_tls;
    let probe_url = url.clone();
    let probe = match tokio::task::spawn_blocking(move || {
        crate::remote_pkg::RemoteSource::probe_with_options(&probe_url, insecure)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return json_err(StatusCode::BAD_REQUEST, &e),
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("probe task failed: {e}"),
            )
        }
    };

    let dir = req
        .dest_dir
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_download_dir);
    let name = local_name_for(&probe.filename, req.keep_name);
    let path = dir.join(name);

    let partial = partial_path(&path);
    let total = probe.total_size;
    let validators = probe.validators();
    // Asked first, before an existing file is taken over below: a live download of the
    // same name owns that file and its partial.
    if let Some(busy) = live_download_for(&state, &path) {
        return already_downloading(&path, busy);
    }

    let parsed = url.parse::<axum::http::Uri>().ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host().map(str::to_string))
        .unwrap_or_default();
    let meta = PartialMeta {
        host: host.clone(),
        path: parsed
            .as_ref()
            .map(|u| u.path().to_string())
            .unwrap_or_default(),
        total,
        etag: probe.etag.clone(),
        last_modified: probe.last_modified.clone(),
    };
    let prep = {
        let (path, partial, existing) = (path.clone(), partial.clone(), req.existing.clone());
        tokio::task::spawn_blocking(move || {
            prepare_files(&dir, &path, &partial, total, existing.as_deref(), &meta)
        })
        .await
    };
    let (complete, may_resume) = match prep {
        Ok(Ok(v)) => v,
        Ok(Err(resp)) => return *resp,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("prepare task failed: {e}"),
            )
        }
    };

    let id = uuid::Uuid::new_v4().to_string();
    let dl = Arc::new(Download {
        url_host: host.clone(),
        path: path.clone(),
        partial: partial.clone(),
        total,
        written: AtomicU64::new(if complete { total } else { 0 }),
        done: AtomicBool::new(complete),
        cancelled: AtomicBool::new(false),
        error: Mutex::new(None),
        worker_running: Mutex::new(!complete),
        ended: Mutex::new(complete.then(std::time::Instant::now)),
    });
    if let Err(busy) = register_unless_busy(&state, &id, dl.clone()) {
        return already_downloading(&path, busy);
    }

    if complete {
        crate::log_info!(
            "link-download: {} is already complete ({} bytes)",
            path.display(),
            total
        );
    } else {
        // Never log the path or query of a link: signed download URLs carry
        // credentials there. The host is enough to tell two origins apart.
        crate::log_info!(
            "link-download start: id={} host={} bytes={} dest={}",
            id,
            host,
            total,
            path.display()
        );
        let worker = dl.clone();
        tokio::task::spawn_blocking(move || {
            let dl = worker;
            let src = crate::remote_pkg::RemoteSource::new_with_options(url, total, insecure)
                .with_validators(validators);
            match copy_to_file(&src, &dl, may_resume) {
                // The partial stays: the next try of the same link resumes from it.
                Err(e) => {
                    crate::log_warn!("link-download failed: host={} err={}", dl.url_host, e);
                    *dl.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.to_string());
                }
                Ok(true) => {
                    dl.done.store(true, Ordering::Relaxed);
                    let _ = std::fs::remove_file(sidecar_path(&dl.partial));
                    crate::log_info!(
                        "link-download done: host={} bytes={}",
                        dl.url_host,
                        dl.written.load(Ordering::Relaxed)
                    );
                }
                Ok(false) => {}
            }
            finish_worker(&dl);
        });
    }

    (
        StatusCode::OK,
        Json(DownloadStartResponse {
            download_id: id,
            path: path.display().to_string(),
            total,
        }),
    )
        .into_response()
}

/// The file work before a download starts, off the async workers: makes the folder, settles
/// a file already under the final name, and decides whether the partial may be resumed (its
/// sidecar says it is this link's, or the user chose to resume a file they had). Writes this
/// link's sidecar. `(complete, may_resume)`.
fn prepare_files(
    dir: &std::path::Path,
    path: &std::path::Path,
    partial: &std::path::Path,
    total: u64,
    existing: Option<&str>,
    meta: &PartialMeta,
) -> Result<(bool, bool), Box<Response>> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(Box::new(json_err(
            StatusCode::BAD_REQUEST,
            &format!("cannot create {}: {e}", dir.display()),
        )));
    }
    // A file already under the final name is either this link's finished download (same
    // size: done, nothing to fetch) or an unrelated file, never resumed into or clobbered:
    // a half-written package that installs is far worse than an error, because the
    // failure surfaces much later as a corrupt install. Unfinished bytes only ever live in
    // `<name>.partial`, so a killed download no longer leaves a short file here.
    let mut took_over = false;
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == total => return Ok((true, false)),
        Ok(meta) => {
            if let Err(e) = take_over_existing(path, partial, existing) {
                return Err(Box::new(match e {
                    TakeOver::Refused => (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "error": format!(
                                "{} already exists with a different size ({} bytes, link says {}).",
                                path.display(),
                                meta.len(),
                                total
                            ),
                            "existing_path": path.display().to_string(),
                            "existing_bytes": meta.len(),
                            "total": total,
                        })),
                    )
                        .into_response(),
                    TakeOver::Io(e) => json_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("cannot take over {}: {e}", path.display()),
                    ),
                }));
            }
            took_over = existing == Some("resume");
        }
        Err(_) => {}
    }
    let may_resume = took_over || read_sidecar(partial).is_some_and(|m| m.same_file(meta));
    // What is still to come must fit: without this the download runs until the disk is full and
    // ends in a raw I/O error. A partial being resumed is part of it; any other is about to go.
    let partial_len = std::fs::metadata(partial).map_or(0, |m| m.len());
    let (need, free_soon) = if may_resume {
        (total.saturating_sub(partial_len), 0)
    } else {
        (total, partial_len)
    };
    if let Some(free) = ps5upload_fpkg::build::free_bytes(dir) {
        if let Some(refusal) = crate::remote::fetch::no_room(free.saturating_add(free_soon), need) {
            return Err(Box::new(refusal));
        }
    }
    if let Err(e) = write_sidecar(partial, meta) {
        crate::log_warn!("link-download: could not record the partial's source: {e}");
    }
    Ok((false, may_resume))
}

#[derive(Debug)]
enum TakeOver {
    /// No choice was given: the caller is asked first.
    Refused,
    Io(std::io::Error),
}

/// Clear the final name for this download, as the user chose. `resume` moves the file to
/// `.partial` (the resume check then keeps only bytes that match the link); `replace`
/// removes it and any partial.
fn take_over_existing(
    path: &std::path::Path,
    partial: &std::path::Path,
    choice: Option<&str>,
) -> Result<(), TakeOver> {
    match choice {
        Some("resume") => std::fs::rename(path, partial).map_err(TakeOver::Io),
        Some("replace") => {
            match std::fs::remove_file(partial) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(TakeOver::Io(e)),
            }
            std::fs::remove_file(path).map_err(TakeOver::Io)
        }
        _ => Err(TakeOver::Refused),
    }
}

/// Bytes re-fetched before a resume point and compared with the partial file's tail: a
/// `.partial` of the same name from a different link (or a different build of the package)
/// must not be spliced onto this one.
const RESUME_CHECK: u64 = 64 * 1024;

/// Pull the package window by window into `dl.partial`, then rename it to `dl.path`.
/// `Ok(true)` once the file is complete under its name; `Ok(false)` when cancelled.
///
/// With `may_resume` (the partial's sidecar names this link), resumes from the partial
/// file's length: the probe already established that the origin serves byte ranges (it
/// refuses one that does not), so a resume is always possible. The first and the last
/// `RESUME_CHECK` bytes are fetched again and compared first; a mismatch, or a partial
/// longer than the package, starts over.
///
/// Sequential writes on purpose: the fetcher is already parallel *inside* a
/// window, so this needs no concurrency of its own, and one append-only
/// stream is what a spinning disk and a network share both handle best.
fn copy_to_file(
    src: &crate::remote_pkg::RemoteSource,
    dl: &Arc<Download>,
    may_resume: bool,
) -> std::io::Result<bool> {
    use std::io::Read;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&dl.partial)?;
    let total = dl.total;
    let mut offset = file.metadata()?.len();
    if offset > total || !may_resume {
        offset = 0;
    }
    if offset > 0 {
        let mut same = true;
        // The head, then the tail (they overlap on a short partial, which is harmless).
        for from in [0, offset.saturating_sub(RESUME_CHECK)] {
            let to = (from + RESUME_CHECK).min(offset);
            let mut have = vec![0u8; (to - from) as usize];
            file.seek(SeekFrom::Start(from))?;
            file.read_exact(&mut have)?;
            if src.read_range(from, to - 1)? != have {
                same = false;
                break;
            }
        }
        if !same {
            crate::log_info!(
                "link-download: the partial file is from another download; starting over"
            );
            offset = 0;
        } else {
            crate::log_info!("link-download: resuming at {offset} of {total} bytes");
        }
    }
    file.set_len(offset)?;
    file.seek(SeekFrom::Start(offset))?;
    dl.written.store(offset, Ordering::Relaxed);

    while offset < total {
        if dl.cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let end = (offset + COPY_WINDOW).min(total) - 1;
        let bytes = src.read_range(offset, end)?;
        file.write_all(&bytes)?;
        offset = end + 1;
        dl.written.store(offset, Ordering::Relaxed);
    }
    file.flush()?;
    // The install that follows reads this file back immediately; make sure
    // what it reads is what we fetched, not what is still in the page cache
    // of a machine that might be about to sleep.
    file.sync_all()?;
    drop(file);
    if dl.cancelled.load(Ordering::Relaxed) {
        return Ok(false);
    }
    std::fs::rename(&dl.partial, &dl.path)?;
    Ok(true)
}

async fn status_handler(
    State(state): State<DownloadStateHandle>,
    Query(q): Query<DownloadIdQuery>,
) -> Response {
    let dl = {
        let items = state.items.lock().unwrap_or_else(|e| e.into_inner());
        items.get(&q.id).cloned()
    };
    let Some(dl) = dl else {
        return json_err(StatusCode::NOT_FOUND, "no such download");
    };
    let error = dl.error.lock().unwrap_or_else(|e| e.into_inner()).clone();
    (
        StatusCode::OK,
        Json(DownloadStatus {
            download_id: q.id,
            path: dl.path.display().to_string(),
            total: dl.total,
            written: dl.written.load(Ordering::Relaxed),
            done: dl.done.load(Ordering::Relaxed),
            cancelled: dl.cancelled.load(Ordering::Relaxed),
            error,
        }),
    )
        .into_response()
}

async fn cancel_handler(
    State(state): State<DownloadStateHandle>,
    Json(req): Json<DownloadCancelRequest>,
) -> Response {
    let dl = {
        let items = state.items.lock().unwrap_or_else(|e| e.into_inner());
        items.get(&req.id).cloned()
    };
    let Some(dl) = dl else {
        return json_err(StatusCode::NOT_FOUND, "no such download");
    };
    // The partial goes: a half-written .pkg left in the downloads folder is worse than no
    // file, since installing one fails late and confusingly. Only ever the incomplete one --
    // a download that already finished is a file the user asked for, whatever a late cancel
    // says. A running worker removes it itself when it sees the cancel; deleting it here
    // could hit a retry that already owns the name.
    // An ended download's partial may meanwhile belong to a retry of the same file, which
    // is left alone. The registry lock is held throughout (before the worker's, as in
    // `is_live`), so no retry can register in between.
    let (dl2, id, state) = (dl.clone(), req.id.clone(), state.clone());
    let _ = tokio::task::spawn_blocking(move || {
        let items = state.items.lock().unwrap_or_else(|e| e.into_inner());
        let retry_owns_it = items
            .iter()
            .any(|(other_id, o)| *other_id != id && o.path == dl2.path && is_live(o));
        let running = dl2.worker_running.lock().unwrap_or_else(|e| e.into_inner());
        dl2.cancelled.store(true, Ordering::Relaxed);
        if !*running && !retry_owns_it && !dl2.done.load(Ordering::Relaxed) {
            match remove_partial(&dl2.partial) {
                Ok(()) => crate::log_info!("link-download cancelled: removed the partial file"),
                Err(e) => {
                    crate::log_warn!("link-download cancelled: could not remove partial: {e}")
                }
            }
        }
        if !*running {
            dl2.mark_ended();
        }
        drop(running);
        drop(items);
    })
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({ "download_id": req.id, "cancelled": true })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_package_gets_its_extension_and_an_archive_part_keeps_its_name() {
        assert_eq!(super::local_name_for("game", false), "game.pkg");
        assert_eq!(super::local_name_for("Game.PKG", false), "Game.PKG");
        assert_eq!(super::local_name_for("", false), "package.pkg");
        // Parts of a set find each other by name.
        assert_eq!(
            super::local_name_for("game.part2.rar", true),
            "game.part2.rar"
        );
        assert_eq!(super::local_name_for("", true), "download");
    }

    use super::*;

    #[test]
    fn an_existing_file_is_refused_unless_the_user_chose_resume_or_replace() {
        let dir = std::env::temp_dir().join(format!("ps5u-dl-takeover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("game.pkg");
        let partial = partial_path(&path);

        std::fs::write(&path, b"short").unwrap();
        assert!(matches!(
            take_over_existing(&path, &partial, None),
            Err(TakeOver::Refused)
        ));
        assert!(path.exists());

        // Resume: the file becomes the partial, so its bytes are checked and reused.
        take_over_existing(&path, &partial, Some("resume")).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&partial).unwrap(), b"short");

        // Replace: both the file and any partial go.
        std::fs::write(&path, b"other").unwrap();
        take_over_existing(&path, &partial, Some("replace")).unwrap();
        assert!(!path.exists());
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_url_cannot_name_a_file_outside_the_download_directory() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("/a/b/c.pkg"), "c.pkg");
        assert_eq!(safe_file_name(r"..\..\x.pkg"), "x.pkg");
        // Still encoded here, so still one component of its own.
        assert_eq!(safe_file_name("..%2F..%2Fx.pkg"), "%2F..%2Fx.pkg");
        assert_eq!(safe_file_name(""), "package.pkg");
        assert_eq!(safe_file_name("...."), "package.pkg");
        assert_eq!(safe_file_name("a\u{0}b.pkg"), "ab.pkg");
    }

    /// `ゲーム.part1.rar` became `part1.rar`, which UnRAR cannot find `part2` from, and every
    /// non-Latin name of one size collapsed into one file, so the second download read as done.
    #[test]
    fn a_name_keeps_everything_that_is_not_unsafe() {
        assert_eq!(safe_file_name("ゲーム.part1.rar"), "ゲーム.part1.rar");
        assert_eq!(safe_file_name("Игра (2024).pkg"), "Игра (2024).pkg");
        assert_eq!(super::local_name_for("ゲーム.pkg", false), "ゲーム.pkg");
        assert_ne!(
            super::local_name_for("ゲーム.pkg", false),
            super::local_name_for("게임.pkg", false)
        );
    }

    #[test]
    fn a_name_cannot_be_something_else_on_windows() {
        assert_eq!(safe_file_name("CON.pkg"), "_CON.pkg");
        assert_eq!(safe_file_name("lpt1"), "_lpt1");
        assert_eq!(safe_file_name("game.pkg."), "game.pkg");
        assert_eq!(safe_file_name("game.pkg  "), "game.pkg");
        assert_eq!(safe_file_name("a:b.pkg"), "ab.pkg");
    }

    /// The probe knows the size, so a download that cannot fit is said at once. It used to run
    /// until the disk was full and end in a raw I/O error.
    #[test]
    fn a_download_that_cannot_fit_is_refused_before_it_starts() {
        let dir = scratch("too-big");
        let (path, partial) = (dir.join("a.pkg"), dir.join("a.pkg.partial"));
        let huge = u64::MAX / 2;
        let meta = PartialMeta {
            host: "h".into(),
            path: "/a.pkg".into(),
            total: huge,
            etag: None,
            last_modified: None,
        };
        let refused = prepare_files(&dir, &path, &partial, huge, None, &meta)
            .expect_err("nothing has that much free space");
        assert_eq!(refused.status(), StatusCode::INSUFFICIENT_STORAGE);
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_normal_package_name_survives_intact() {
        assert_eq!(
            safe_file_name("UP9000-PPSA03016_00-MARVELSPIDERMAN2.pkg"),
            "UP9000-PPSA03016_00-MARVELSPIDERMAN2.pkg"
        );
        // Spaces are legal in a package name and common in dumps.
        assert_eq!(
            safe_file_name("Marvel Spider-Man 2 - BASE.pkg"),
            "Marvel Spider-Man 2 - BASE.pkg"
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5u-dl-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn download_for(dir: &std::path::Path, total: u64) -> Arc<Download> {
        let path = dir.join("game.pkg");
        Arc::new(Download {
            partial: partial_path(&path),
            path,
            total,
            ..Default::default()
        })
    }

    /// The bytes land in `<name>.partial` and only a complete file gets the real name.
    #[test]
    fn a_download_writes_a_partial_and_renames_it_when_complete() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};
        let data = body(300_000);
        let origin = spawn_origin(data.clone(), 0, false);
        let dir = scratch("full");
        let dl = download_for(&dir, data.len() as u64);
        assert_eq!(dl.partial, dir.join("game.pkg.partial"));
        let src = crate::remote_pkg::RemoteSource::new(
            format!("http://{}/game.pkg", origin.addr),
            data.len() as u64,
        );
        assert!(copy_to_file(&src, &dl, true).unwrap());
        assert_eq!(std::fs::read(&dl.path).unwrap(), data);
        assert!(!dl.partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A killed download resumes from its partial file instead of being refused.
    #[test]
    fn a_download_resumes_from_its_partial_file() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};
        let data = body(300_000);
        let origin = spawn_origin(data.clone(), 0, false);
        let dir = scratch("resume");
        let dl = download_for(&dir, data.len() as u64);
        std::fs::write(&dl.partial, &data[..200_000]).unwrap();
        let src = crate::remote_pkg::RemoteSource::new(
            format!("http://{}/game.pkg", origin.addr),
            data.len() as u64,
        );
        assert!(copy_to_file(&src, &dl, true).unwrap());
        assert_eq!(std::fs::read(&dl.path).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A partial of the same name from another file is not spliced on: it starts over.
    #[test]
    fn a_partial_from_another_file_starts_over() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};
        let data = body(300_000);
        let origin = spawn_origin(data.clone(), 0, false);
        let dir = scratch("other");
        let dl = download_for(&dir, data.len() as u64);
        std::fs::write(&dl.partial, vec![0xAAu8; 150_000]).unwrap();
        let src = crate::remote_pkg::RemoteSource::new(
            format!("http://{}/game.pkg", origin.addr),
            data.len() as u64,
        );
        assert!(copy_to_file(&src, &dl, true).unwrap());
        assert_eq!(std::fs::read(&dl.path).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The start route: an already complete file is done at once, and a second live
    /// download of the same file is refused with the first one's id.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_complete_file_is_done_and_a_second_live_download_is_refused() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};
        let data = body(100_000);
        let origin = spawn_origin(data.clone(), 0, false);
        let dir = scratch("route");
        std::fs::write(dir.join("game.pkg"), &data).unwrap();
        let state: DownloadStateHandle = Arc::default();
        let req = || DownloadStartRequest {
            url: format!("http://{}/game.pkg", origin.addr),
            insecure_tls: false,
            dest_dir: Some(dir.display().to_string()),
            keep_name: false,
            existing: None,
        };
        let resp = start_handler(State(state.clone()), Json(req())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let item = state
            .items
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned()
            .unwrap();
        assert!(item.done.load(Ordering::Relaxed));
        assert_eq!(item.written.load(Ordering::Relaxed), data.len() as u64);

        // A live one for the same file blocks a second.
        let live = download_for(&dir, 5);
        state.items.lock().unwrap().clear();
        register_unless_busy(&state, "first", live.clone()).unwrap();
        assert_eq!(
            register_unless_busy(&state, "second", download_for(&dir, 5)),
            Err("first".to_string())
        );
        // Once it ended (failed here), the file is free again.
        *live.error.lock().unwrap() = Some("x".into());
        assert!(register_unless_busy(&state, "third", download_for(&dir, 5)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A different file under the name is refused with its size (so the UI can offer Resume
    /// or Replace), and never taken over while a live download owns the name.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_existing_file_is_described_and_a_live_download_is_never_taken_over() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};
        let data = body(100_000);
        let origin = spawn_origin(data.clone(), 0, false);
        let dir = scratch("exists");
        let path = dir.join("game.pkg");
        std::fs::write(&path, &data[..10]).unwrap();
        let state: DownloadStateHandle = Arc::default();
        let req = |existing: Option<&str>| DownloadStartRequest {
            url: format!("http://{}/game.pkg", origin.addr),
            insecure_tls: false,
            dest_dir: Some(dir.display().to_string()),
            keep_name: false,
            existing: existing.map(str::to_string),
        };
        let resp = start_handler(State(state.clone()), Json(req(None))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["existing_bytes"], 10);
        assert_eq!(json["total"], data.len() as u64);
        assert!(json["existing_path"]
            .as_str()
            .unwrap()
            .ends_with("game.pkg"));

        // A live download of the same name answers with its id; the file stays put.
        let live = download_for(&dir, data.len() as u64);
        register_unless_busy(&state, "first", live).unwrap();
        let resp = start_handler(State(state.clone()), Json(req(Some("replace")))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&path).unwrap(), &data[..10]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancel while the worker runs: the partial is the worker's to remove, and the entry
    /// stays live (blocking a second download of the file) until the worker exits.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_download_stays_live_until_its_worker_exits() {
        let dir = scratch("cancel");
        let dl = download_for(&dir, 10);
        *dl.worker_running.lock().unwrap() = true;
        std::fs::write(&dl.partial, b"half").unwrap();
        let state: DownloadStateHandle = Arc::default();
        register_unless_busy(&state, "first", dl.clone()).unwrap();
        let resp = cancel_handler(
            State(state.clone()),
            Json(DownloadCancelRequest { id: "first".into() }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(dl.partial.exists(), "the running worker owns the partial");
        assert_eq!(
            register_unless_busy(&state, "second", download_for(&dir, 10)),
            Err("first".to_string())
        );
        finish_worker(&dl);
        assert!(!dl.partial.exists());
        assert!(register_unless_busy(&state, "third", download_for(&dir, 10)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancelling a failed download never deletes the partial a live retry is writing.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_a_failed_download_spares_the_retrys_partial() {
        let dir = scratch("retry");
        let failed = download_for(&dir, 10);
        *failed.error.lock().unwrap() = Some("x".into());
        let state: DownloadStateHandle = Arc::default();
        register_unless_busy(&state, "failed", failed).unwrap();
        let retry = download_for(&dir, 10);
        *retry.worker_running.lock().unwrap() = true;
        register_unless_busy(&state, "retry", retry.clone()).unwrap();
        std::fs::write(&retry.partial, b"retry bytes").unwrap();
        cancel_handler(
            State(state.clone()),
            Json(DownloadCancelRequest {
                id: "failed".into(),
            }),
        )
        .await;
        assert!(retry.partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A partial is resumed only when its sidecar names the same link and size.
    #[test]
    fn a_partial_resumes_only_when_its_sidecar_matches() {
        let dir = scratch("sidecar");
        let path = dir.join("game.pkg");
        let partial = partial_path(&path);
        let meta = |p: &str, etag: Option<&str>| PartialMeta {
            host: "h".into(),
            path: p.into(),
            total: 100,
            etag: etag.map(str::to_string),
            last_modified: None,
        };
        std::fs::write(&partial, b"x").unwrap();
        // No sidecar: an unknown partial is not resumed.
        assert_eq!(
            prepare_files(&dir, &path, &partial, 100, None, &meta("/a.pkg", None)).unwrap(),
            (false, false)
        );
        // The sidecar now names /a.pkg.
        assert_eq!(
            prepare_files(&dir, &path, &partial, 100, None, &meta("/a.pkg", None)).unwrap(),
            (false, true)
        );
        assert_eq!(
            prepare_files(&dir, &path, &partial, 100, None, &meta("/b.pkg", None)).unwrap(),
            (false, false)
        );
        write_sidecar(&partial, &meta("/a.pkg", Some("\"v1\""))).unwrap();
        assert!(!read_sidecar(&partial)
            .unwrap()
            .same_file(&meta("/a.pkg", Some("\"v2\""))));
        assert!(read_sidecar(&partial)
            .unwrap()
            .same_file(&meta("/a.pkg", None)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A partial whose tail matches but whose head does not is another file: start over.
    #[test]
    fn a_partial_with_a_different_head_starts_over() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};
        let data = body(300_000);
        let origin = spawn_origin(data.clone(), 0, false);
        let dir = scratch("head");
        let dl = download_for(&dir, data.len() as u64);
        let mut part = data[..200_000].to_vec();
        part[..1000].fill(0xEE);
        std::fs::write(&dl.partial, &part).unwrap();
        let src = crate::remote_pkg::RemoteSource::new(
            format!("http://{}/game.pkg", origin.addr),
            data.len() as u64,
        );
        assert!(copy_to_file(&src, &dl, true).unwrap());
        assert_eq!(std::fs::read(&dl.path).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stopped_downloads_are_forgotten_after_a_while_and_beyond_a_cap() {
        let mut items = std::collections::HashMap::new();
        let ended = |ago: u64| {
            let d = Download::default();
            *d.ended.lock().unwrap() =
                std::time::Instant::now().checked_sub(std::time::Duration::from_secs(ago));
            Arc::new(d)
        };
        items.insert("old".to_string(), ended(ENDED_TTL.as_secs() + 5));
        items.insert("live".to_string(), Arc::new(Download::default()));
        for i in 0..ENDED_CAP + 3 {
            items.insert(format!("n{i}"), ended(i as u64));
        }
        prune(&mut items);
        assert!(!items.contains_key("old"));
        assert!(items.contains_key("live"));
        assert_eq!(items.len(), ENDED_CAP + 1);
        // The newest stay.
        assert!(items.contains_key("n0"));
    }

    #[test]
    fn the_download_directory_is_overridable() {
        // The default must be under the user's home, not a temp dir that a
        // reboot clears while a 100 GiB download is half finished.
        let d = default_download_dir();
        assert!(d.ends_with("ps5upload"), "got {}", d.display());
    }
}
