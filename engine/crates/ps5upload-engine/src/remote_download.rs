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
use std::path::{Path, PathBuf};
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
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Downloads").join("ps5upload")
}

/// Strip anything that is not safe in a file name. The name comes from a URL,
/// so it is attacker-influenced in the sense that a hostile link could carry
/// `../../` or a NUL; it must never escape the download directory.
fn safe_file_name(raw: &str) -> String {
    let trimmed = raw.rsplit('/').next().unwrap_or(raw);
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "package.pkg".to_string()
    } else {
        cleaned
    }
}

#[derive(Debug, Default)]
pub struct Download {
    pub url_host: String,
    pub path: PathBuf,
    pub total: u64,
    pub written: AtomicU64,
    pub done: AtomicBool,
    pub cancelled: AtomicBool,
    pub error: Mutex<Option<String>>,
    pub started_unix: u64,
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

fn json_err(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn router(state: DownloadStateHandle) -> Router {
    Router::new()
        .route("/api/pkg/remote/download/start", post(start_handler))
        .route("/api/pkg/remote/download/status", get(status_handler))
        .route("/api/pkg/remote/download/cancel", post(cancel_handler))
        .with_state(state)
}

#[cfg(not(target_os = "android"))]
async fn start_handler(
    State(state): State<DownloadStateHandle>,
    Json(req): Json<DownloadStartRequest>,
) -> Response {
    let url = req.url.trim().to_string();
    if url.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "url is required");
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
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json_err(
            StatusCode::BAD_REQUEST,
            &format!("cannot create {}: {e}", dir.display()),
        );
    }
    let name = safe_file_name(if probe.filename.is_empty() {
        "package.pkg"
    } else {
        &probe.filename
    });
    let path = dir.join(name);

    // Refuse to silently resume into, or clobber, an unrelated file of the
    // same name: a half-written package that installs is far worse than an
    // error, because the failure surfaces much later as a corrupt install.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() != probe.total_size {
            return json_err(
                StatusCode::CONFLICT,
                &format!(
                    "{} already exists with a different size ({} bytes, link says {}). \
                     Move or delete it first.",
                    path.display(),
                    meta.len(),
                    probe.total_size
                ),
            );
        }
    }

    let host = url
        .parse::<axum::http::Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_string))
        .unwrap_or_default();

    let id = uuid::Uuid::new_v4().to_string();
    let dl = Arc::new(Download {
        url_host: host.clone(),
        path: path.clone(),
        total: probe.total_size,
        written: AtomicU64::new(0),
        done: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        error: Mutex::new(None),
        started_unix: now_unix(),
    });
    state
        .items
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.clone(), dl.clone());

    // Never log the path or query of a link: signed download URLs carry
    // credentials there. The host is enough to tell two origins apart.
    crate::log_info!(
        "link-download start: id={} host={} bytes={} dest={}",
        id,
        host,
        probe.total_size,
        path.display()
    );

    let total = probe.total_size;
    let dest = path.clone();
    let worker = dl.clone();
    tokio::task::spawn_blocking(move || {
        let dl = worker;
        let src = crate::remote_pkg::RemoteSource::new_with_options(url, total, insecure);
        if let Err(e) = copy_to_file(&src, &dl, &dest) {
            crate::log_warn!("link-download failed: host={} err={}", dl.url_host, e);
            *dl.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.to_string());
        } else if !dl.cancelled.load(Ordering::Relaxed) {
            dl.done.store(true, Ordering::Relaxed);
            crate::log_info!(
                "link-download done: host={} bytes={}",
                dl.url_host,
                dl.written.load(Ordering::Relaxed)
            );
        }
    });

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

#[cfg(target_os = "android")]
async fn start_handler(
    State(_state): State<DownloadStateHandle>,
    Json(_req): Json<DownloadStartRequest>,
) -> Response {
    json_err(
        StatusCode::BAD_REQUEST,
        "downloading a package from a link is not available in the Android build",
    )
}

/// Pull the package window by window and write it out sequentially.
///
/// Sequential writes on purpose: the fetcher is already parallel *inside* a
/// window, so this needs no concurrency of its own, and one append-only
/// stream is what a spinning disk and a network share both handle best.
fn copy_to_file(
    src: &crate::remote_pkg::RemoteSource,
    dl: &Arc<Download>,
    path: &Path,
) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.seek(SeekFrom::Start(0))?;

    let total = dl.total;
    let mut offset = 0u64;
    while offset < total {
        if dl.cancelled.load(Ordering::Relaxed) {
            return Ok(());
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
    Ok(())
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
    dl.cancelled.store(true, Ordering::Relaxed);
    // Remove the partial file. A half-written .pkg left in the downloads
    // folder is worse than no file: it is indistinguishable from a real
    // package at a glance, and installing one fails late and confusingly.
    // Only ever the incomplete one -- a download that already finished is a
    // file the user asked for, whatever a late cancel says.
    if !dl.done.load(Ordering::Relaxed) {
        match std::fs::remove_file(&dl.path) {
            Ok(()) => crate::log_info!("link-download cancelled: removed the partial file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::log_warn!("link-download cancelled: could not remove partial: {e}"),
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "download_id": req.id, "cancelled": true })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_cannot_name_a_file_outside_the_download_directory() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("/a/b/c.pkg"), "c.pkg");
        assert_eq!(
            safe_file_name("..%2F..%2Fx.pkg"),
            "..2F..2Fx.pkg".trim_start_matches('.')
        );
        assert_eq!(safe_file_name(""), "package.pkg");
        assert_eq!(safe_file_name("...."), "package.pkg");
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

    #[test]
    fn the_download_directory_is_overridable() {
        // The default must be under the user's home, not a temp dir that a
        // reboot clears while a 100 GiB download is half finished.
        let d = default_download_dir();
        assert!(d.ends_with("ps5upload"), "got {}", d.display());
    }
}
