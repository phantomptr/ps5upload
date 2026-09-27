//! Engine-side .pkg install plumbing.
//!
//! Three responsibilities:
//!   1. Parse a `.pkg` file (single or split-part set) and surface
//!      metadata for the UI — `parse_handler`.
//!   2. Host the `.pkg` bytes over HTTP with Range support so Sony's
//!      BGFT service on the PS5 can pull them — `serve_handler`.
//!   3. Drive the install: tell the payload to call BGFT, poll status,
//!      surface progress + final outcome — `install_start_handler` /
//!      `install_status_handler` / `install_cancel_handler`.
//!
//! Sessions are keyed by a random UUID v4 in the URL path so anyone
//! else on the LAN can't enumerate or hijack a different user's PKG.
//! Standard LAN-trust model for local PS5-side HTTP fetches.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{header, HeaderMap, Response, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use ps5upload_core::pkg_install::PkgInstallStatus;
use ps5upload_pkg::{
    extract_from_ffpkg, inspect_ffpkg, metadata_from_reader, package_fingerprint_from_reader,
    parse_pkg, parse_split_pkg, PkgKind, PkgMetadata, SplitPkgMetadata,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One in-flight install. The session lives from `install/start` until
/// the user dismisses the result or cancels. The HTTP-host listener
/// uses `parts` to satisfy Range requests.
///
/// Several fields are recorded for diagnostics / future introspection
/// endpoints (e.g. listing active sessions in the engine logs) even
/// though no current handler reads them — `#[allow(dead_code)]` documents
/// this intentional surplus rather than churn the struct each release.
/// A package being proxied from an HTTP(S) origin for an install-from-a-link
/// session (see `remote_pkg`).
///
/// Installing from a link needs an HTTP client, which the Android build
/// deliberately does not carry so its cross-compile stays pure Rust. There the
/// type is uninhabited: `Option<Arc<RemotePkg>>` can only ever be `None`, so
/// the remote paths are statically unreachable rather than conditionally
/// compiled out of every call site.
#[cfg(not(target_os = "android"))]
#[derive(Debug)]
pub enum RemotePkg {
    /// An HTTP(S) link, fetched over many connections.
    Http(Arc<crate::remote_pkg::RemoteSource>),
    /// A file on a saved server (SMB, FTP, FTPS or SFTP), read in positioned pieces.
    Remote(crate::remote::range::RemoteRangeSource),
}

#[cfg(not(target_os = "android"))]
impl RemotePkg {
    pub fn read_range(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        match self {
            RemotePkg::Http(r) => r.read_range(start, end),
            RemotePkg::Remote(r) => r.read_range(start, end),
        }
    }

    /// Fetch ahead of the console. Only the HTTP source needs it: its window
    /// cache is what lets the origin run ahead of the console. An SMB read on
    /// a LAN share answers well inside the console's own pacing.
    pub fn prefetch_after(this: &Arc<Self>, offset: u64) {
        if let RemotePkg::Http(r) = &**this {
            crate::remote_pkg::RemoteSource::prefetch_after(r, offset);
        }
    }
}

#[cfg(target_os = "android")]
#[derive(Debug)]
pub enum RemotePkg {}

#[cfg(target_os = "android")]
impl RemotePkg {
    pub fn read_range(&self, _start: u64, _end: u64) -> std::io::Result<Vec<u8>> {
        match *self {}
    }

    /// Readahead stub for the uninhabited Android type. No value of `RemotePkg`
    /// can exist here, so this is never called; it exists so the serve path's
    /// call site needs no `cfg`, matching `read_range` above.
    pub fn prefetch_after(_this: &std::sync::Arc<Self>, _offset: u64) {}
}

/// How often the pkg-host serve-rate line may be emitted, in seconds.
const SERVE_RATE_LOG_SECS: u64 = 15;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct InstallSession {
    pub id: String,
    pub parts: Vec<PathBuf>,
    pub part_sizes: Vec<u64>,
    pub total_size: u64,
    pub content_id: String,
    pub title: String,
    pub package_type: String,
    /// Sampled BLAKE3 identity of the exact source artifact. Unlike ContentID
    /// and APP_VER this distinguishes same-version alternatives (backport vs.
    /// optional fix) and lets status verify the package that actually landed.
    pub package_fingerprint: String,
    /// PS5 mgmt-port address for status polling.
    pub ps5_mgmt_addr: String,
    /// BGFT task_id assigned by the payload after install/start.
    pub task_id: Option<i32>,
    /// Latest BGFT err_code surfaced to the client.
    pub err_code: u32,
    /// Latest detail string from payload / engine.
    pub detail: String,
    /// Whether the user has requested cancel (host-side; we stop
    /// serving HTTP when this is set).
    pub cancelled: bool,
    pub created_at_unix: u64,
    /// PS5-side absolute path of the Tier-1 staging file. Set on
    /// install_start when `local_ps5_path` was provided; the
    /// status handler best-effort fs_delete's it when the install
    /// terminates (phase = Done | Error). Then `take()`'s the field
    /// to None to prevent double-delete on subsequent polls.
    pub staging_path: Option<String>,
    /// Cached terminal status. Once an install reaches Done/Error the
    /// payload may reap the BGFT task_id, so re-polling it would 502 and
    /// flip a finished install to a spurious error on the client's next
    /// poll. We snapshot the terminal status here and replay it for any
    /// later poll instead of hitting the (now-gone) task.
    pub terminal_status: Option<PkgInstallStatus>,
    /// Resolved launchability once verification concludes: `Some(true)` =
    /// the title_id appeared in app.db (definitively launchable),
    /// `Some(false)` = it never appeared within the verification window,
    /// `None` = verification wasn't applicable (sqlite unavailable on this
    /// FW, or no real title_id) — the legacy optimistic behavior. Cached
    /// alongside `terminal_status` so replayed polls stay consistent.
    pub launchable: Option<bool>,
    /// True for a Stream/serve-only session: the engine only hosts the pkg
    /// over HTTP and the DPI daemon performs the install, so this session
    /// legitimately never gets a BGFT task_id.
    pub serve_only: bool,
    /// True once the console-side "install finished" toast has been sent.
    ///
    /// The status endpoint is polled repeatedly and a session can be observed
    /// terminal on any number of those polls, so without this latch the
    /// console would get a fresh toast every poll interval forever.
    pub notified_console: bool,
    /// Free bytes on the data volume at the first status poll — the baseline
    /// the progress tracker measures "bytes consumed" against. `None` until the
    /// first poll captures it (or if volumes couldn't be listed). See
    /// `observe_consumed` / `install_verdict`.
    pub install_start_free_bytes: Option<u64>,
    /// Max bytes the install has consumed so far (monotonic) — `max(free-space
    /// drop, title-dir size)`. Drives the live progress % and the stall clock.
    pub progress_consumed_bytes: u64,
    /// Unix time `progress_consumed_bytes` last increased. Resets the stall
    /// clock on every observed bit of progress, so a slow-but-advancing install
    /// never trips the adaptive stall deadline. `None` until the first poll.
    pub last_progress_unix: Option<u64>,
    /// Cached "the install stalled (no disk progress)" verdict, so replayed
    /// terminal polls keep reporting the stall (and the UI keeps the pkg).
    pub stalled: bool,
    /// Sony accepted a synthetic-DONE install, but neither registration nor
    /// byte-settle proved completion. Terminal for UI purposes, but never
    /// eligible for staging deletion or a green "installed" result.
    pub accepted_unverified: bool,
    /// Number of pkg-host responses served for this session. Stream-install
    /// diagnostics use zero to distinguish a Sony HTTP/proxy preflight reject
    /// from a failure after package transfer began.
    pub requests_served: u64,
    /// Unix time the last `pkg-host serve rate` line was emitted. The rate
    /// log is paced by TIME, not by a request count: a link install using
    /// 32 MiB windows serves only ~100 requests for a whole 3 GiB, so the
    /// previous "every 512 requests" trigger never fired once on exactly the
    /// slow installs it existed to explain.
    pub last_rate_log_unix: u64,
    /// Total response-body bytes served across Range requests. This may exceed
    /// package size when Sony retries a range; it is diagnostic, not progress.
    pub bytes_served: u64,
    /// Highest byte the console has fetched, as a monotonic `end + 1` over
    /// every Range response. `bytes_served` is a raw sum and is NOT a progress
    /// ratio — Sony re-fetches ranges, and a measured 1.35 GB package pulled
    /// 1.53 GB, i.e. the raw sum passes 100% well before the transfer ends.
    pub transfer_bytes: u64,
    /// Which granules of the package the console has received. `transfer_bytes`
    /// is its derived `bytes()`; see `TransferCoverage` for why neither a raw
    /// sum nor a furthest-offset can stand in for it.
    pub transfer: TransferCoverage,
    /// The DPI daemon's answer for a Stream/serve-only session, recorded on the
    /// session rather than only in the HTTP reply. A caller that stopped waiting
    /// (browser, proxy, or a client timeout) still gets the verdict from the
    /// status poll, and a slow hand-off stops being an ambiguous outcome.
    pub dpi_ok: Option<bool>,
    pub dpi_rc: Option<i32>,
    pub dpi_detail: String,
    /// Unix time of the last sign of life for this session — a pkg-host range
    /// served, or a status poll. Session expiry is measured from THIS, not from
    /// creation: a 200-300 GB install on a modest link runs for many hours, and
    /// ageing it out by creation time reaped the session while the console was
    /// still fetching. The console's next range then got `404 no such install
    /// session`, which is what users saw as the transfer "losing connection".
    pub last_activity_unix: u64,
    /// Set for an install-from-a-link session: the package lives on an HTTP
    /// origin and `parts` is empty, so range reads are proxied through this
    /// instead of the local filesystem. `None` for every local/staged install.
    pub remote: Option<Arc<RemotePkg>>,
}

#[derive(Default)]
pub struct PkgInstallState {
    /// Active install sessions keyed by UUIDv4. Every route that
    /// touches this map locks via `.lock().unwrap_or_else(|e|
    /// e.into_inner())` rather than a bare `.unwrap()`: a panic in
    /// any handler that holds this lock would otherwise poison the
    /// mutex and wedge every subsequent install request for the
    /// engine's lifetime. The `into_inner()` recovery is safe here
    /// because the map's invariant is per-entry self-contained — a
    /// partially-mutated session row is no worse than a stale row,
    /// and the next status poll / GC pass cleans it up.
    pub sessions: Mutex<HashMap<String, InstallSession>>,
    /// Unified-install (spec 2) job store: one install per console, statuses
    /// polled via `/api/pkg/install/status`.
    pub jobs: crate::install::JobStore,
}

pub type PkgInstallStateHandle = Arc<PkgInstallState>;

impl PkgInstallState {
    /// The state a starting engine opens with: any install sessions the previous engine
    /// process was serving, so a restart mid-install keeps answering the console.
    pub fn restored() -> Self {
        let sessions = persist::load();
        if !sessions.is_empty() {
            crate::log_info!(
                "pkg-host: resumed {} install session(s) from before the engine restarted",
                sessions.len()
            );
        }
        Self {
            sessions: Mutex::new(sessions),
            jobs: crate::install::JobStore::new(),
        }
    }
}

/// Install sessions that outlive the engine process.
///
/// The console fetches a package from a URL naming its session, for hours on a large title, and
/// retries a range a few times before failing the install (`0x80b22404`). An engine that
/// restarts meanwhile (an app update, a crash, `tauri dev` rebuilding the desktop app) used to
/// come back with an empty session map and answer 404, killing a 119 GB install at 13%. Each
/// session serving local files is therefore written to disk, and a starting engine takes back
/// those whose files are still exactly as they were. Link sessions are not kept: they hold a
/// live connection to their origin.
mod persist {
    use super::*;

    #[derive(Serialize, Deserialize)]
    struct Saved {
        id: String,
        parts: Vec<PathBuf>,
        part_sizes: Vec<u64>,
        total_size: u64,
        content_id: String,
        title: String,
        package_type: String,
        package_fingerprint: String,
        ps5_mgmt_addr: String,
        serve_only: bool,
        staging_path: Option<String>,
        created_at_unix: u64,
        last_activity_unix: u64,
    }

    /// `PS5UPLOAD_STATE_DIR`, else `~/.ps5upload/state`. Tests use only an explicit directory.
    fn path() -> Option<PathBuf> {
        let dir = match std::env::var("PS5UPLOAD_STATE_DIR") {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
            _ if cfg!(test) => return None,
            _ => {
                let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
                PathBuf::from(home.ok().filter(|h| !h.trim().is_empty())?)
                    .join(".ps5upload")
                    .join("state")
            }
        };
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("pkg-host-sessions.json"))
    }

    /// Write every session still serving local files. Best effort: a failure only loses the
    /// ability to resume after a restart.
    pub(super) fn save(sessions: &HashMap<String, InstallSession>) {
        if let Some(path) = path() {
            save_to(&path, sessions);
        }
    }

    pub(super) fn save_to(path: &std::path::Path, sessions: &HashMap<String, InstallSession>) {
        let saved: Vec<Saved> = sessions
            .values()
            .filter(|s| {
                s.remote.is_none()
                    && !s.parts.is_empty()
                    && !s.cancelled
                    && s.terminal_status.is_none()
            })
            .map(|s| Saved {
                id: s.id.clone(),
                parts: s.parts.clone(),
                part_sizes: s.part_sizes.clone(),
                total_size: s.total_size,
                content_id: s.content_id.clone(),
                title: s.title.clone(),
                package_type: s.package_type.clone(),
                package_fingerprint: s.package_fingerprint.clone(),
                ps5_mgmt_addr: s.ps5_mgmt_addr.clone(),
                serve_only: s.serve_only,
                staging_path: s.staging_path.clone(),
                created_at_unix: s.created_at_unix,
                last_activity_unix: s.last_activity_unix,
            })
            .collect();
        let Ok(json) = serde_json::to_vec(&saved) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, path)) {
            crate::log_warn!("pkg-host: could not save install sessions: {e}");
        }
    }

    /// The saved sessions still worth serving: not past the session age limit, every part
    /// still present at its recorded size.
    pub(super) fn load() -> HashMap<String, InstallSession> {
        path().map(|p| load_from(&p)).unwrap_or_default()
    }

    pub(super) fn load_from(path: &std::path::Path) -> HashMap<String, InstallSession> {
        let Ok(bytes) = std::fs::read(path) else {
            return HashMap::new();
        };
        let saved: Vec<Saved> = serde_json::from_slice(&bytes).unwrap_or_default();
        let now = now_unix();
        let max_age = pkg_session_max_age_sec();
        saved
            .into_iter()
            .filter(|s| now.saturating_sub(s.last_activity_unix) < max_age)
            .filter(|s| {
                s.parts.len() == s.part_sizes.len()
                    && s.parts.iter().zip(&s.part_sizes).all(|(p, &size)| {
                        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == size)
                    })
            })
            .map(|s| {
                let session = InstallSession {
                    id: s.id.clone(),
                    parts: s.parts,
                    part_sizes: s.part_sizes,
                    total_size: s.total_size,
                    content_id: s.content_id,
                    title: s.title,
                    package_type: s.package_type,
                    package_fingerprint: s.package_fingerprint,
                    ps5_mgmt_addr: s.ps5_mgmt_addr,
                    task_id: None,
                    err_code: 0,
                    detail: String::new(),
                    cancelled: false,
                    created_at_unix: s.created_at_unix,
                    last_activity_unix: now,
                    staging_path: s.staging_path,
                    terminal_status: None,
                    launchable: None,
                    serve_only: s.serve_only,
                    notified_console: false,
                    install_start_free_bytes: None,
                    progress_consumed_bytes: 0,
                    last_progress_unix: None,
                    stalled: false,
                    accepted_unverified: false,
                    requests_served: 0,
                    last_rate_log_unix: 0,
                    bytes_served: 0,
                    transfer_bytes: 0,
                    transfer: TransferCoverage::new(s.total_size),
                    dpi_ok: None,
                    dpi_rc: None,
                    dpi_detail: String::new(),
                    remote: None,
                };
                (s.id, session)
            })
            .collect()
    }
}

/// Where uploaded packages land before a stream install serves them. Beside the
/// engine's other scratch state; each upload gets a UUID subdir so concurrent
/// uploads and the same filename don't collide.
fn pkg_upload_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("ps5upload-pkg-upload")
}

/// Browser uploads are temporary engine-side staging, not a package library.
/// A normal client deletes them after Stream/fallback completes; this sweep
/// catches browser crashes and engine restarts without racing any realistic
/// active upload or install.
const PKG_UPLOAD_STALE_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

fn cleanup_stale_pkg_uploads() {
    let Ok(entries) = std::fs::read_dir(pkg_upload_dir()) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= PKG_UPLOAD_STALE_AGE);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn pkg_upload_path(id: &str) -> Option<std::path::PathBuf> {
    Uuid::parse_str(id)
        .ok()
        .map(|id| pkg_upload_dir().join(id.to_string()))
}

/// Basename only, with traversal and separators stripped — the client controls
/// this string, and it becomes a path segment.
fn sanitize_pkg_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "package.pkg".to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_install_package_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".pkg") || lower.ends_with(".fpkg")
}

/// POST /api/pkg/upload — receive a .pkg from the browser and stage it on the
/// engine's own filesystem, returning the path.
///
/// This is the one piece the self-hosted web UI / Docker engine was missing for
/// stream install. The desktop client hands `install/start` a path to a file on
/// the same machine as the engine; a browser has no such path. It uploads the
/// bytes here, gets back an engine-side path, and drives the identical
/// serve_only + DPI flow the desktop uses — no console-side staging.
///
/// Streamed to disk field-by-field, never buffered whole: packages run to tens
/// of gigabytes.
async fn pkg_upload_handler(mut form: axum::extract::Multipart) -> Response<Body> {
    use tokio::io::AsyncWriteExt;

    cleanup_stale_pkg_uploads();
    let id = Uuid::new_v4();
    let dir = pkg_upload_dir().join(id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": format!("could not create upload dir: {e}") }),
        );
    }

    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return json_response(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({ "error": format!("malformed upload: {e}") }),
                );
            }
        };
        let filename = sanitize_pkg_filename(field.file_name().unwrap_or("package.pkg"));
        // The parser validates CNT/FIH magic; the suffix is only a picker hint.
        if !is_install_package_filename(&filename) {
            continue;
        }
        let dest = dir.join(&filename);
        let mut file = match tokio::fs::File::create(&dest).await {
            Ok(f) => f,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({ "error": format!("could not open {}: {e}", dest.display()) }),
                );
            }
        };
        let mut field = field;
        let mut written: u64 = 0;
        loop {
            match field.chunk().await {
                Ok(Some(bytes)) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        drop(file);
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        return json_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            serde_json::json!({ "error": format!("write failed: {e}") }),
                        );
                    }
                    written += bytes.len() as u64;
                }
                Ok(None) => break,
                Err(e) => {
                    drop(file);
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        serde_json::json!({ "error": format!("upload stream error: {e}") }),
                    );
                }
            }
        }
        if let Err(e) = file.flush().await {
            drop(file);
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("flush failed: {e}") }),
            );
        }
        return json_response(
            StatusCode::OK,
            serde_json::json!({
                "upload_id": id.to_string(),
                "path": dest.to_string_lossy(),
                "size": written,
                "filename": filename,
            }),
        );
    }

    // No .pkg field arrived — clean up the empty dir.
    let _ = tokio::fs::remove_dir_all(&dir).await;
    json_response(
        StatusCode::BAD_REQUEST,
        serde_json::json!({ "error": "no .pkg or .fpkg file in the upload" }),
    )
}

#[derive(Debug, Serialize)]
struct PkgUploadDeleteResponse {
    upload_id: String,
    removed: bool,
}

/// DELETE /api/pkg/upload/:id — discard browser-side temporary staging.
/// The UUID-only lookup makes it impossible for this endpoint to remove an
/// arbitrary host path even if a caller supplies separators or `..`.
async fn pkg_upload_delete_handler(AxumPath(id): AxumPath<String>) -> Response<Body> {
    let Some(path) = pkg_upload_path(&id) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "invalid upload id" }),
        );
    };
    let removed = match tokio::fs::remove_dir_all(&path).await {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("could not remove upload: {e}") }),
            )
        }
    };
    json_response(
        StatusCode::OK,
        serde_json::json!(PkgUploadDeleteResponse {
            upload_id: id,
            removed,
        }),
    )
}

fn json_response(code: StatusCode, body: serde_json::Value) -> Response<Body> {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Release a serve-only pkg-host session its install no longer needs: mark it
/// cancelled, so the duplicate-session guard lets the same package install
/// again and the session is not persisted across a restart. Only call once the
/// console has finished pulling (or stalled) — a cancelled session answers 410,
/// which makes PlayGo discard a partial download. Returns whether it existed.
pub(crate) fn release_serve_session(
    sessions: &Mutex<HashMap<String, InstallSession>>,
    sid: &str,
) -> bool {
    let mut map = sessions.lock().unwrap_or_else(|e| e.into_inner());
    let found = mark_released(&mut map, sid);
    // Persist now. Sessions used to be written only when a NEW one was
    // created, so a released session stayed "live" on disk and came back on
    // the next engine restart — blocking a re-install of that package.
    if found {
        persist::save(&map);
    }
    found
}

/// Pure half of `release_serve_session`: mark the session cancelled.
fn mark_released(map: &mut HashMap<String, InstallSession>, sid: &str) -> bool {
    match map.get_mut(sid) {
        Some(s) => {
            s.cancelled = true;
            true
        }
        None => false,
    }
}

/// `POST /api/pkg/payload-restore` — (re)send the bundled MAIN ps5upload
/// payload to the console's loader (:9021). The desktop app does this itself
/// (it holds the ELF and can open the socket); the self-hosted web UI cannot,
/// so it asks the engine — which is what `ensurePayloadCurrent` /
/// `restoreMainPayload` call in the browser build to redeploy a stale or dead
/// helper. Install-time restore is handled inside the unified install state
/// machine; this route is for the general payload-freshness path only.
///
/// Never an HTTP error on a failed send: the browser caller runs it
/// best-effort and a failed restore is information to log, not a reason to
/// surface a red error over whatever prompted the refresh.
#[derive(serde::Deserialize)]
struct PayloadRestoreRequest {
    ps5_addr: String,
}

async fn payload_restore_handler(Json(req): Json<PayloadRestoreRequest>) -> Response<Body> {
    let ps5_ip = strip_host_port(req.ps5_addr.trim()).trim().to_string();
    if ps5_ip.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"ok": false, "error": "ps5_addr is required"}),
        );
    }
    let res = tokio::task::spawn_blocking(move || {
        use ps5upload_core::payload_lifecycle as pl;
        let bytes = crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Payload)?;
        pl::send_elf_to_loader(
            &ps5_ip,
            pl::PS5_LOADER_PORT,
            &bytes,
            pl::LoaderImage::Ps5Upload,
        )
    })
    .await;
    match res {
        Ok(Ok(bytes)) => {
            crate::log_info!("payload-restore: sent {bytes} bytes");
            json_response(
                StatusCode::OK,
                serde_json::json!({"ok": true, "bytes": bytes}),
            )
        }
        Ok(Err(e)) => {
            crate::log_warn!("payload-restore: {e}");
            json_response(
                StatusCode::OK,
                serde_json::json!({"ok": false, "bytes": 0, "error": e}),
            )
        }
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"ok": false, "error": format!("payload-restore task failed: {e}")}),
        ),
    }
}

pub fn router(state: PkgInstallStateHandle) -> Router {
    Router::new()
        // Packages routinely exceed the app-wide 64 MiB JSON/form limit.
        // Multipart is consumed incrementally above, so disabling buffering's
        // size guard for this one route does not put the package in RAM.
        .route(
            "/api/pkg/upload",
            post(pkg_upload_handler).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/pkg/upload/{id}", delete(pkg_upload_delete_handler))
        .route("/api/pkg/parse", post(parse_handler))
        .route("/api/pkg/parse-split", post(parse_split_handler))
        // Read-only UFS2 image inspector for .ffpkg / .ufs files.
        // Lets the renderer surface "what's in this image?" before
        // a multi-GB upload. See ps5upload_pkg::ufs2 for the parser.
        .route("/api/ffpkg/inspect", post(inspect_handler))
        // Extract a file or subtree from a local .ffpkg to a local
        // dir. Useful for grabbing a single asset without uploading
        // the whole image to the PS5 first.
        .route("/api/ffpkg/extract", post(extract_handler))
        .route("/api/pkg/remote/probe", post(remote_probe_handler))
        // Unified install (spec 2): one endpoint owns resolve → deliver →
        // install (through the :9115 daemon) → verify → record. Replaces the
        // old install/start + dpi-* surface. Status is per-job; history is a
        // per-console log.
        .route("/api/pkg/install", post(crate::install::install_handler))
        .route(
            "/api/pkg/install/status",
            get(crate::install::install_status_handler),
        )
        .route(
            "/api/pkg/install/history",
            get(crate::install::install_history_handler),
        )
        .route("/api/pkg/install/sessions", get(install_sessions_handler))
        .route("/api/pkg/install/cancel", post(install_cancel_handler))
        .route("/api/pkg/installed", get(installed_pkg_inventory_handler))
        // "Do you already have this?" answered by the same artifact matching the
        // install tracker uses, so the UI and the completion check can't
        // disagree. Read-only; safe to poll from the package list.
        .route("/api/pkg/install/preflight", get(install_preflight_handler))
        .route("/api/pkg/payload-restore", post(payload_restore_handler))
        // The session UUID is the lookup key. We allow ANY {filename} so the
        // URL can carry the pkg's canonical `<ContentID>.pkg` name that
        // Sony's installer cross-checks against the pkg header. Without
        // this Sony rejects with 0x80B21106 on user-renamed pkgs (file
        // header says "FOO" but URL ends in "bar.pkg" — installer treats
        // them as inconsistent). A name ending in `.crc` is the console
        // asking for the package's PlayGo CRC table (#319); see serve_handler.
        .route("/pkg-host/{session}/{filename}", get(serve_handler))
        // Short alias for a link too long for the PS5's installer; answers
        // with a redirect to the real link. Under /pkg-host/ because that is
        // the one prefix the console is allowed to reach. See
        // `shorten_for_installer`.
        .route("/pkg-host/link/{file}", get(link_redirect_handler))
        .with_state(state)
}

// ─── /api/pkg/installed ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InstalledPkgArtifact {
    /// "base" | "patch" | "dlc".
    pub kind: String,
    pub path: String,
    pub size: u64,
    /// Same bounded sampled-BLAKE3 identity emitted by the local parser.
    pub fingerprint: String,
    /// Best-effort header ContentID. Useful for legacy staged rows whose
    /// fingerprint was not cached.
    pub content_id: String,
}

#[derive(Debug, Serialize)]
pub struct InstalledPkgInventory {
    pub title_id: String,
    pub artifacts: Vec<InstalledPkgArtifact>,
}

#[derive(Debug, Deserialize)]
struct InstalledPkgQuery {
    addr: String,
    title_id: String,
}

fn valid_title_id(title_id: &str) -> bool {
    let b = title_id.as_bytes();
    b.len() == 9
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4..].iter().all(u8::is_ascii_digit)
}

fn installed_storage_roots(addr: &str) -> Vec<String> {
    let mut roots = vec![String::new()];
    if let Ok(vols) = ps5upload_core::volumes::list_volumes(addr) {
        roots.extend(
            vols.volumes
                .into_iter()
                .filter(|v| v.path.starts_with("/mnt/ext"))
                .map(|v| v.path),
        );
    }
    roots
}

fn artifact_for_file(addr: &str, kind: &str, path: String, size: u64) -> InstalledPkgArtifact {
    let fingerprint = package_fingerprint_from_reader(size, |off, len| {
        ps5upload_core::fs_ops::fs_read(addr, &path, off, len).ok()
    })
    .unwrap_or_default();
    let content_id = metadata_from_reader(|off, len| {
        ps5upload_core::fs_ops::fs_read(addr, &path, off, len).ok()
    })
    .map(|m| m.content_id)
    .unwrap_or_default();
    InstalledPkgArtifact {
        kind: kind.to_string(),
        path,
        size,
        fingerprint,
        content_id,
    }
}

fn named_pkg_in_dir(
    addr: &str,
    kind: &str,
    dir: &str,
    filename: &str,
) -> Option<InstalledPkgArtifact> {
    let listing = ps5upload_core::fs_ops::list_dir(
        addr,
        dir,
        ps5upload_core::fs_ops::ListDirOptions::default(),
    )
    .ok()?;
    let entry = listing
        .entries
        .into_iter()
        .find(|e| e.kind == "file" && e.name == filename && e.size > 0)?;
    Some(artifact_for_file(
        addr,
        kind,
        format!("{dir}/{filename}"),
        entry.size,
    ))
}

fn collect_dlc_pkgs(addr: &str, dir: &str, depth: u8, out: &mut Vec<InstalledPkgArtifact>) {
    // Add-on layouts differ between PS4 BC and PS5-native packages. Walk a
    // small bounded tree below /user/addcont/<title_id>; never escape it and
    // cap the result so malformed directory graphs cannot grow work forever.
    if depth > 4 || out.len() >= 256 {
        return;
    }
    let Ok(listing) = ps5upload_core::fs_ops::list_dir(
        addr,
        dir,
        ps5upload_core::fs_ops::ListDirOptions::default(),
    ) else {
        return;
    };
    for entry in listing.entries {
        if out.len() >= 256 {
            break;
        }
        let path = format!("{dir}/{}", entry.name);
        if entry.kind == "file"
            && entry.size > 0
            && entry.name.to_ascii_lowercase().ends_with(".pkg")
        {
            out.push(artifact_for_file(addr, "dlc", path, entry.size));
        } else if entry.kind == "dir" && entry.name != "." && entry.name != ".." {
            collect_dlc_pkgs(addr, &path, depth + 1, out);
        }
    }
}

fn installed_pkg_inventory(addr: &str, title_id: &str) -> InstalledPkgInventory {
    let mut artifacts = Vec::new();
    for root in installed_storage_roots(addr) {
        if let Some(a) = named_pkg_in_dir(
            addr,
            "base",
            &format!("{root}/user/app/{title_id}"),
            "app.pkg",
        ) {
            artifacts.push(a);
        }
        if let Some(a) = named_pkg_in_dir(
            addr,
            "patch",
            &format!("{root}/user/patch/{title_id}"),
            "patch.pkg",
        ) {
            artifacts.push(a);
        }
        collect_dlc_pkgs(
            addr,
            &format!("{root}/user/addcont/{title_id}"),
            0,
            &mut artifacts,
        );
    }
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    artifacts.dedup_by(|a, b| a.path == b.path);
    InstalledPkgInventory {
        title_id: title_id.to_string(),
        artifacts,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstalledArtifactCheck {
    Match,
    Absent,
    Different,
    Unsupported,
}

fn package_kind_for_type(package_type: &str) -> &'static str {
    if package_type.ends_with("DP") {
        "patch"
    } else if package_type.ends_with("AC") {
        "dlc"
    } else {
        "base"
    }
}

/// Whether an on-console artifact is the package we just installed.
///
/// PS4 packages land as the same bytes we served, so a sampled fingerprint
/// (or exact size) is authoritative. A PS5 debug FPKG is a FIH container:
/// Sony writes the *inner* image as `app.pkg`, whose size and fingerprint
/// differ from the file we hosted. Hardware-observed on Minecraft
/// (outer 1_345_936_761 / fp f9545e9c… vs inner 1_333_460_992 / fp c08d913d…).
/// For those, matching ContentID on the category-specific artifact is the
/// proof the title landed.
fn artifact_identity_matches(
    artifact: &InstalledPkgArtifact,
    wanted_kind: &str,
    content_id: &str,
    expected_size: u64,
    expected_fingerprint: &str,
    package_type: &str,
) -> bool {
    if !expected_fingerprint.is_empty() && artifact.fingerprint == expected_fingerprint {
        return true;
    }
    if package_type.starts_with("PS5") {
        return !content_id.is_empty() && artifact.content_id == content_id && artifact.size > 0;
    }
    if expected_size > 0 && artifact.size != expected_size {
        return false;
    }
    // DLC ContentIDs are per-add-on and therefore useful even for legacy
    // rows. Base/update ContentIDs are shared, so size is their fallback.
    if wanted_kind == "dlc" && !content_id.is_empty() && !artifact.content_id.is_empty() {
        return artifact.content_id == content_id;
    }
    expected_fingerprint.is_empty()
}

/// Verify the exact install target, not merely the shared base title.
///
/// A patch install used to call `verify_launchable(content_id)`, which sees the
/// already-installed base `app.pkg` and immediately returns success before
/// Sony has written `patch.pkg`. DLC had the same problem. We instead inspect
/// the category-specific artifact and compare the sampled package identity (or
/// size/content id for legacy callers).
fn verify_installed_artifact(
    addr: &str,
    content_id: &str,
    package_type: &str,
    expected_size: u64,
    expected_fingerprint: &str,
) -> InstalledArtifactCheck {
    // Sony's in-flight placeholder title id means the install never got
    // rewritten with the real one. That is a BROKEN install, not an
    // unverifiable one: the tile exists but launches Sony's CloudClientApp
    // instead of the user's eboot. It must map to Different (a real failure)
    // and not Unsupported, which the poll treats as "can't tell, assume fine"
    // and would hand the user a broken tile with a green tick.
    if ps5upload_core::pkg_install::is_placeholder_content_id(content_id) {
        return InstalledArtifactCheck::Different;
    }
    let Some(title_id) = ps5upload_core::pkg_install::title_id_from_content_id(content_id) else {
        return InstalledArtifactCheck::Unsupported;
    };
    let wanted_kind = package_kind_for_type(package_type);
    let inventory = installed_pkg_inventory(addr, &title_id);
    let candidates: Vec<_> = inventory
        .artifacts
        .iter()
        .filter(|a| a.kind == wanted_kind)
        .collect();
    if candidates.is_empty() {
        return InstalledArtifactCheck::Absent;
    }
    let matched = candidates.iter().any(|a| {
        artifact_identity_matches(
            a,
            wanted_kind,
            content_id,
            expected_size,
            expected_fingerprint,
            package_type,
        )
    });
    if matched {
        InstalledArtifactCheck::Match
    } else {
        InstalledArtifactCheck::Different
    }
}

fn remote_pkg_size(addr: &str, path: &str) -> Option<u64> {
    let (parent, name) = path.rsplit_once('/')?;
    let parent = if parent.is_empty() { "/" } else { parent };
    let listing = ps5upload_core::fs_ops::list_dir(
        addr,
        parent,
        ps5upload_core::fs_ops::ListDirOptions::default(),
    )
    .ok()?;
    listing
        .entries
        .into_iter()
        .find(|e| e.kind == "file" && e.name == name)
        .map(|e| e.size)
}

fn remote_pkg_identity(addr: &str, path: &str) -> (u64, String) {
    let size = remote_pkg_size(addr, path).unwrap_or(0);
    let fingerprint = package_fingerprint_from_reader(size, |off, len| {
        ps5upload_core::fs_ops::fs_read(addr, path, off, len).ok()
    })
    .unwrap_or_default();
    (size, fingerprint)
}

async fn installed_pkg_inventory_handler(Query(q): Query<InstalledPkgQuery>) -> Response<Body> {
    if !valid_title_id(&q.title_id) {
        return json_err(StatusCode::BAD_REQUEST, "invalid title_id");
    }
    // Normalize the port like every other entry point: the inventory is read
    // over :9114, and an address carrying a different port doesn't fail — it
    // reports an empty console.
    let addr = normalize_mgmt_addr(&q.addr);
    let title_id = q.title_id;
    match tokio::task::spawn_blocking(move || {
        // Reachability, checked separately. An unreadable console and a
        // console with nothing installed both produce an empty artifact list,
        // and callers render the second as "not installed" — a wrong verdict
        // about a title that is sitting right there. Say which one it is.
        let reachable = ps5upload_core::volumes::list_volumes(&addr).is_ok();
        (reachable, installed_pkg_inventory(&addr, &title_id))
    })
    .await
    {
        Ok((false, _)) => json_err(
            StatusCode::BAD_GATEWAY,
            "the console didn't answer, so its installed packages couldn't be read",
        ),
        Ok((true, inventory)) => json_ok(&inventory),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("installed pkg inventory task failed: {e}"),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct PreflightQuery {
    pub addr: String,
    pub content_id: String,
    #[serde(default)]
    pub package_type: String,
    #[serde(default)]
    pub expected_size: u64,
    #[serde(default)]
    pub package_fingerprint: String,
}

/// What the console already has, for the package the user is about to install.
///
/// The engine's own artifact matching is the answer to "is this installed?"
/// deliberately lives HERE rather than in the client: the client used to compare
/// fingerprints itself and disagreed with the engine, so a PS5 FPKG could be
/// reported as not-installed in the UI while the engine's own completion check
/// considered it installed (Sony writes a debug package's *inner* image as
/// `app.pkg`, so its size and hash can never equal the outer container's).
#[derive(Debug, Serialize)]
pub struct InstallPreflightResponse {
    /// `installed`                 — these exact bytes are already on the console
    /// `different_version_installed` — the title has this kind of artifact, but not this build
    /// `base_missing`              — a patch/add-on with no base game to apply to
    /// `not_installed`             — nothing for this title in this category
    /// `unknown`                   — the console couldn't be read; never act on this
    pub state: &'static str,
    pub title_id: String,
    /// Human-readable one-liner for the UI; empty when nothing needs saying.
    pub detail: String,
    /// The console's artifacts for this title, so the UI can show what IS there.
    pub installed_artifacts: Vec<InstalledPkgArtifact>,
    /// The version currently installed, when the console reports one.
    pub installed_version: Option<String>,
    /// `category` from the package being asked about (`gd`, `gp`, `ac`).
    pub category: String,
}

fn install_preflight(
    addr: &str,
    content_id: &str,
    package_type: &str,
    expected_size: u64,
    expected_fingerprint: &str,
) -> InstallPreflightResponse {
    let title_id =
        ps5upload_core::pkg_install::title_id_from_content_id(content_id).unwrap_or_default();
    let category = if package_type.ends_with("DP") {
        "gp"
    } else if package_type.ends_with("AC") {
        "ac"
    } else {
        "gd"
    };
    let mut resp = InstallPreflightResponse {
        state: "unknown",
        title_id: title_id.clone(),
        detail: String::new(),
        installed_artifacts: Vec::new(),
        installed_version: None,
        category: category.to_string(),
    };
    if title_id.is_empty() {
        resp.state = "unknown";
        resp.detail = "the package's content id has no title id to look up".into();
        return resp;
    }
    // Reachability, checked BEFORE the inventory is trusted. The inventory
    // swallows every read error into an empty artifact list, so an unreachable
    // console is indistinguishable from a console with nothing installed — and
    // `not_installed` is the one answer that must never be wrong here, because a
    // caller reads it as "there is nothing to replace". A volume list is the
    // cheapest frame that answers "is this console there at all".
    if ps5upload_core::volumes::list_volumes(addr).is_err() {
        resp.state = "unknown";
        resp.detail =
            "the console didn't answer, so its installed packages couldn't be read".into();
        return resp;
    }
    let inventory = installed_pkg_inventory(addr, &title_id);
    resp.installed_artifacts = inventory.artifacts.clone();
    resp.installed_version = read_installed_app_ver(&normalize_mgmt_addr(addr), &title_id)
        .filter(|v| !v.trim().is_empty());

    // A patch or add-on applies to a base that must already be there. Sony
    // accepts the request without one and then writes nothing, which used to
    // surface as a 10-minute stall — the answer is knowable now, so say it now.
    if category != "gd" {
        let base_present = inventory.artifacts.iter().any(|a| a.kind == "base");
        if !base_present {
            resp.state = "base_missing";
            resp.detail =
                "the base game isn't installed, so this can't be applied (Sony accepts the request and then writes nothing)".into();
            return resp;
        }
    }

    resp.state = match verify_installed_artifact(
        addr,
        content_id,
        package_type,
        expected_size,
        expected_fingerprint,
    ) {
        InstalledArtifactCheck::Match => "installed",
        InstalledArtifactCheck::Different => "different_version_installed",
        InstalledArtifactCheck::Absent => "not_installed",
        InstalledArtifactCheck::Unsupported => "unknown",
    };
    resp.detail = match resp.state {
        "installed" => "this exact package is already installed".into(),
        "different_version_installed" => match resp.installed_version.as_deref() {
            Some(v) => format!("a different version is installed (version {v} on the console)"),
            None => "a different version of this content is installed".into(),
        },
        "unknown" => "the console's installed packages couldn't be read".into(),
        _ => String::new(),
    };
    resp
}

async fn install_preflight_handler(Query(q): Query<PreflightQuery>) -> Response<Body> {
    let addr = q.addr;
    let content_id = q.content_id;
    let package_type = q.package_type;
    let res = tokio::task::spawn_blocking(move || {
        // Two blocking FS probes plus an app.db read: keep them off the reactor,
        // this endpoint is polled from the UI.
        install_preflight(
            &addr,
            &content_id,
            &package_type,
            q.expected_size,
            &q.package_fingerprint,
        )
    })
    .await;
    match res {
        Ok(resp) => json_ok(&resp),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("preflight task failed: {e}"),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    /// Local path to the `.ffpkg` image.
    pub ffpkg_path: String,
    /// Slash-separated path inside the image. Empty string = whole
    /// image (the root directory).
    #[serde(default)]
    pub inner_path: String,
    /// Local directory to write into. Created if missing.
    pub dest_dir: String,
}

async fn extract_handler(Json(req): Json<ExtractRequest>) -> Response<Body> {
    let ffpkg = req.ffpkg_path.clone();
    let inner = req.inner_path.clone();
    let dest = req.dest_dir.clone();
    let res = tokio::task::spawn_blocking(move || {
        extract_from_ffpkg(
            std::path::Path::new(&ffpkg),
            &inner,
            std::path::Path::new(&dest),
        )
    })
    .await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("extract: {e}")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

async fn inspect_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    // spawn_blocking because the parser is sync I/O on a potentially
    // multi-GB image. With ~10 root entries (one inode read each) the
    // call typically finishes in a few hundred ms; without spawn_blocking
    // a slow disk could stall the axum reactor.
    let path = req.path.clone();
    let res = tokio::task::spawn_blocking(move || inspect_ffpkg(std::path::Path::new(&path))).await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("inspect: {e}")),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

// ─── /api/pkg/parse ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ParseRequest {
    pub path: String,
}

async fn parse_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    // spawn_blocking: parse_pkg is synchronous disk I/O (reads the pkg
    // header); a slow/remote path would otherwise stall the async reactor.
    // Mirrors inspect_handler / extract_handler below — parse_handler was
    // the lone holdout still blocking inline.
    let res = tokio::task::spawn_blocking(move || parse_pkg(std::path::Path::new(&req.path))).await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("parse failed: {e}")),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("parse task panicked: {e}"),
        ),
    }
}

async fn parse_split_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    if crate::remote::path::is_remote(&req.path) {
        return parse_remote_handler(&req.path).await;
    }
    let res =
        tokio::task::spawn_blocking(move || parse_split_pkg(std::path::Path::new(&req.path))).await;
    match res {
        Ok(Ok(meta)) => json_ok(&meta),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, &format!("split parse failed: {e}")),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("split parse task panicked: {e}"),
        ),
    }
}

/// A package on a saved server, described the way a local one is (a single part).
#[cfg(not(target_os = "android"))]
async fn parse_remote_handler(remote_path: &str) -> Response<Body> {
    match open_remote_package(remote_path).await {
        Ok((_remote, total_size, head, fingerprint, name)) => {
            let head = stream_metadata(head, fingerprint, total_size, &name, None);
            json_ok(&SplitPkgMetadata {
                parts: vec![PathBuf::from(remote_path)],
                part_sizes: vec![total_size],
                total_size,
                head,
            })
        }
        Err(e) => json_err(StatusCode::BAD_REQUEST, &e),
    }
}

#[cfg(target_os = "android")]
async fn parse_remote_handler(_remote_path: &str) -> Response<Body> {
    json_err(
        StatusCode::BAD_REQUEST,
        "reading packages on a server is not available in the Android build",
    )
}

// ─── /api/pkg/install/start ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InstallStartRequest {
    /// PS5 mgmt-port address, e.g. "192.168.1.42:9114".
    pub ps5_addr: String,
    /// Proceed with a staged re-install of an already-installed full game.
    ///
    /// Off by default because that path is destructive on failure (see the
    /// guard in `install_start_handler`). A caller that has explicitly warned
    /// the user may set it.
    #[serde(default)]
    pub allow_destructive_reinstall: bool,
    /// Skip TLS certificate verification when THIS COMPUTER downloads the
    /// package from `remote_url`. Per install, never global, default off.
    ///
    /// Has no effect on a direct install, where the console performs its own
    /// handshake and we have no say in what it accepts — the client disables
    /// the option in that mode rather than letting it silently do nothing.
    #[serde(default)]
    // Read only on platforms that can install from a link; the Android build
    // compiles the remote path out entirely but must still accept the field
    // so one client speaks to every engine.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    pub insecure_tls: bool,
    /// Either `path` (single .pkg) or `split_root` (lead `.pkg` of a
    /// split set) must be set. `split_root` triggers split-pkg
    /// detection — we look for `<root>.0`, `<root>.1`, ... siblings.
    pub path: Option<String>,
    pub split_root: Option<String>,
    /// Install straight from an HTTP(S) link: the engine fetches the package
    /// from this URL over many connections at once and re-serves it to the
    /// console from the pkg-host, so nothing is staged on PC disk or on the
    /// console. Mutually exclusive with `path` / `split_root` /
    /// `local_ps5_path`. The origin must honour byte ranges.
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Optional override for the package_type passed to BGFT. When
    /// unset we use whatever `derive_package_type(category)` returns
    /// or fall back to "PS4GD". Useful for unknown-magic PKGs where
    /// the user picks the type manually in the UI.
    pub package_type_override: Option<String>,
    /// PS5-side absolute path to a pkg already on the console's disk.
    /// When `Some`, the install URL is built as `file://{local_ps5_path}`
    /// and the HTTP-host serve_handler is not used. This is the
    /// "Tier-1" path: bytes already on PS5-local storage + ShellUI-RPC
    /// install + file:// URL = exactly what Settings → Debug Settings
    /// → Game → Package Installer does internally. Bypasses both the
    /// PlayGo HTTP-fetch authid reject (0x80B22404) and the
    /// engine-side network plumbing entirely.
    ///
    /// When `None`, falls back to the http://{lan-ip}:{port} flow —
    /// the engine hosts bytes for Sony's BGFT downloader.
    pub local_ps5_path: Option<String>,
    /// Caller-supplied content id for the staged-pkg case. When the pkg
    /// is already on the PS5 (`local_ps5_path` set) there's no PC-side
    /// file to parse, so the client passes the content id it parsed at
    /// upload time. Optional — the payload re-parses it from the staged
    /// pkg itself, so an empty value still installs.
    #[serde(default)]
    pub content_id: Option<String>,
    /// Expected exact source size and sampled identity. Current clients pass
    /// these from the local parser; older clients may omit them, in which case
    /// a staged PS5 path is sampled here before installation.
    #[serde(default)]
    pub expected_size: Option<u64>,
    #[serde(default)]
    pub package_fingerprint: Option<String>,
    /// Whether to DELETE the staged `local_ps5_path` pkg after the install
    /// reaches a terminal phase (the "Tier-1 staging cleanup"). This is the
    /// user's "Auto Delete after installation" preference. Defaults TRUE for
    /// backward-compatibility (older clients that don't send it keep the old
    /// always-clean behaviour), but the current client always sends the real
    /// setting — so "Auto Delete off" now actually KEEPS the uploaded pkg
    /// instead of the engine silently deleting it regardless.
    #[serde(default = "default_true")]
    pub delete_staging: bool,
    /// Serve-only mode for the streaming-install (Stream beta) path.
    ///
    /// When true, create + register the /pkg-host/ serving session and
    /// return its `session_id`, but DO NOT run the in-process
    /// `sceAppInstUtilInstallByPackage` frame. The caller then hands the
    /// session to `/api/pkg/dpi-direct-install`, which installs via the
    /// DPI daemon (a separate loader process) pulling bytes over HTTP.
    ///
    /// The standalone DPI process owns Sony's HTTP installer state for Stream
    /// installs. Keeping the main payload out of that call prevents a blocked
    /// proxy/pre-flight request from tying up its RPC thread and lets the DPI
    /// daemon return the real Sony error. Defaults false so the staged/normal
    /// install path is completely unchanged.
    #[serde(default)]
    pub serve_only: bool,
}

fn default_true() -> bool {
    true
}

/// Decide the session's `staging_path` — the file the terminal-phase handler
/// deletes after install. Returns `Some(path)` ONLY when the caller asked us to
/// clean up (`delete_staging`) AND a non-empty local path was supplied; `None`
/// otherwise, which KEEPS the uploaded pkg. Pulled out as a pure fn so the
/// "Auto Delete off ⇒ pkg kept" guarantee is unit-tested rather than buried in
/// the install_start flow (it was the root of a reported data-loss bug).
fn staging_path_for(local_ps5_path: &Option<String>, delete_staging: bool) -> Option<String> {
    if delete_staging {
        local_ps5_path.clone().filter(|s| !s.is_empty())
    } else {
        None
    }
}

/// Maximum number of attempts when retrying a staging cleanup that the
/// payload rejected with `fs_delete_failed`. Sony's in-process installer
/// briefly holds the staged `.pkg` file open (EBUSY / EBUSY-equivalent)
/// right after `terminal_complete` or cancellation; a single-shot
/// `fs_delete` races that window and surfaces as a scary "staging cleanup
/// failed" log even though the file would vanish a second later. We retry
/// a bounded number of times with a short backoff so the common case
/// (installer releasing the handle) succeeds without burning a slot.
const STAGING_DELETE_MAX_ATTEMPTS: u32 = 3;

/// Per-attempt sleep between staging-delete retries. Long enough for
/// Sony's installer to release its file handle on the staged pkg, short
/// enough that the spawn_blocking worker doesn't park the pool.
const STAGING_DELETE_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a staging-delete error is worth retrying. Only the bare
/// `fs_delete_failed` token (Sony's installer still holding the file
/// open) qualifies — path-not-allowed, too-many-inflight, socket
/// timeout, or cancellation won't resolve on retry and should surface
/// immediately so the user sees the real cause. Exported as a pure fn
/// so the decision can be unit-tested without a live PS5 socket.
fn is_retryable_delete_error(err_str: &str) -> bool {
    err_str.contains("fs_delete_failed")
}

/// Delete a staged `.pkg` with a bounded retry on `fs_delete_failed`.
/// The payload sends that bare token when `rm_rf` returns non-zero — on
/// FW 10.40+ this is almost always Sony's installer still holding the
/// file open moments after the install completed (or was rejected), not
/// a genuine filesystem error. Retrying mirrors elf-arsenal's
/// `wait_for_install_row` settle window.
///
/// Returns Ok(()) if the file is gone (either deleted or already absent)
/// or the last error if all attempts failed. Logs each retry at warn so
/// a wedged console is still visible. The `label` is included in logs to
/// distinguish the terminal and cancellation call sites.
fn delete_staging_with_retry(addr: &str, path: &str, label: &str) -> Result<(), String> {
    let mut last_err: Option<String> = None;
    for attempt in 1..=STAGING_DELETE_MAX_ATTEMPTS {
        match ps5upload_core::fs_ops::fs_delete_with_timeout(
            addr,
            path,
            Some(std::time::Duration::from_secs(10)),
        ) {
            Ok(()) => {
                if attempt > 1 {
                    crate::log_info!(
                        "staging cleaned after retry: label={} addr={} path={} attempts={}",
                        label,
                        addr,
                        path,
                        attempt
                    );
                }
                return Ok(());
            }
            Err(e) => {
                let err_str = format!("{e:#}");
                // Only retry on the bare `fs_delete_failed` token — a
                // genuine path-not-allowed, too-many-inflight, or socket
                // timeout won't resolve on retry and should surface
                // immediately so the user sees the real cause.
                let retryable = is_retryable_delete_error(&err_str);
                last_err = Some(err_str);
                if !retryable || attempt == STAGING_DELETE_MAX_ATTEMPTS {
                    crate::log_warn!(
                        "staging cleanup failed: label={} addr={} path={} attempt={}/{} err={}",
                        label,
                        addr,
                        path,
                        attempt,
                        STAGING_DELETE_MAX_ATTEMPTS,
                        e
                    );
                    break;
                }
                crate::log_warn!(
                    "staging cleanup retrying: label={} addr={} path={} attempt={}/{} (installer may still hold the file) err={}",
                    label,
                    addr,
                    path,
                    attempt,
                    STAGING_DELETE_MAX_ATTEMPTS,
                    e
                );
                std::thread::sleep(STAGING_DELETE_BACKOFF);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "unknown error".to_string()))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstallStartResponse {
    pub session_id: String,
    pub url: String,
    pub task_id: i32,
    pub err_code: u32,
    pub err_message: Option<String>,
    pub detail: String,
    /// 2.2.52 diagnostics — surfaced to the UI's "Advanced details"
    /// expander for failed installs. Empty / false on older payloads.
    /// `register_path` reports which BGFT Register variant ran
    /// ("intdebug" / "regular" / "none"); `intdebug_avail` is whether
    /// the IntDebug symbol resolved at all (false = fakepkg installs
    /// effectively unsupported on this firmware regardless of cred);
    /// `kernel_rw` mirrors the payload's process-wide cred-elevation
    /// state.
    pub register_path: String,
    pub intdebug_avail: bool,
    pub kernel_rw: bool,
    /// Legacy per-tier err codes from the retired in-process cascade. The
    /// serve-only start never sets them; kept for response-shape stability.
    pub shellui_err: Option<u32>,
    pub appinst_err: Option<u32>,
    /// Always "serve-only": the unified orchestrator hands the session URL
    /// to the installer daemon.
    pub via: String,
    /// Always false for a serve-only start; the launch caution now comes
    /// from the unified install status's `may_not_launch` verdict.
    pub may_not_launch: bool,
    /// The package_type the install actually ran with, AFTER the engine's
    /// staged-pkg category parse (so a "…DP" here means "this was treated as a
    /// patch"). Lets the client recognise guarded patch/DLC hand-offs even on
    /// USB/queue/File-System paths where it sent no type, then select the safe
    /// category-aware DPI fallback.
    #[serde(default)]
    pub package_type: String,
}

pub(crate) async fn install_start_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<InstallStartRequest>,
) -> Response<Body> {
    // Only the unified install orchestrator calls this, always serve-only.
    // Refuse anything else BEFORE a session is registered so nothing leaks.
    if !req.serve_only {
        return json_err(
            StatusCode::BAD_REQUEST,
            "the in-process install path was removed; use POST /api/pkg/install",
        );
    }
    let (parts, part_sizes, total_size, head_meta, remote) =
        match resolve_parts_and_meta(&req).await {
            Ok(t) => t,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, &e),
        };

    let mut package_type = req
        .package_type_override
        .clone()
        .or_else(|| head_meta.package_type.clone())
        .unwrap_or_else(|| "PS4GD".to_string());

    // ── Data-loss guard (engine layer) ──────────────────────────────────
    // The payload refuses to fall back to a DESTRUCTIVE install tier
    // (shellui-rpc / BGFT) for a patch — but it only recognises a patch by
    // package_type ending in "DP". The Library path carries the type the
    // client parsed at upload, but the USB-scan / File-System / upload-queue
    // paths have NO PARAM.SFO category, so they default to "PS4GD": a PS4
    // patch (which shares its base game's content_id) then looked like a full
    // game and slipped past the guard, re-registering the shared content_id
    // and WIPING the installed base (hardware-confirmed: a Jak X patch deleted
    // its 3.8 GB base). Fix: when the caller didn't declare a type, read the
    // category straight from the STAGED package's CNT metadata (a bounded set
    // of small ranged reads) and derive the platform-specific type. This arms
    // the guard for an ACTUAL patch (`gp` → PS4DP/PS5DP) while leaving a
    // full-game re-install (`gd` → PS4GD/PS5GD)
    // alone — an earlier "is it already installed?" heuristic wrongly blocked
    // legitimate base re-installs, which this avoids. Bounded + fail-soft: a
    // slow/unreadable pkg just leaves the default type, never hangs the start.
    let is_local = req
        .local_ps5_path
        .as_deref()
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let type_declared = req.package_type_override.is_some() || head_meta.package_type.is_some();
    // Read the staged package's own metadata once. This used to run only when
    // the caller declared no package_type, and to keep nothing but the
    // category — so a staged install had an EMPTY content_id and a total of 0.
    // That is not cosmetic: `title_id_from_content_id("")` is empty, so
    // `title_dir_size` measured nothing, the free-space baseline is captured
    // after Sony pre-allocates, and `total` is the percentage's denominator.
    // A staged install therefore reported "0 bytes, 0%" for its entire run
    // with no ETA — measured on FW 5.10 installing a 101 GB PS5 title that
    // was in fact installing perfectly (PlayGo chunks advancing the whole
    // time). That display is almost certainly why staged installs are
    // believed not to work at all.
    let mut staged_meta: Option<ps5upload_pkg::ReaderMetadata> = None;
    if is_local {
        if let Some(local_path) = req.local_ps5_path.clone() {
            let addr = req.ps5_addr.clone();
            let parsed = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                tokio::task::spawn_blocking(move || {
                    ps5upload_pkg::metadata_from_reader(|off, len| {
                        ps5upload_core::fs_ops::fs_read(&addr, &local_path, off, len).ok()
                    })
                }),
            )
            .await;
            if let Ok(Ok(Some(meta))) = parsed {
                staged_meta = Some(meta);
            }
        }
    }
    if is_local && !type_declared {
        {
            let parsed = staged_meta.as_ref().and_then(|meta| {
                ps5upload_pkg::package_type_for_category_and_platform(
                    &meta.category,
                    &meta.platform,
                )
                .map(|pt| (meta.category.clone(), pt))
            });
            if let Some((cat, pt)) = parsed {
                if pt != package_type {
                    crate::log_info!(
                        "install guard: staged pkg category '{}' → package_type {} \
                         (was {}); {}",
                        cat,
                        pt,
                        package_type,
                        if pt.ends_with("DP") {
                            "arms the patch guard so a fallback tier can't wipe the base"
                        } else {
                            "full game — normal install cascade"
                        }
                    );
                    package_type = pt;
                }
            }
        }
    }

    // Fill the identity and size a staged install otherwise has no source
    // for. `head_meta` is populated from the PC-side file or the HTTP HEAD;
    // neither exists when the package is already sitting on the console, so
    // without this the progress tracker has no title to measure and no total
    // to divide by. Never overrides a value the caller already supplied.
    let mut head_meta = head_meta;
    let mut total_size = total_size;
    if let Some(meta) = staged_meta.as_ref() {
        if head_meta.content_id.is_empty() && !meta.content_id.is_empty() {
            crate::log_info!(
                "staged install: resolved content_id {} from the package on the console                  (progress tracking needs it)",
                meta.content_id
            );
            head_meta.content_id = meta.content_id.clone();
        }
        if head_meta.title.is_empty() && !meta.title.is_empty() {
            head_meta.title = meta.title.clone();
        }
    }
    if total_size == 0 && is_local {
        if let Some(local_path) = req.local_ps5_path.clone() {
            let addr = req.ps5_addr.clone();
            if let Ok(Ok(sz)) = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                tokio::task::spawn_blocking(move || staged_file_size(&addr, &local_path)),
            )
            .await
            {
                if sz > 0 {
                    total_size = sz;
                }
            }
        }
    }

    // ── Destructive-staged-reinstall guard ──────────────────────────────
    // A STAGED (PS5-local) install of a full game whose title is ALREADY
    // installed is destructive on failure: Sony's installer clears the old
    // title before writing the new one, so if the write then fails the user
    // is left with neither. Hardware-observed on FW 5.10 — a working
    // CUSA03474 was removed by a staged reinstall that failed with
    // 0x80B2150F, while `install/start` had returned 0 (start only registers;
    // the failure is asynchronous, so nothing upstream can react in time).
    //
    // Staged is also simply the worse path: measured 0/3 successful staged
    // installs across two consoles vs 9/9 for stream, same packages.
    //
    // So refuse this specific combination by default and point at Stream.
    // `allow_destructive_reinstall` lets a caller that has warned the user
    // proceed anyway. Stream (serve_only) is never blocked — it does not
    // take this destructive route.
    if !req.serve_only && is_local && package_type.ends_with("GD") {
        if let Some(title_id) =
            ps5upload_core::pkg_install::title_id_from_content_id(&head_meta.content_id)
        {
            if !req.allow_destructive_reinstall {
                let addr = req.ps5_addr.clone();
                let tid = title_id.clone();
                let check = tokio::task::spawn_blocking(move || {
                    ps5upload_core::pkg_install::verify_title_registered(&addr, &tid)
                })
                .await
                .unwrap_or(ps5upload_core::pkg_install::LaunchCheck::Unsupported);
                if matches!(check, ps5upload_core::pkg_install::LaunchCheck::Registered) {
                    crate::log_warn!(
                        "staged reinstall refused: {} already installed on {} (destructive on failure)",
                        title_id,
                        req.ps5_addr
                    );
                    return json_err(
                        StatusCode::CONFLICT,
                        &format!(
                            "{title_id} is already installed. Re-installing it from PS5 storage wipes the current copy before writing the new one, so a failure would leave the game gone. Use Stream install instead, or uninstall {title_id} first."
                        ),
                    );
                }
            }
        }
    }

    // ── Patch/add-on-without-base pre-flight ────────────────────────────
    // A patch or add-on whose base game isn't installed cannot install:
    // Sony's installer accepts the request, writes nothing, and the progress
    // tracker eventually calls it a stall — after ~10 MINUTES of a spinner.
    // Measured: a Toy Story 2 backport (category `gp`) aimed at a console
    // without CUSA33334 sat for 604s before reporting "0 of 15335424 bytes
    // written". An add-on (`ac`) against a missing base fails the same way.
    //
    // The answer is knowable instantly, so check it instantly. Only a
    // DEFINITE absence fails: an enumeration that errors leaves the install
    // to proceed exactly as before, because a check we cannot perform must
    // never block a legitimate install. The category-gating and the "never
    // block on uncertainty" rule live in core::preflight_patch_install.
    let category = if package_type.ends_with("DP") {
        "gp"
    } else if package_type.ends_with("AC") {
        "ac"
    } else {
        "gd"
    };
    {
        let addr = req.ps5_addr.clone();
        let content_id = head_meta.content_id.clone();
        let cat = category;
        let preflight = tokio::task::spawn_blocking(move || {
            ps5upload_core::pkg_install::preflight_patch_install(&addr, &content_id, cat)
        })
        .await
        .unwrap_or(None);
        if let Some(message) = preflight {
            crate::log_warn!(
                "install rejected: {} {} has no installed base on {}",
                category,
                head_meta.content_id,
                req.ps5_addr
            );
            return json_err(StatusCode::BAD_REQUEST, &message);
        }
    }

    // Exact source identity used by category-aware completion verification.
    // PC/stream installs already have it from the local parser. Staged installs
    // have no host-side file, so sample the PS5 copy once (two 64 KiB reads)
    // before Sony begins consuming it. Caller-provided values win.
    let mut expected_size = req.expected_size.unwrap_or(total_size);
    let mut package_fingerprint = req
        .package_fingerprint
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| head_meta.fingerprint.clone());
    if is_local && (expected_size == 0 || package_fingerprint.is_empty()) {
        if let Some(local_path) = req.local_ps5_path.as_deref() {
            let addr = req.ps5_addr.clone();
            let path = local_path.to_string();
            if let Ok((remote_size, remote_fingerprint)) =
                tokio::task::spawn_blocking(move || remote_pkg_identity(&addr, &path)).await
            {
                if expected_size == 0 {
                    expected_size = remote_size;
                }
                if package_fingerprint.is_empty() {
                    package_fingerprint = remote_fingerprint;
                }
            }
        }
    }

    // URL strategy: raw path when caller staged the pkg on PS5 disk
    // (Tier 1). On FW 9.60+ Sony's installer accepts a bare absolute
    // path WITHOUT the `file://` prefix; with the prefix it returns
    // 0x80B21106 (rejected) even on a freshly-rebooted PS5. The 6.xx
    // branch uses an HTTP loopback proxy; non-6.xx (incl. 9.60) uses
    // the bare path. Switched from `file://{path}` to raw `{path}` in
    // 2.2.54-fix-round-14 after direct hardware verification.
    let session_id = Uuid::new_v4().to_string();
    let url = match req.local_ps5_path.as_deref() {
        Some(p) if !p.is_empty() => {
            // Raw path. Sony's installer reads bytes off PS5
            // local disk; no engine-side HTTP listener needed.
            p.to_string()
        }
        _ => {
            // Pick the LAN IP this host presents to the PS5 and build the
            // /pkg-host/ URL Sony's installer will fetch. Centralised in
            // pkg_host_url_for so the direct-install (DPI) path constructs
            // an identical URL — a divergence would silently break the
            // installer's header cross-check (0x80B21106).
            let url = match pkg_host_url_for(&req.ps5_addr, &session_id, &head_meta.content_id) {
                Ok(u) => u,
                Err(e) => {
                    let ps5_host_only = strip_host_port(&req.ps5_addr);
                    return json_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("could not determine local LAN IP for PS5 {ps5_host_only}: {e}"),
                    );
                }
            };
            url
        }
    };

    let session = InstallSession {
        id: session_id.clone(),
        parts,
        part_sizes,
        total_size: expected_size,
        content_id: head_meta.content_id.clone(),
        title: if head_meta.title.is_empty() {
            head_meta
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("(unknown)")
                .to_string()
        } else {
            head_meta.title.clone()
        },
        package_type: package_type.clone(),
        package_fingerprint,
        // Normalize to the MANAGEMENT port, always. Every observation this
        // session makes — the on-disk artifact check, the free-space and
        // title-dir signals that decide `installed_bytes`, the Sony-log
        // verdict — goes to :9114. A caller that sends a bare IP (a script,
        // the web UI, a future client) used to get a session whose every
        // filesystem frame failed instantly: 0 ms status polls, `Absent`
        // forever, `installed_bytes: 0`, and a phase stuck on `install` until
        // the 600 s startup-stall deadline fired. Measured 2026-09-14 with a
        // portless addr while the exact package was already installed on the
        // console — the tracker could not see it. `req.ps5_addr` may also
        // arrive on the transfer port (:9113), which `mgmt_addr_for` swaps.
        ps5_mgmt_addr: normalize_mgmt_addr(&req.ps5_addr),
        task_id: None,
        err_code: 0,
        detail: String::new(),
        cancelled: false,
        created_at_unix: now_unix(),
        last_activity_unix: now_unix(),
        // staging_path drives the terminal-phase cleanup. None ⇒ pkg KEPT,
        // honouring "Auto Delete after installation" = off. See staging_path_for.
        staging_path: staging_path_for(&req.local_ps5_path, req.delete_staging),
        terminal_status: None,
        launchable: None,
        serve_only: req.serve_only,
        notified_console: false,
        install_start_free_bytes: None,
        progress_consumed_bytes: 0,
        last_progress_unix: None,
        stalled: false,
        accepted_unverified: false,
        requests_served: 0,
        last_rate_log_unix: 0,
        bytes_served: 0,
        transfer_bytes: 0,
        transfer: TransferCoverage::new(expected_size),
        dpi_ok: None,
        dpi_rc: None,
        dpi_detail: String::new(),
        remote,
    };

    // Insert *before* sending the install frame so the HTTP listener
    // is ready to serve when BGFT starts pulling immediately on its end.
    //
    // 2.2.55: opportunistic GC pass under the same lock to bound the
    // sessions map across a long-running engine.
    //
    // Two-policy GC, by design — see `gc_old_sessions` for the
    // status-poll-time fallback. Policies:
    //   1. (here, install_start) Drop sessions whose staging_path is
    //      already None AND that are older than ~half the
    //      configured max-age. staging_path == None is a strong
    //      "we already cleaned up" signal — set by the terminal and cancel
    //      paths. Aggressive prune of
    //      definitely-done sessions, while still keeping recent
    //      rows the UI may poll.
    //   2. (gc_old_sessions, status_handler) Pure age-based prune at
    //      full max-age; runs on every status poll. Catches
    //      sessions that never reached terminal (e.g. user closed
    //      the app mid-install, no cancel ever fired).
    //
    // Without this insert-time pass, a sequence of register-reject
    // failures (no status polls fire because the UI sees the
    // immediate error) would bloat the map unbounded — gc_old_sessions
    // alone wouldn't help because nothing calls status.
    let rival = {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let max_age = pkg_session_max_age_sec();
        let full_cutoff = now.saturating_sub(max_age);
        let aggressive_cutoff = now.saturating_sub(max_age / 2);
        // Aggressive prune: drop sessions that are clearly "done with
        // host-side work AND old enough." `staging_path == None`
        // *alone* doesn't qualify any more — HTTP-mode (streaming)
        // installs always have staging_path == None for their entire
        // lifetime, so the v2.16.0 gate `staging_path.is_some()` was
        // evicting in-flight multi-GB downloads at the half-max-age
        // mark.
        //
        // The correct "definitely done" signal is `terminal_status.is_
        // some()` — set by the status handler the moment BGFT reaches
        // Done or Error. Sessions in either of those states have no
        // further polls to serve and can safely be reaped early.
        //
        // ALSO apply a pure age prune at full max-age regardless of state.
        // Register-reject installs (err_code != 0) never reach the status
        // poll's Done/Error path, so their terminal_status stays None forever
        // — the `terminal_status.is_none()` arm would otherwise retain them
        // indefinitely (gc_old_sessions only runs from the status handler,
        // which a register-reject never calls). The full-age sweep here is the
        // only thing that reaps them, bounding the sessions map.
        sessions.retain(|_, s| {
            // Measured from the last sign of life, so an install that is still
            // pulling bytes is never reaped no matter how long it runs.
            let idle_since = s.last_activity_unix.max(s.created_at_unix);
            if idle_since <= full_cutoff {
                return false;
            }
            idle_since > aggressive_cutoff || s.terminal_status.is_none()
        });
        // ── Rival-session guard ──────────────────────────────────────────
        //
        // Sony keys an install by content_id, and a base, its patch and its
        // DLC all SHARE one content_id. Before this guard, starting a second
        // install for the same package left the first session in the table
        // alongside it, still holding the URL the console was actively
        // fetching. Observed live on FW 5.10: three concurrent sessions for
        // one content_id (one cancelled with 60 GiB already served), the
        // console pulling a stale one, the pkg-host answering `410 Gone`, and
        // PlayGo responding to that by DELETING the 64.8 GiB it had
        // downloaded — `[PlayGoCore][Uninstall] begin`.
        //
        // Identity here is deliberately NOT content_id. `package_fingerprint`
        // is per-PACKAGE, so a base and its patch are distinct even though
        // Sony cannot tell them apart, and a genuine retry of the SAME package
        // is recognised as the rival it is.
        let rival_info = sessions
            .values()
            .find(|s| {
                s.id != session_id
                    && !s.cancelled
                    && s.terminal_status.is_none()
                    && !s.package_fingerprint.is_empty()
                    && s.package_fingerprint == session.package_fingerprint
                    // ...and the console is ACTUALLY still pulling it.
                    //
                    // `terminal_status` is only set by the status handler, so a
                    // session nobody polls never reaches a terminal state and
                    // would block its own package forever. Hit immediately when
                    // dogfooding this guard: a session that had transferred
                    // 20,625,752,064 of 20,624,703,488 bytes — past 100% — still
                    // refused the next install. Recent serving activity is the
                    // signal that matters; a session no console has fetched from
                    // in RIVAL_ACTIVE_WINDOW_SEC is not a rival, whatever its
                    // bookkeeping says.
                    && now_unix().saturating_sub(s.last_activity_unix)
                        < RIVAL_ACTIVE_WINDOW_SEC
            })
            .map(|r| {
                (
                    r.id.clone(),
                    r.bytes_served,
                    r.total_size,
                    r.requests_served,
                )
            });
        // Insert ONLY when no rival exists. Nothing has been registered with
        // Sony yet — that happens further down — so refusing here costs
        // nothing and leaves the running install completely untouched.
        if rival_info.is_none() {
            sessions.insert(session_id.clone(), session.clone());
        }
        persist::save(&sessions);
        rival_info
    };
    if let Some((rid, served, total, reqs)) = rival {
        crate::log_warn!(
            "install start refused: a live session for this exact package is \
             already serving (session={} served={}/{} bytes over {} requests). \
             A second session leaves the console able to fetch a URL we may \
             later invalidate, and a pkg-host 410 makes PlayGo discard the \
             whole partial download.",
            rid,
            served,
            total,
            reqs,
        );
        return json_err(
            StatusCode::CONFLICT,
            &format!(
                "an install of this exact package is already running (session \
                 {rid}, {served} of {total} bytes transferred). Wait for it, or \
                 cancel it first."
            ),
        );
    }

    // Serve-only (Stream beta): the session + its /pkg-host/ listener are now
    // live, so the DPI daemon can pull bytes. Return WITHOUT running the
    // in-process InstallByPackage — on FW < 11 that call against an http:// URL
    // hangs the payload until the watchdog kills the helper. The client's next
    // step (`/api/pkg/dpi-direct-install`) performs the real install.
    // Serve-only by construction: the session + its /pkg-host/ listener are
    // now live and the unified install state machine hands the URL to the
    // installer daemon. The in-process InstallByPackage cascade that used to
    // follow here is gone (on FW < 11 it hung the payload on an http:// URL).
    crate::log_info!(
        "pkg_install serve-only: addr={} session={} url={} content_id={} title={:?} — skipping in-process install; DPI daemon will pull",
        req.ps5_addr,
        session_id,
        url,
        session.content_id,
        session.title,
    );
    json_ok(&InstallStartResponse {
        session_id,
        url,
        task_id: 0,
        err_code: 0,
        err_message: None,
        detail: String::new(),
        may_not_launch: false,
        register_path: "serve-only".to_string(),
        intdebug_avail: false,
        kernel_rw: false,
        shellui_err: None,
        appinst_err: None,
        via: "serve-only".to_string(),
        package_type,
    })
}

// ─── /api/pkg/install/status ─────────────────────────────────────────

/// How recently a session must have served the console to count as a rival
/// that blocks a new install of the same package.
///
/// Sized well above the gap between BGFT's range requests (sub-second at LAN
/// speed, and it retries for minutes before giving up) and well below the
/// session GC age, so a genuinely active transfer is always protected while a
/// finished-but-unpolled one never blocks its own retry.
const RIVAL_ACTIVE_WINDOW_SEC: u64 = 90;

/// Default maximum age (seconds) of an install session before the
/// engine GCs it. 2 hours covers the practical worst case: a large
/// game (~50 GB) over weak WiFi (~10 Mbps sustained) takes ~70 min;
/// add Sony's BGFT install phase (decrypt + write, ~5-15 min for a
/// 50 GB title) and the upper bound is ~90 min real-world. The 2h
/// ceiling adds buffer without growing the sessions map unbounded.
///
/// Pre-2.2.32 was 30 min — too aggressive. A user with a slow PS5
/// network would see the session GC'd while polling was still active,
/// surfacing a 404 in the UI even though BGFT was still running on
/// the PS5. The new default avoids that bite.
///
/// Override at runtime via `PS5UPLOAD_PKG_SESSION_MAX_AGE_SEC` env
/// var — power users with extreme installs (huge games + cellular
/// hotspot) can extend further; sandboxed test environments can
/// shrink to seconds.
const PKG_SESSION_MAX_AGE_SEC_DEFAULT: u64 = 2 * 60 * 60;

fn pkg_session_max_age_sec() -> u64 {
    std::env::var("PS5UPLOAD_PKG_SESSION_MAX_AGE_SEC")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 60) // sanity floor: <1min would race normal polling
        .unwrap_or(PKG_SESSION_MAX_AGE_SEC_DEFAULT)
}

// ─── progress-driven install tracker ──────────────────────────────────
//
// The fixed `pkg_verify_window_sec` window is *size-blind*: it gives up
// after ~90s and (historically) let the staging cleanup delete the uploaded
// pkg — fatal for a large title, because Sony's installer reads the pkg from
// that staged file for the *entire* install (a 25 GB game takes minutes, a
// 200 GB game far longer). Deleting it mid-install kills the install and
// leaves no game and no pkg (the reported Bloodborne data-loss).
//
// Instead of *assuming* completion after a timer, we *observe* it. Two
// physical signals the console already reports (no new payload frames):
//   • DONE  — `/user/app/<title_id>/app.pkg` appears (LaunchCheck::Registered).
//             Sony renames it into place at completion, so this is authoritative.
//   • ALIVE — bytes are landing: free space on the data volume drops
//             (FS_LIST_VOLUMES) and/or the title dir grows (FS_LIST_DIR sizes).
// We know the *expected* size up front (the pkg we just uploaded), so every
// decision is evidence-based against the real target instead of a guess.
//
// The stall deadline is *adaptive* and resets on any progress, so a slow but
// advancing install never false-fails; only a genuine flatline trips it, and
// only when both signals are flat (we track `max(free-drop, dir-size)`).

/// How many granules a package is divided into for transfer progress. Fixed, so
/// the bitmap stays 8 KiB per session whatever the package size — accuracy far
/// beyond what a progress bar needs (1/65536 of the package).
const COVERAGE_GRANULES: u64 = 1 << 16;

/// How much of a package the console has actually received.
///
/// Neither obvious metric works, and both were measured on hardware:
///
///   * **The sum of response bodies** overshoots, because Sony re-fetches
///     ranges. A 1.35 GB package served 1.53 GB, so the sum passes 100% while
///     the transfer is still going.
///   * **The furthest byte asked for** jumps to the *end of the file* on the
///     first requests: a debug FPKG is a container with a trailing index, so
///     Sony reads the tail (and the header) before the bulk. That made progress
///     read 100% at 7% transferred, which is how this was caught.
///
/// A bitmap of granules answers the question that is actually being asked —
/// "how much of this file has arrived" — and is immune to both out-of-order
/// ranges and duplicate fetches.
#[derive(Debug, Clone)]
pub struct TransferCoverage {
    /// Bytes per granule: `total / COVERAGE_GRANULES`, rounded up (min 1).
    granule: u64,
    words: Vec<u64>,
    covered: u64,
    total: u64,
}

impl TransferCoverage {
    pub fn new(total: u64) -> Self {
        let granule = total.div_ceil(COVERAGE_GRANULES).max(1);
        let granules = total.div_ceil(granule);
        Self {
            granule,
            words: vec![0u64; granules.div_ceil(64) as usize],
            covered: 0,
            total,
        }
    }

    /// Record that `[start, end]` (inclusive) has been received. Re-marking
    /// already-covered granules is free, which is what makes a re-fetch
    /// harmless.
    pub fn mark(&mut self, start: u64, end: u64) {
        if self.total == 0 {
            return;
        }
        let last = end.min(self.total - 1);
        if start > last {
            return;
        }
        // At most COVERAGE_GRANULES iterations: a response body is capped at
        // 16 MiB, and the granule size is chosen so the whole package spans
        // exactly that many granules.
        for idx in (start / self.granule)..=(last / self.granule) {
            let Some(word) = self.words.get_mut((idx / 64) as usize) else {
                break;
            };
            let bit = 1u64 << (idx % 64);
            if *word & bit == 0 {
                *word |= bit;
                self.covered = self.covered.saturating_add(self.granule);
            }
        }
    }

    /// Bytes covered, never more than the package size.
    pub fn bytes(&self) -> u64 {
        self.covered.min(self.total)
    }
}

/// Observe how many bytes the install has consumed so far, from the two
/// physical signals. Returns the larger of (free-space drop on the data
/// volume since the baseline) and (sum of the title dir's file sizes). Both
/// are best-effort: an unreadable signal contributes 0, not an error — the
/// tracker degrades to whichever signal is available.
/// Size of one staged package already on the console, by listing its parent
/// directory. A staged install has no PC-side file to stat and no HTTP HEAD to
/// read a Content-Length from, so without this its `total` stays 0 and the
/// client can render neither a percentage nor an estimate. 0 if unreadable.
fn staged_file_size(addr: &str, path: &str) -> u64 {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) if !n.is_empty() => (if d.is_empty() { "/" } else { d }, n),
        _ => return 0,
    };
    ps5upload_core::fs_ops::list_dir(addr, dir, ps5upload_core::fs_ops::ListDirOptions::default())
        .ok()
        .and_then(|l| {
            l.entries
                .into_iter()
                .find(|e| e.kind == "file" && e.name == name)
                .map(|e| e.size)
        })
        .unwrap_or(0)
}

// ─── /api/pkg/install/cancel ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub session: String,
}

#[derive(Debug, Serialize)]
pub struct CancelResponse {
    pub session_id: String,
    /// True if the cancel reached the host-side serving listener.
    /// BGFT continues running on the PS5; once it sees the HTTP stream
    /// drop it surfaces a download error in PS5 notifications.
    pub host_stopped: bool,
}

/// GET /api/pkg/install/sessions — summarise every live install session.
///
/// Read-only diagnostics. `install/status` needs a session id, so a bug
/// report could never show what the console said about an install that the
/// user had already navigated away from — the exact gap that made an
/// "install reported success but the game is broken" report unanswerable.
///
/// Deliberately omits `parts` (host-side absolute paths, which carry the
/// user's account name) and reports only the file names. Everything else here
/// is console-side state the bundle already exposes elsewhere.
async fn install_sessions_handler(State(state): State<PkgInstallStateHandle>) -> Response<Body> {
    let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    let summary: Vec<serde_json::Value> = sessions
        .values()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "content_id": s.content_id,
                "title": s.title,
                "package_type": s.package_type,
                "total_size": s.total_size,
                "part_names": s
                    .parts
                    .iter()
                    .map(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>(),
                "task_id": s.task_id,
                "err_code": s.err_code,
                "detail": s.detail,
                "cancelled": s.cancelled,
                "created_at_unix": s.created_at_unix,
                "staging_path": s.staging_path,
                "launchable": s.launchable,
                "serve_only": s.serve_only,
                "stalled": s.stalled,
                "accepted_unverified": s.accepted_unverified,
                "requests_served": s.requests_served,
                "bytes_served": s.bytes_served,
                "progress_consumed_bytes": s.progress_consumed_bytes,
                "last_progress_unix": s.last_progress_unix,
            })
        })
        .collect();
    json_ok(&summary)
}

async fn install_cancel_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<CancelRequest>,
) -> Response<Body> {
    // 2.2.55: also take() the staging path so we can delete it after
    // releasing the lock. Pre-fix the cancel path left the file on
    // PS5 disk forever and polluted Sony's installer queue. Pull both fields
    // under a single
    // lock acquisition so we never race with status_handler taking
    // the path first.
    let (cancel_ack, path_to_clean, ps5_addr) = {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get_mut(&req.session) {
            Some(s) => {
                s.cancelled = true;
                let path = s.staging_path.take();
                let addr = s.ps5_mgmt_addr.clone();
                // Persist, or the cancelled session is restored as live on the
                // next engine restart.
                persist::save(&sessions);
                (true, path, addr)
            }
            None => {
                return json_err(
                    StatusCode::NOT_FOUND,
                    &format!("no install session {}", req.session),
                )
            }
        }
    };
    if let Some(path) = path_to_clean {
        let sid = req.session.clone();
        tokio::task::spawn_blocking(move || {
            // Retry on `fs_delete_failed` — Sony's installer may briefly
            // hold the staged pkg open when a cancel lands mid-install;
            // see delete_staging_with_retry.
            match delete_staging_with_retry(&ps5_addr, &path, "cancel") {
                Ok(()) => crate::log_info!(
                    "cancel staging cleaned: session={} addr={} path={}",
                    sid,
                    ps5_addr,
                    path
                ),
                Err(e) => crate::log_warn!(
                    "cancel staging cleanup failed: session={} addr={} path={} err={}",
                    sid,
                    ps5_addr,
                    path,
                    e
                ),
            }
        });
    }
    let _ = cancel_ack;
    json_ok(&CancelResponse {
        session_id: req.session,
        host_stopped: true,
    })
}

/// Normalize whatever address the caller gave into the payload's MANAGEMENT
/// address (`ip:9114`), whatever port it arrived on.
///
/// Callers today pass `ip:9114`, but the engine's public surfaces accept a
/// bare IP, and the transfer port (`:9113`) turns up in the same slots — both
/// of which must end up on `:9114`. A wrong port here does not fail loudly:
/// every frame (FS_LIST_DIR, the artifact hash, the Sony log read) fails
/// instantly and every caller degrades to "nothing is there". That is how a
/// session with a portless address sat at `phase=install` for the full 600 s
/// stall while the exact package was already installed on the console
/// (measured 2026-09-14).
pub(crate) fn normalize_mgmt_addr(addr: &str) -> String {
    let host = strip_host_port(addr);
    if host.is_empty() {
        return addr.to_string();
    }
    // A bare IPv6 literal has to go back in brackets, or `::1:9114` is a
    // different (invalid) address than `[::1]:9114`.
    if host.contains(':') {
        format!("[{host}]:{PS5_MGMT_PORT}")
    } else {
        format!("{host}:{PS5_MGMT_PORT}")
    }
}

/// The payload's management port. Mirrors the crate-root constant of the same
/// name (`mgmt_addr_for`) and `ps5upload_core::transfer`'s.
const PS5_MGMT_PORT: u16 = 9114;

/// Read a title's installed `APP_VER`, or `None` when it cannot be read (title
/// absent, payload too old, console busy). `None` deliberately means "unknown"
/// and never "failed" — see patch_verify, which stays inconclusive on it.
pub(crate) fn read_installed_app_ver(mgmt_addr: &str, title_id: &str) -> Option<String> {
    let rows =
        ps5upload_core::diagnostics::appinfo_query(mgmt_addr, title_id, Some("APP_VER")).ok()?;
    rows.rows
        .into_iter()
        .find(|r| r.key == "APP_VER")
        .map(|r| r.val)
        .filter(|v| !v.trim().is_empty())
}

/// Wait for a patch to take effect, then say whether it did.
///
/// A PS5 install is asynchronous: on hardware the same patch landed 150 s
/// after the call returned. Polling too early is exactly how this bug was
/// twice mis-diagnosed, so this waits, and returns the moment the version
/// moves rather than burning the whole budget on a success.
pub(crate) fn verify_patch_after_install(
    mgmt_addr: &str,
    title_id: &str,
    before: Option<&str>,
    package_app_ver: &str,
) -> (ps5upload_core::patch_verify::PatchVerdict, Option<String>) {
    use ps5upload_core::patch_verify::{parse_app_ver, verify_patch_applied, PatchVerdict};
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(240);
    const STEP: std::time::Duration = std::time::Duration::from_secs(10);
    let deadline = std::time::Instant::now() + BUDGET;
    let mut latest: Option<String> = before.map(|s| s.to_string());

    // When the package does not claim a newer version there is no change to
    // wait for. Poll once and report — this also stops an ordinary
    // same-version re-install from sitting here for minutes.
    let expecting_rise = match (
        before.and_then(parse_app_ver),
        parse_app_ver(package_app_ver),
    ) {
        (Some(b), Some(p)) => p > b,
        _ => false,
    };
    if !expecting_rise {
        let after = read_installed_app_ver(mgmt_addr, title_id).or(latest.clone());
        let verdict = verify_patch_applied(before, after.as_deref(), Some(package_app_ver));
        // A single sample is enough ONLY when it is not claiming a loss.
        // Sony's overwrite removes the old update before writing the new one,
        // so mid-flight the title legitimately reads lower than it started —
        // returning `Regressed` from one poll turns that transient into a
        // reported failure. Observed exactly that way on hardware. Anything
        // that looks like a loss falls through to the wait loop, which only
        // concludes at its deadline.
        if verdict != PatchVerdict::Regressed {
            return (verdict, after);
        }
        latest = after;
    }

    loop {
        let after = read_installed_app_ver(mgmt_addr, title_id);
        if after.is_some() {
            latest = after.clone();
        }
        // `Applied` is the ONLY early exit. A reading below where we started
        // is not evidence of loss: Sony's overwrite removes the old update
        // before installing the new one, so a re-apply legitimately reports
        // the base version for a while — observed on hardware as 01.00 with
        // /user/patch/<TID> absent for over a minute, then back to 01.09.
        // Concluding "regressed" from that sample would fail a perfectly good
        // install, which is worse than the silent no-op this exists to catch.
        // Only the deadline decides a failure.
        if verify_patch_applied(before, latest.as_deref(), Some(package_app_ver))
            == PatchVerdict::Applied
        {
            return (PatchVerdict::Applied, latest);
        }
        if std::time::Instant::now() >= deadline {
            return (
                verify_patch_applied(before, latest.as_deref(), Some(package_app_ver)),
                latest,
            );
        }
        std::thread::sleep(STEP);
    }
}

/// The DPI wire protocol is one newline-terminated URI in a 4096-byte buffer.
/// Keep URL validation here rather than trusting the UI (the engine is also an
/// HTTP API), and leave room for the newline and terminating NUL.
fn valid_dpi_install_source(source: &str) -> bool {
    if source.is_empty() || source.len() > 4093 || source.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return false;
    }
    if source.starts_with('/') {
        return true;
    }
    if source.contains('#') {
        return false;
    }
    match source.parse::<axum::http::Uri>() {
        Ok(uri) => {
            matches!(uri.scheme_str(), Some("http" | "https"))
                && uri.host().is_some_and(|host| !host.is_empty())
        }
        Err(_) => false,
    }
}

// ─── /api/pkg/dpi-direct-install (streaming install beta, #81) ───────

async fn serve_handler(
    State(state): State<PkgInstallStateHandle>,
    // Two path params for the `{session}/{filename}` pattern. The
    // `filename` never participates in authentication (session UUID is the
    // only auth signal). A name ending in `.crc` selects the package's
    // PlayGo CRC table; any other name serves the package itself, keeping
    // the content_id-canonical name Sony's installer cross-checks. It MUST
    // be extracted, or axum returns 500 ErrorMissingPathParams on every
    // fetch and BGFT sees 0x80B22404 PlayGo HTTP 404 (Round-1 v2.16.1 audit).
    AxumPath((session, filename)): AxumPath<(String, String)>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response<Body> {
    // Log every PS5-side fetch attempt. Critical for diagnosing the
    // SCE_PLAYGO_ERROR_CORE_HTTP_STATUS_CODE_404_NOT_FOUND (0x80B22404)
    // class of failures: Sony's PlayGo HTTP client got a 404 from us
    // and we need to see exactly what URL/method/range it asked for
    // to figure out why. Captures the Range header and the User-Agent
    // (to identify which Sony component is making the request).
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)");
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)");
    // 2.2.55: single lock acquisition (was two — one for `contains_key`
    // logging, one for the actual `.get`). Cuts mutex pressure under
    // BGFT's parallel range fetches and removes the small TOCTOU window
    // where the session could be cancelled between the two acquisitions.
    let session_lookup = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.get(&session).cloned()
    };
    let session_known = session_lookup.is_some();
    crate::log_info!(
        "pkg-host fetch: session={} name={} known={} peer={} range={:?} user-agent={:?}",
        session,
        filename,
        session_known,
        peer.ip(),
        range,
        ua,
    );
    let session = match session_lookup {
        Some(s) if !s.cancelled => s,
        Some(_) => return plain_response(StatusCode::GONE, "install session was cancelled"),
        None => return plain_response(StatusCode::NOT_FOUND, "no such install session"),
    };

    // Source-address gate (2.9.0). The session UUID is high-entropy and
    // gated everywhere else, but the URL the engine emits to the PS5
    // (`http://{lan-ip}:{port}/pkg-host/{uuid}/file.pkg`) flows over
    // plaintext HTTP. Any host on the LAN that can passively observe
    // the PS5↔engine TCP stream (promiscuous WiFi, ARP-spoof, SOHO
    // router admin) recovers the UUID from the first GET and can then
    // hammer the URL with Range requests to drive a 16 MiB allocation
    // per call — DoS the engine, possibly OOM the whole Tauri shell.
    //
    // Defense: refuse any source IP that isn't the PS5 the session
    // belongs to. The session records `ps5_mgmt_addr` as `ip:port` at
    // install_start time; we compare the bare IP. Loopback callers are
    // allowed because dev workflows (curl against localhost, MITM
    // proxies running on the same box) need to work for debugging.
    // Strip the port + IPv6 brackets so the bare-IP compare against
    // `peer.ip().to_string()` works for both IPv4 and IPv6. See
    // strip_host_port for details — extracted so the URL-builder above
    // and this gate stay in sync (Round 1 fixed only this site; Round 2
    // caught the URL-builder mirror bug at the same site of truth).
    let expected_ip = strip_host_port(&session.ps5_mgmt_addr);
    let peer_ip = peer.ip().to_string();
    // A peer the operator already declared trusted via PS5UPLOAD_ALLOW_IP is
    // exempt from the source gate. This is what makes a NAT'd deployment work:
    // behind Docker port-mapping (Docker Desktop macOS/Windows) the console's
    // fetch arrives from the gateway address, not its own IP, so the bare-IP
    // compare below would reject every range request. The operator opens the
    // control API to that same gateway with PS5UPLOAD_ALLOW_IP; honouring it
    // here too keeps the two consistent. On a Linux `--network host` box the
    // real console IP is visible and no allowlist entry is needed.
    let peer_trusted = peer.ip().is_loopback() || parse_allow_ips_env().contains(&peer.ip());
    if !peer_trusted && !expected_ip.is_empty() && peer_ip != expected_ip {
        crate::log_warn!(
            "pkg-host fetch REJECTED: peer={} expected={} session={} \
             (set PS5UPLOAD_ALLOW_IP to this peer if the console is behind NAT)",
            peer_ip,
            expected_ip,
            session.id,
        );
        return plain_response(
            StatusCode::FORBIDDEN,
            "pkg-host URL is bound to the PS5 it was issued for",
        );
    }

    // A debug FPKG makes the console ask for `<content-id>.crc` beside the
    // package. Answering that with package bytes failed every such install
    // with 0x80b211cd (#319); serve the real CRC table or a 404 instead.
    //
    // A `.crc` fetch is a different file whose offsets mean nothing against the
    // package, so it must not be counted as transfer progress.
    let counts_as_progress = !crate::pkg_sidecar::is_crc_request(&filename);
    let source = if crate::pkg_sidecar::is_crc_request(&filename) {
        let lookup_session = session.clone();
        let lookup_name = filename.clone();
        match tokio::task::spawn_blocking(move || resolve_crc(&lookup_session, &lookup_name)).await
        {
            Ok(Ok(Some(src))) => src,
            Ok(Ok(None)) => {
                crate::log_info!(
                    "pkg-host crc: no playgo-chunk.crc in the package or beside it: session={} name={}",
                    session.id,
                    filename,
                );
                return plain_response(
                    StatusCode::NOT_FOUND,
                    "no playgo-chunk.crc for this package",
                );
            }
            Ok(Err(e)) => {
                return plain_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("crc lookup failed: {e}"),
                )
            }
            Err(e) => {
                return plain_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("crc lookup task panicked/cancelled: {e}"),
                )
            }
        }
    } else {
        ServeSource::Package
    };

    let total = source.len(session.total_size);
    let served_session_id = session.id.clone();
    let (start, end) = match parse_range_header(&headers, total) {
        Ok(r) => r,
        // RFC 9110 §15.5.17 requires `Content-Range: bytes */<total>` on a 416
        // so the client can re-derive the real size and retry; a bare 416 just
        // dead-ends Sony's fetch loop.
        Err(_) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, "0")
                .body(Body::empty())
                .unwrap_or_else(builder_failed_response)
        }
    };

    // The body is produced in bounded chunks on a blocking thread, never
    // materialised whole. Sony asks for the trailing metadata of a large
    // package in a single quarter-gigabyte range, so buffering the response
    // would mean allocating that per request (and, across concurrently
    // installing consoles, several times over). Reads stay off the async
    // reactor for the same reason as before: they are synchronous disk (or
    // proxied network) I/O and would otherwise park reactor workers.
    let len = end - start + 1;
    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = sessions.get_mut(&served_session_id) {
            // Every served range is a sign of life; without this the session
            // would age out from under a long transfer.
            active.last_activity_unix = now_unix();
            active.requests_served = active.requests_served.saturating_add(1);
            active.bytes_served = active.bytes_served.saturating_add(len);
            if counts_as_progress {
                active.transfer.mark(start, end);
                active.transfer_bytes = active.transfer.bytes();
            }
            // Periodic serve-rate line, so the CONSOLE leg is measurable the
            // same way the origin leg is. A "my install is slow" report needs
            // both numbers: a slow origin and a slow console link look
            // identical from the outside and want opposite fixes.
            //
            // Paced by TIME, not by a request count. The previous trigger
            // fired every 512 ranges, but a link install uses 32 MiB windows
            // and serves only ~100 requests for a whole 3 GiB — so on a real
            // 30-minute 1.9 MB/s report it never emitted a single line, which
            // is exactly the case it existed to explain.
            let now_s = now_unix();
            if now_s.saturating_sub(active.last_rate_log_unix) >= SERVE_RATE_LOG_SECS {
                active.last_rate_log_unix = now_s;
                let secs = now_s.saturating_sub(active.created_at_unix).max(1);
                crate::log_info!(
                    "pkg-host serve rate: session={} served={} of {} bytes over {} requests in {}s = {:.1} MB/s average",
                    active.id,
                    active.bytes_served,
                    active.total_size,
                    active.requests_served,
                    secs,
                    (active.bytes_served as f64) / (secs as f64) / 1_000_000.0,
                );
            }
        }
    }
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len.to_string())
        // Validator. ShellCore's downloader only advances to the next chunk
        // against a *cacheable* response: hyper already supplies `Date`, but
        // without `Last-Modified` the fetch loop can stall mid-package (the
        // "install fails halfway" class). The value is deliberately a fixed
        // constant rather than the pkg's mtime so it stays stable across
        // requests, engine restarts and split-part layouts — a validator that
        // changes between two range fetches of the same install looks to the
        // client like the file was replaced underneath it.
        .header(header::LAST_MODIFIED, PKG_HOST_LAST_MODIFIED);

    // Respond 206 + Content-Range whenever the body is a *subset* of the
    // file — i.e. there was a Range header, OR a bare GET whose body the
    // 16 MiB cap trimmed below `total` (`end + 1 < total`). The bare-GET
    // case is the important one: without Content-Range, a plain
    // `GET` on a >16 MiB pkg returned `200 OK` + `Content-Length: 16 MiB`
    // and no signal that more bytes existed — any non-Range consumer would
    // treat a truncated package as complete. Only a body that covers the
    // whole file gets a bare `200 OK`.
    let has_range = headers.contains_key(header::RANGE);
    let is_partial = serve_is_partial(has_range, end, total);
    if is_partial {
        builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    } else {
        builder = builder.status(StatusCode::OK);
    }

    builder
        .body(range_body(session, source, start, end))
        .unwrap_or_else(builder_failed_response)
}

/// Bytes read per chunk while streaming a range response. Small enough that a
/// quarter-gigabyte range costs kilobytes of buffer, large enough that the
/// per-chunk overhead stays negligible against disk and LAN throughput.
const PKG_HOST_STREAM_CHUNK: u64 = 1024 * 1024;

/// Stream `[start, end]` of `source` as a response body.
///
/// The producer runs on a blocking thread and hands chunks to the reactor
/// through a short channel, so a slow console applies backpressure instead of
/// letting the engine read ahead without bound. A read error ends the body
/// early: the headers are already sent by then, so the client sees a truncated
/// response and retries, which is the only signalling HTTP allows at that
/// point.
fn range_body(session: InstallSession, source: ServeSource, start: u64, end: u64) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(4);
    tokio::task::spawn_blocking(move || {
        let mut cursor = start;
        while cursor <= end {
            let stop = end.min(cursor + PKG_HOST_STREAM_CHUNK - 1);
            let read = match &source {
                ServeSource::Package => read_split_range(&session, cursor, stop),
                ServeSource::EmbeddedCrc { offset, .. } => {
                    read_split_range(&session, offset + cursor, offset + stop)
                }
                ServeSource::SiblingCrc(bytes) => {
                    Ok(bytes[cursor as usize..=stop as usize].to_vec())
                }
            };
            match read {
                Ok(b) => {
                    // A send error means the console hung up; stop reading.
                    if tx.blocking_send(Ok(axum::body::Bytes::from(b))).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
            cursor = stop + 1;
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

// ─── helpers ─────────────────────────────────────────────────────────

/// Probe an install-from-a-link URL and read its metadata over byte ranges.
///
/// The package is never downloaded to parse it: `metadata_from_reader` needs
/// only a few ranged reads (envelope, entry table, PARAM.SFO/param.json), all
/// of which the proxy satisfies from its first window. So a 100 GB link is
/// identified in about as long as one HTTP round trip.
#[cfg(not(target_os = "android"))]
async fn resolve_remote_source(
    url: &str,
    req: &InstallStartRequest,
) -> Result<ResolvedSource, String> {
    if req.path.is_some() || req.split_root.is_some() {
        return Err("remote_url cannot be combined with path or split_root".into());
    }
    if req.local_ps5_path.as_deref().is_some_and(|p| !p.is_empty()) {
        return Err("remote_url cannot be combined with local_ps5_path".into());
    }
    let owned = url.to_string();
    let insecure_tls = req.insecure_tls;
    // Probe and header-parse are blocking HTTP; keep them off the reactor so
    // concurrent installs for other consoles keep being served.
    let (remote, meta) = tokio::task::spawn_blocking(move || {
        let probe = crate::remote_pkg::RemoteSource::probe_with_options(&owned, insecure_tls)?;
        let remote = Arc::new(crate::remote_pkg::RemoteSource::new_with_options(
            owned,
            probe.total_size,
            insecure_tls,
        ));
        let read_at = |offset: u64, len: u64| -> Option<Vec<u8>> {
            if len == 0 {
                return Some(Vec::new());
            }
            remote.read_range(offset, offset + len - 1).ok()
        };
        let head = ps5upload_pkg::metadata_from_reader(read_at).ok_or_else(|| {
            "the link does not look like a PS4/PS5 package (no readable PKG header)".to_string()
        })?;
        let fingerprint =
            ps5upload_pkg::package_fingerprint_from_reader(probe.total_size, &read_at)
                .unwrap_or_default();
        Ok::<_, String>((remote, (head, fingerprint, probe)))
    })
    .await
    .map_err(|e| format!("remote package probe task panicked/cancelled: {e}"))??;

    let (head, fingerprint, probe) = meta;
    let total_size = probe.total_size;
    let display_name = if probe.filename.is_empty() {
        head.content_id.clone()
    } else {
        probe.filename.clone()
    };
    let metadata = streamed_metadata(req, head, fingerprint, total_size, &display_name);
    // `parts` stays empty: every range read is proxied, never read off disk.
    Ok((
        vec![],
        vec![],
        total_size,
        metadata,
        Some(Arc::new(RemotePkg::Http(remote))),
    ))
}

/// Metadata for a package that is streamed rather than read off local disk —
/// a link or an SMB file. One copy, because it carries the patch data-loss
/// guard below and two copies would drift.
#[cfg(not(target_os = "android"))]
fn streamed_metadata(
    req: &InstallStartRequest,
    head: ps5upload_pkg::ReaderMetadata,
    fingerprint: String,
    total_size: u64,
    display_name: &str,
) -> PkgMetadata {
    stream_metadata(
        head,
        fingerprint,
        total_size,
        display_name,
        req.package_type_override.clone(),
    )
}

/// Metadata for a package read through ranges rather than from a local file.
fn stream_metadata(
    head: ps5upload_pkg::ReaderMetadata,
    fingerprint: String,
    total_size: u64,
    display_name: &str,
    package_type_override: Option<String>,
) -> PkgMetadata {
    PkgMetadata {
        // No local file exists; the name is for display and logging only.
        path: PathBuf::from(display_name),
        size: total_size,
        kind: ps5upload_pkg::PkgKind::CntContainer,
        authenticity: head.authenticity,
        content_id: head.content_id,
        title: head.title,
        title_id: head.title_id,
        fingerprint,
        // Derive the BGFT type from the package's own category and platform,
        // exactly as the staged path does. Leaving this as the caller's
        // override alone meant a link install fell back to the "PS4GD"
        // default, so a PS5 patch pulled from a URL was typed as a PS4 full
        // game — and the payload recognises a patch only by a type ending in
        // "DP". The data-loss guard therefore never armed on the link path,
        // and a patch shares its base game's content_id: a destructive
        // fallback tier would re-register that id and WIPE the installed
        // base. That is the exact failure the guard was added for, already
        // hardware-confirmed once on the staged path.
        package_type: package_type_override.or_else(|| {
            ps5upload_pkg::package_type_for_category_and_platform(&head.category, &head.platform)
        }),
        category: head.category,
        app_ver: head.app_ver,
        platform: head.platform,
        icon_png_base64: None,
        warnings: vec![],
    }
}

#[cfg(test)]
mod remote_type_tests {
    /// A link install must derive its BGFT package type from the package,
    /// not fall back to the PS4-full-game default.
    ///
    /// The payload recognises a patch only by a type ending in "DP", and a
    /// patch shares its base game's content_id — so a mistyped patch lets a
    /// destructive fallback tier re-register that id and wipe the installed
    /// base. The staged path derives the type; the link path did not, and
    /// typed every PS5 patch pulled from a URL as "PS4GD".
    #[test]
    fn category_and_platform_decide_the_type_for_every_platform() {
        for (cat, plat, want) in [
            ("gd", "ps5", "PS5GD"),
            ("gp", "ps5", "PS5DP"),
            ("ac", "ps5", "PS5AC"),
            ("gd", "ps4", "PS4GD"),
            ("gp", "ps4", "PS4DP"),
        ] {
            assert_eq!(
                ps5upload_pkg::package_type_for_category_and_platform(cat, plat).as_deref(),
                Some(want),
                "{cat}/{plat}"
            );
        }
    }
}

#[cfg(target_os = "android")]
async fn resolve_remote_source(
    _url: &str,
    _req: &InstallStartRequest,
) -> Result<ResolvedSource, String> {
    Err("installing from a link is not available in the Android build".into())
}

/// Open a package on a saved server and read its header through ranges.
#[cfg(not(target_os = "android"))]
async fn open_remote_package(
    remote_path: &str,
) -> Result<
    (
        Arc<RemotePkg>,
        u64,
        ps5upload_pkg::ReaderMetadata,
        String,
        String,
    ),
    String,
> {
    let r = crate::remote::pool::global().map_err(|e| e.to_string())?;
    let source = crate::remote::range::RemoteRangeSource::open(
        Arc::clone(&r.pool),
        Arc::clone(&r.store),
        remote_path,
        crate::remote::pool::Backoff::standard(),
    )
    .await
    .map_err(|e| format!("could not open the package on the server: {e}"))?;
    let total_size = source.total_size();
    // Server host only; the path can name a user's folders.
    crate::log_info!(
        "remote install: host={} bytes={}",
        source.host(),
        total_size
    );
    let remote = Arc::new(RemotePkg::Remote(source));
    let probe = Arc::clone(&remote);
    let (head, fingerprint) = tokio::task::spawn_blocking(move || {
        let read_at = |offset: u64, len: u64| -> Option<Vec<u8>> {
            if len == 0 {
                return Some(Vec::new());
            }
            probe.read_range(offset, offset + len - 1).ok()
        };
        let head = ps5upload_pkg::metadata_from_reader(read_at).ok_or_else(|| {
            "that file does not look like a PS4/PS5 package (no readable PKG header)".to_string()
        })?;
        let fingerprint = ps5upload_pkg::package_fingerprint_from_reader(total_size, &read_at)
            .unwrap_or_default();
        Ok::<_, String>((head, fingerprint))
    })
    .await
    .map_err(|e| format!("remote package probe task panicked/cancelled: {e}"))??;
    let name = remote_path
        .rsplit('/')
        .next()
        .unwrap_or(remote_path)
        .to_string();
    Ok((remote, total_size, head, fingerprint, name))
}

/// Resolve an install that streams from a package on a saved server.
#[cfg(not(target_os = "android"))]
async fn resolve_remote_fs_source(
    remote_path: &str,
    req: &InstallStartRequest,
) -> Result<ResolvedSource, String> {
    if req.split_root.is_some() || req.remote_url.is_some() {
        return Err("a server path cannot be combined with split_root or remote_url".into());
    }
    if req.local_ps5_path.as_deref().is_some_and(|p| !p.is_empty()) {
        return Err("a server path cannot be combined with local_ps5_path".into());
    }
    let (remote, total_size, head, fingerprint, name) = open_remote_package(remote_path).await?;
    let metadata = streamed_metadata(req, head, fingerprint, total_size, &name);
    Ok((vec![], vec![], total_size, metadata, Some(remote)))
}

#[cfg(target_os = "android")]
async fn resolve_remote_fs_source(
    _remote_path: &str,
    _req: &InstallStartRequest,
) -> Result<ResolvedSource, String> {
    Err("installing from a server is not available in the Android build".into())
}

// ─── /api/pkg/remote/probe ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RemoteProbeRequest {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct RemoteProbeResponse {
    pub total_size: u64,
    pub filename: String,
    pub content_id: String,
    pub title: String,
    pub title_id: String,
    pub category: String,
    pub app_ver: String,
    pub platform: String,
    pub package_type: String,
    pub fingerprint: String,
}

/// Identify a package behind an HTTP(S) link without downloading it.
///
/// Lets the UI show what the link actually is — and reject a share page or a
/// non-package file — before the user commits a multi-hour install. Reads only
/// a few byte ranges (see `resolve_remote_source`).
async fn remote_probe_handler(Json(req): Json<RemoteProbeRequest>) -> Response<Body> {
    let url = req.url.trim().to_string();
    if url.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "url is required");
    }
    if !valid_dpi_install_source(&url) || url.starts_with('/') {
        return json_err(
            StatusCode::BAD_REQUEST,
            "url must be an http(s) package link with no fragment or control characters",
        );
    }
    let probe_req = InstallStartRequest {
        ps5_addr: String::new(),
        allow_destructive_reinstall: false,
        // A probe only reads a byte range to identify the package; it always
        // verifies certificates regardless of the install's own choice.
        insecure_tls: false,
        path: None,
        split_root: None,
        remote_url: Some(url.clone()),
        package_type_override: None,
        local_ps5_path: None,
        content_id: None,
        expected_size: None,
        package_fingerprint: None,
        delete_staging: false,
        serve_only: true,
    };
    match resolve_remote_source(&url, &probe_req).await {
        Ok((_, _, total_size, meta, _)) => {
            let package_type = meta
                .package_type
                .clone()
                .or_else(|| {
                    ps5upload_pkg::package_type_for_category_and_platform(
                        &meta.category,
                        &meta.platform,
                    )
                })
                .unwrap_or_default();
            json_ok(&RemoteProbeResponse {
                total_size,
                filename: meta
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string(),
                content_id: meta.content_id,
                title: meta.title,
                title_id: meta.title_id,
                category: meta.category,
                app_ver: meta.app_ver,
                platform: meta.platform,
                package_type,
                fingerprint: meta.fingerprint,
            })
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, &e),
    }
}

type ResolvedSource = (
    Vec<PathBuf>,
    Vec<u64>,
    u64,
    PkgMetadata,
    Option<Arc<RemotePkg>>,
);

async fn resolve_parts_and_meta(req: &InstallStartRequest) -> Result<ResolvedSource, String> {
    if let Some(url) = req.remote_url.as_deref().filter(|u| !u.is_empty()) {
        return resolve_remote_source(url, req).await;
    }
    if let Some(p) = req
        .path
        .as_deref()
        .filter(|p| crate::remote::path::is_remote(p))
    {
        return resolve_remote_fs_source(p, req).await;
    }
    // The two `parse_*` calls open and read .pkg / split-part headers (and stat
    // each split part) from disk. On a cold or network-hosted pkg that's
    // blocking I/O; run it OFF the async reactor so concurrent install-starts
    // across consoles can't park reactor worker threads. (Mirrors parse_handler.)
    if let Some(p) = &req.split_root {
        let p = p.clone();
        let m: SplitPkgMetadata =
            tokio::task::spawn_blocking(move || parse_split_pkg(std::path::Path::new(&p)))
                .await
                .map_err(|e| format!("split pkg parse task panicked/cancelled: {e}"))?
                .map_err(|e| format!("{e}"))?;
        Ok((m.parts, m.part_sizes, m.total_size, m.head, None))
    } else if let Some(p) = &req.path {
        let p = p.clone();
        let meta = tokio::task::spawn_blocking(move || parse_pkg(std::path::Path::new(&p)))
            .await
            .map_err(|e| format!("pkg parse task panicked/cancelled: {e}"))?
            .map_err(|e| format!("{e}"))?;
        let size = meta.size;
        Ok((vec![meta.path.clone()], vec![size], size, meta, None))
    } else if req
        .local_ps5_path
        .as_deref()
        .map(|p| !p.is_empty())
        .unwrap_or(false)
    {
        // Staged-pkg install: the .pkg is already on the PS5's disk and
        // there's no PC-side file to parse. Build minimal metadata from the
        // caller-provided fields (the client parsed the header at upload
        // time). The install URL is just the raw PS5 path and the payload
        // re-parses the content id from the staged pkg itself, so empty
        // values here are fine. parts/size are unused for a local install
        // (nothing is HTTP-served). This is what lets the Install Package
        // page route a staged pkg through the main payload's
        // InstallByPackage (which installs launchable content) instead of
        // the metadata-only DPI daemon.
        let lp = req.local_ps5_path.clone().unwrap_or_default();
        let meta = PkgMetadata {
            path: PathBuf::from(&lp),
            size: 0,
            kind: PkgKind::CntContainer,
            authenticity: ps5upload_pkg::PkgAuthenticity::Unknown,
            content_id: req.content_id.clone().unwrap_or_default(),
            title: String::new(),
            title_id: String::new(),
            category: String::new(),
            app_ver: String::new(),
            fingerprint: String::new(),
            package_type: req.package_type_override.clone(),
            platform: ps5upload_pkg::derive_platform(
                ps5upload_pkg::PKG_MAGIC,
                req.content_id.as_deref().unwrap_or(""),
                "",
            ),
            icon_png_base64: None,
            warnings: vec![],
        };
        Ok((vec![PathBuf::from(lp)], vec![0], 0, meta, None))
    } else {
        Err("either `path`, `split_root`, or `local_ps5_path` is required".into())
    }
}

/// Per-response byte cap. A LAN client (or a misbehaving BGFT) that
/// requests `bytes=0-{total-1}` on a 50 GB pkg would otherwise force the
/// engine to allocate ~50 GB and OOM. Sony's real BGFT fetches in
/// MB-sized chunks, so this cap is only ever hit by abuse cases. When a
/// requested range exceeds the cap, we trim `end` to `start + CAP - 1`
/// and return that prefix; HTTP Range semantics let the client follow
/// up with a `bytes=(end+1)-...` request, which is what BGFT already
/// does for legitimate chunked fetches.
/// Fixed `Last-Modified` validator for every pkg-host response. Constant on
/// purpose: see the header comment in `serve_handler`.
const PKG_HOST_LAST_MODIFIED: &str = "Wed, 01 Jan 2025 00:00:00 GMT";

/// Decide whether a pkg-host response must be `206 Partial Content` (with a
/// `Content-Range`) rather than a bare `200 OK`. True when the client sent a
/// Range header, OR when the served body covers less than the whole file
/// (`end + 1 < total`) — the latter happens on a bare GET whose body the
/// 16 MiB cap trimmed. Without the second case a plain GET on a >16 MiB pkg
/// returned `200 OK` for a truncated body, so any non-Range consumer treated
/// an incomplete package as complete.
fn serve_is_partial(has_range: bool, end: u64, total: u64) -> bool {
    has_range || end + 1 < total
}

/// Map a Range request to (start, end) inclusive over the total size.
/// We support `bytes=N-M` and `bytes=N-` only — Sony BGFT only sends
/// those forms in practice. Out-of-bounds and inverted ranges are
/// rejected; over-large ranges are trimmed to the per-response cap so
/// a malicious large-range request can't OOM the engine.
fn parse_range_header(headers: &HeaderMap, total: u64) -> Result<(u64, u64), ()> {
    let h = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => {
            // No Range header — serve the whole file. The body is streamed in
            // bounded chunks, so size is not a memory concern.
            return Ok((0, total.saturating_sub(1)));
        }
    };
    let after = h.strip_prefix("bytes=").ok_or(())?;
    let (s, e) = after.split_once('-').ok_or(())?;
    // Support suffix-range `bytes=-N` (last N bytes) per RFC 9110 §14.1.2.
    // BGFT doesn't use this, but spec-compliant clients (curl) do.
    let (start, end) = if s.is_empty() {
        let suffix: u64 = e.parse().map_err(|_| ())?;
        let start = total.saturating_sub(suffix);
        (start, total.saturating_sub(1))
    } else {
        let start: u64 = s.parse().map_err(|_| ())?;
        let end: u64 = if e.is_empty() {
            total.saturating_sub(1)
        } else {
            e.parse().map_err(|_| ())?
        };
        (start, end)
    };
    if start > end || start >= total {
        return Err(());
    }
    // RFC 9110 §14.1.2: a range that starts inside the file is satisfiable even
    // when its end runs past it — clamp rather than reject. Sony's installer asks
    // for whole 64 KiB blocks, so the last block of every package whose size is
    // not a multiple of 64 KiB arrives with `end >= total`; rejecting it with a
    // 416 aborted the install (0x80b22416) on packages Sony's own installer
    // accepts.
    let end = end.min(total - 1);
    // NOTE: ranges are NEVER truncated. We used to trim anything over 16 MiB
    // and let the client ask for the rest, which is legal HTTP but broke every
    // large install: Sony reads a package's trailing metadata in ONE request
    // whose size scales with the package, and it does not re-request the tail
    // it did not get. Measured in user bug reports — a 121 GB FF16 asked for a
    // single 249.6 MiB range and was refused with 0x80b211cd right after we
    // answered with 16 MiB; another report truncated 718 requests (largest
    // 39 MiB). Small packages never hit the cap, which is exactly why installs
    // "worked with small files and failed with big ones". The body is streamed
    // now, so a large range costs bounded memory rather than its full size.
    Ok((start, end))
}

/// What a `/pkg-host/{session}/{filename}` request resolves to.
enum ServeSource {
    /// The package itself (every non-`.crc` filename, as before).
    Package,
    /// `playgo-chunk.crc` stored inside the package's trailing ZIP.
    EmbeddedCrc { offset: u64, len: u64 },
    /// A `<name>.crc` file sitting next to the package on this host.
    SiblingCrc(Arc<Vec<u8>>),
}

impl ServeSource {
    fn len(&self, package_size: u64) -> u64 {
        match self {
            ServeSource::Package => package_size,
            ServeSource::EmbeddedCrc { len, .. } => *len,
            ServeSource::SiblingCrc(bytes) => bytes.len() as u64,
        }
    }
}

/// Largest sibling `.crc` we read into memory. A 200 GB package needs 12 MiB.
const SIBLING_CRC_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Resolve a `.crc` request: the member inside the package wins (it is the one
/// the package was finalized with), then a sibling file, else `None` (404).
fn resolve_crc(session: &InstallSession, filename: &str) -> std::io::Result<Option<ServeSource>> {
    let located = crate::pkg_sidecar::locate_playgo_crc(session.total_size, |off, n| {
        // read_split_range is inclusive and cannot express an empty range.
        if n == 0 {
            return Ok(Vec::new());
        }
        read_split_range(session, off, off + n - 1)
    })?;
    if let Some((offset, len)) = located {
        return Ok(Some(ServeSource::EmbeddedCrc { offset, len }));
    }
    let plain_name = !filename.is_empty()
        && filename != ".crc"
        && !filename.contains(['/', '\\'])
        && !filename.contains("..");
    if !plain_name {
        return Ok(None);
    }
    let Some(dir) = session.parts.first().and_then(|p| p.parent()) else {
        return Ok(None);
    };
    let candidate = dir.join(filename);
    match std::fs::metadata(&candidate) {
        Ok(m) if m.is_file() && m.len() <= SIBLING_CRC_MAX_BYTES => Ok(Some(
            ServeSource::SiblingCrc(Arc::new(std::fs::read(&candidate)?)),
        )),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read a byte range `[start, end]` (inclusive) from the split-pkg
/// part list, crossing part boundaries as needed.
fn read_split_range(s: &InstallSession, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    // Install-from-a-link: there is no local file, so the range is satisfied
    // from the origin (many connections at once, short in-memory window
    // cache). Everything downstream — 206/Content-Range, the transfer
    // coverage map, cancel — is identical to a local stream install.
    if let Some(remote) = &s.remote {
        let bytes = remote.read_range(start, end)?;
        // Overlap the NEXT window's origin fetch with serving this one. Without
        // it the proxy only ever pulls as fast as the console consumes, because
        // every window is faulted in on demand and the console blocks while it
        // is fetched. See RemoteSource::prefetch_after.
        //
        // Deliberately only here, not on the header-probe read_range above: that
        // one reads a small fixed range once to identify the package, and a
        // readahead after it would pull a whole window the install may never ask
        // for.
        RemotePkg::prefetch_after(remote, end);
        return Ok(bytes);
    }
    let want_len = usize::try_from(end - start + 1).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "range too large for usize",
        )
    })?;
    let mut out = Vec::with_capacity(want_len);
    let mut cursor = start;

    // Find the part containing `cursor` and stream until we've covered
    // the requested range. Read in capped chunks to avoid huge buffers
    // on a single call.
    let mut prefix = 0u64;
    for (i, part_size) in s.part_sizes.iter().enumerate() {
        let part_end = prefix.checked_add(*part_size).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "part size overflow")
        })?;
        if cursor < part_end {
            let local_start = cursor - prefix;
            let want_end_global = end.min(part_end - 1);
            let local_end = want_end_global - prefix;
            let take = local_end - local_start + 1;
            let take_usize = usize::try_from(take).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "take too large for usize")
            })?;

            let mut f = std::fs::File::open(&s.parts[i])?;
            f.seek(SeekFrom::Start(local_start))?;
            // Read directly into `out`'s tail to avoid a double allocation
            // (previously allocated `chunk` + `out.extend_from_slice`, using
            // twice the chunk size per read).
            let old_len = out.len();
            out.resize(old_len + take_usize, 0);
            f.read_exact(&mut out[old_len..])?;

            cursor = want_end_global + 1;
            if cursor > end {
                break;
            }
        }
        prefix = part_end;
    }
    Ok(out)
}

/// Pick the LAN IP this host presents to the given PS5 host. Works
/// across multi-NIC machines by asking the OS what local IP it would
/// use to send a packet to the PS5 — that's the right one to give to
/// BGFT in the install URL.
/// The PS5UPLOAD_ALLOW_IP peers, parsed once. The pkg-host source gate is hit
/// by hundreds of Range requests per install, so cache rather than re-parse the
/// env on each. The value is fixed at process start (set by the container/host
/// launcher), so a OnceLock snapshot is correct.
fn parse_allow_ips_env() -> &'static [IpAddr] {
    static ALLOW: std::sync::OnceLock<Vec<IpAddr>> = std::sync::OnceLock::new();
    ALLOW.get_or_init(|| {
        crate::parse_allow_ips(&std::env::var("PS5UPLOAD_ALLOW_IP").unwrap_or_default())
    })
}

pub fn lan_ip_for_ps5(ps5_host: &str) -> std::io::Result<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    // UDP "connect" doesn't actually send anything — it just sets the
    // peer for routing-table lookup, so local_addr() returns the IP
    // the OS would use. Port number is arbitrary.
    sock.connect(format!("{ps5_host}:1"))?;
    Ok(sock.local_addr()?.ip())
}

/// Build the engine's `/pkg-host/{session}/{filename}` URL as the PS5
/// will fetch it. Picks the LAN IP this host presents to the PS5 (multi-
/// NIC safe), stamps the engine port, and canonicalises the filename
/// from the pkg's content_id so Sony's installer header cross-check
/// passes. Returns an Err with a human-readable cause when the LAN IP
/// can't be determined (e.g. PS5 host unresolvable).
///
/// Shared by the regular install-start flow (which embeds the URL in
/// the BGFT register request) and the direct/streaming install flow
/// (which hands the URL to the DPI daemon instead of a local path).
pub(crate) fn pkg_host_url_for(
    ps5_addr: &str,
    session_id: &str,
    content_id: &str,
) -> std::io::Result<String> {
    let origin = engine_origin_for_ps5(ps5_addr)?;
    let url_filename = pkg_url_filename(content_id);
    Ok(format!("{origin}/pkg-host/{session_id}/{url_filename}"))
}

/// `http://<ip>:<port>` of this engine as the PS5 reaches it. See
/// `pkg_host_url_for` for how the IP is chosen and when it must be pinned.
fn engine_origin_for_ps5(ps5_addr: &str) -> std::io::Result<String> {
    let ps5_host_only = strip_host_port(ps5_addr);
    // PS5UPLOAD_PKG_HOST_IP lets a deployment pin the IP the console fetches
    // from, overriding the routing-table guess. Required whenever the engine
    // can't see a console-reachable source IP for itself — most importantly a
    // container on Docker Desktop (macOS/Windows), where the daemon runs in a
    // VM and `lan_ip_for_ps5` returns the container's NAT address that the PS5
    // can't route back to. Set it to the HOST's LAN IP (the one the PS5 reaches
    // the published port on). Ignored when empty/unset.
    let local_ip = match std::env::var("PS5UPLOAD_PKG_HOST_IP") {
        Ok(v) if !v.trim().is_empty() => v.trim().parse::<IpAddr>().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("PS5UPLOAD_PKG_HOST_IP='{v}' is not a valid IP: {e}"),
            )
        })?,
        _ => lan_ip_for_ps5(&ps5_host_only)?,
    };
    let host_port = std::env::var("PS5UPLOAD_ENGINE_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(19113);
    Ok(format!("http://{local_ip}:{host_port}"))
}

// ─── Short aliases for over-long install links ───────────────────────

/// Longest install source the PS5's installer accepts, in bytes.
///
/// Hardware-measured on FW 5.10 by binary search: 127 accepted, 128 refused
/// with 0x80A30003 (SCE_APP_INSTALLER_ERROR_PARAM) — a 128-byte buffer. An
/// ordinary library link is longer than that (a 139-character one was
/// refused), which is why "let the PS5 download it" failed for them.
pub const MAX_INSTALL_SOURCE_LEN: usize = 127;

/// How long an alias lives after its last use. An install re-resolves the
/// alias on every range request, so an active one never expires.
const LINK_ALIAS_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

struct LinkAlias {
    url: String,
    last_used: std::time::Instant,
}

fn link_aliases() -> &'static Mutex<HashMap<String, LinkAlias>> {
    static ALIASES: std::sync::OnceLock<Mutex<HashMap<String, LinkAlias>>> =
        std::sync::OnceLock::new();
    ALIASES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The alias id for `url`, reusing an existing one for the same link.
fn link_alias_id_for(url: &str) -> String {
    let mut map = link_aliases().lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    map.retain(|_, a| now.duration_since(a.last_used) < LINK_ALIAS_TTL);
    if let Some((id, a)) = map.iter_mut().find(|(_, a)| a.url == url) {
        a.last_used = now;
        return id.clone();
    }
    // 96 random bits: an alias is reachable off-loopback, so it must not be
    // guessable — though even a guessed one only redirects to a link this
    // user registered through the loopback-only API.
    let id = uuid::Uuid::new_v4().simple().to_string()[..24].to_string();
    map.insert(
        id.clone(),
        LinkAlias {
            url: url.to_string(),
            last_used: now,
        },
    );
    id
}

fn link_alias_target(id: &str) -> Option<String> {
    let mut map = link_aliases().lock().unwrap_or_else(|e| e.into_inner());
    let a = map.get_mut(id)?;
    a.last_used = std::time::Instant::now();
    Some(a.url.clone())
}

/// A short URL the PS5 can be given in place of `url`, or `None` when `url`
/// already fits.
///
/// The console's installer follows HTTP redirects — hardware-verified: handed
/// a 33-character URL that redirected to a 182-character one, it installed a
/// package in about 3 s. So the alias keeps "let the PS5 download it" direct:
/// the package bytes still flow from the link to the console. The cost is
/// that the console re-resolves the alias on every range request (measured:
/// one redirect per request), so this computer has to stay reachable until
/// the install finishes, answering tiny redirects.
pub(crate) fn shorten_for_installer(ps5_addr: &str, url: &str) -> std::io::Result<Option<String>> {
    if url.starts_with('/') || url.len() <= MAX_INSTALL_SOURCE_LEN {
        return Ok(None);
    }
    let origin = engine_origin_for_ps5(ps5_addr)?;
    let id = link_alias_id_for(url);
    let short = format!("{origin}/pkg-host/link/{id}.pkg");
    if short.len() > MAX_INSTALL_SOURCE_LEN {
        return Err(std::io::Error::other(format!(
            "even the shortened link is {} characters (limit {MAX_INSTALL_SOURCE_LEN})",
            short.len()
        )));
    }
    Ok(Some(short))
}

/// GET/HEAD /pkg-host/link/{id}.pkg — redirect to the link the alias stands for.
async fn link_redirect_handler(AxumPath(file): AxumPath<String>) -> Response<Body> {
    let id = file.strip_suffix(".pkg").unwrap_or(&file);
    match link_alias_target(id) {
        Some(url) => Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, url)
            .header(header::CONTENT_LENGTH, "0")
            .body(Body::empty())
            .unwrap_or_else(builder_failed_response),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap_or_else(builder_failed_response),
    }
}

/// Last-ditch fallback when a `Response::builder()` chain fails. The
/// builders in this file only set statically valid headers, so this is
/// unreachable in practice — but one engine process serves every
/// console, and a panic in a response builder would kill all of their
/// transfers, so fail soft with a bare 500 instead of unwrapping.
fn builder_failed_response(e: axum::http::Error) -> Response<Body> {
    let mut resp = Response::new(Body::from(format!("response build failed: {e}")));
    *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    resp
}

fn json_ok<T: Serialize>(v: &T) -> Response<Body> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(builder_failed_response)
}

fn json_err(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": msg }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(builder_failed_response)
}

fn plain_response(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(msg.to_string()))
        .unwrap_or_else(builder_failed_response)
}

/// Strip port + IPv6 brackets from a `host:port` (or `[ipv6]:port`)
/// string, returning just the bare host or IP. Centralised so the URL
/// builder (where we feed the IP to `lan_ip_for_ps5`) and the source-IP
/// gate (where we compare against `peer.ip().to_string()`) stay in
/// lock-step — the v2.16.1 Round 1 audit caught one site and Round 2
/// caught the other, with the same root cause: `split(':').next()`
/// truncates IPv6 to `[` because IPv6 addresses contain colons.
///
/// rsplit_once on the LAST `:` correctly cuts off the port for both
/// `1.2.3.4:9114` → `1.2.3.4` and `[2001:db8::1]:9114` → `[2001:db8::1]`.
/// We then strip surrounding brackets to normalise to the bare form
/// `peer.ip().to_string()` emits.
///
/// Edge cases:
///   - input with no port (`1.2.3.4`, `::1`) → returned as-is (without
///     brackets if present)
///   - empty input → empty string (caller is expected to handle)
pub(crate) fn strip_host_port(host_port: &str) -> String {
    // Bracketed IPv6 with port: `[2001:db8::1]:9114` →
    // rsplit_once on `]:` gives `[2001:db8::1` (with leading bracket).
    if let Some((host, port)) = host_port.rsplit_once("]:") {
        // host has leading `[` from the original; port is just digits.
        if port.parse::<u16>().is_ok() {
            return host.trim_start_matches('[').to_string();
        }
        // Empty port like `[::1]:` or non-numeric like `[::1]:foo` —
        // strip the host's leading `[` plus the trailing `]:port`
        // fragment, leaving the bare IPv6. Without this branch the
        // string falls through to the multi-colon catch-all and
        // returns unchanged, breaking the source-IP gate.
        return host.trim_start_matches('[').to_string();
    }
    // Bracketed IPv6 without port: `[2001:db8::1]` → strip brackets.
    if host_port.starts_with('[') && host_port.ends_with(']') {
        return host_port
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
    }
    // Bare IPv6 (no brackets, no port): contains multiple colons —
    // can't reliably distinguish host from port. Assume no port and
    // return the whole string. Callers feeding bare IPv6 with a
    // port suffix MUST bracket the address — that's the only
    // unambiguous form.
    if host_port.matches(':').count() > 1 {
        return host_port.to_string();
    }
    // Single colon: IPv4-with-port or hostname-with-port — split.
    match host_port.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host.to_string(),
        _ => host_port.to_string(),
    }
}

/// Derive the URL filename component for a hosted pkg, preferring the
/// pkg's canonical content_id (Sony cross-checks the URL filename
/// against the pkg header on register). Falls back to "file.pkg" when
/// the header didn't carry a content_id (corrupt pkg, parse failure,
/// or a non-Sony header). Sanitises against URL-meta chars even on
/// the happy path — content_id is supposed to be `[A-Z0-9-_]+` per
/// Sony's spec, but a malformed pkg could carry slashes / spaces that
/// would re-route the request or break axum's path matcher.
fn pkg_url_filename(content_id: &str) -> String {
    let trimmed = content_id.trim();
    if trimmed.is_empty() {
        return "file.pkg".to_string();
    }
    // Keep only ASCII printable, no path/URL-meta. Anything else is
    // replaced with '_' so a hostile or corrupt pkg can't escape the
    // URL pattern. Length-cap at 64 chars (content_id spec is 36;
    // 64 is a generous safety ceiling).
    let safe: String = trimmed
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        return "file.pkg".to_string();
    }
    format!("{safe}.pkg")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod loader_route_tests {
    use ps5upload_core::payload_lifecycle::dpi_send_failure_reason;

    /// The failure the field exists for. A console whose ELF loader has
    /// stopped answering on :9021 cannot be handed the DPI daemon at all —
    /// which is a problem with the console, not with the engine build. The
    /// client picks its guidance from this code, so the classifier has to
    /// separate "couldn't connect" from every later failure.
    #[test]
    fn a_refused_loader_port_is_reported_as_loader_unreachable() {
        use ps5upload_core::payload_lifecycle::{
            DPI_REASON_LOADER_SEND_FAILED, DPI_REASON_LOADER_UNREACHABLE,
        };
        assert_eq!(
            dpi_send_failure_reason("connect 10.0.0.5:9021: Connection refused (os error 111)"),
            DPI_REASON_LOADER_UNREACHABLE
        );
        assert_eq!(
            dpi_send_failure_reason("connect 10.0.0.5:9021: Operation timed out (os error 60)"),
            DPI_REASON_LOADER_UNREACHABLE
        );
        // Everything past the connect is a different problem with different
        // advice, so it must NOT claim the loader was unreachable.
        for after_connect in [
            "write 10.0.0.5:9021: Broken pipe (os error 32)",
            "half-close 10.0.0.5:9021: Socket is not connected (os error 57)",
            "resolve ps5.local:9021: failed to lookup address information",
            "not an ELF image (first bytes [00, 00, 00, 00])",
        ] {
            assert_eq!(
                dpi_send_failure_reason(after_connect),
                DPI_REASON_LOADER_SEND_FAILED,
                "{after_connect}"
            );
        }
    }

    /// The prefix `dpi_send_failure_reason` keys off is produced by
    /// `send_elf_to_loader`, in another crate. Pin the contract here so a
    /// reworded error there fails this test instead of silently sending
    /// every user the wrong advice.
    #[test]
    fn loader_send_reports_an_unreachable_port_with_the_connect_prefix() {
        use ps5upload_core::payload_lifecycle as pl;
        // RFC 5737 TEST-NET-2: nothing answers, so this is a connect failure.
        let err = pl::send_elf_to_loader(
            "198.51.100.1",
            pl::PS5_LOADER_PORT,
            b"\x7FELF-not-a-real-image",
            pl::LoaderImage::Companion,
        )
        .expect_err("nothing is listening on TEST-NET-2");
        assert!(err.starts_with("connect "), "{err}");
        assert_eq!(
            dpi_send_failure_reason(&err),
            pl::DPI_REASON_LOADER_UNREACHABLE
        );
    }
}

#[cfg(test)]
mod persist_tests {
    use super::*;

    fn session(id: &str, part: PathBuf, size: u64) -> InstallSession {
        let mut sessions = persist::load_from(std::path::Path::new("/nonexistent"));
        assert!(sessions.is_empty());
        let saved = serde_json::json!([{
            "id": id, "parts": [part], "part_sizes": [size], "total_size": size,
            "content_id": "UP0000-TEST00000_00-0000000000000000", "title": "t",
            "package_type": "app", "package_fingerprint": "f", "ps5_mgmt_addr": "1.2.3.4:9114",
            "serve_only": true, "staging_path": null,
            "created_at_unix": now_unix(), "last_activity_unix": now_unix()
        }]);
        let dir = std::env::temp_dir().join(format!("ps5u-persist-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s.json");
        std::fs::write(&file, saved.to_string()).unwrap();
        sessions = persist::load_from(&file);
        sessions.remove(id).expect("restored")
    }

    /// A restarted engine serves the sessions it was serving, and only while their files are
    /// unchanged.
    #[test]
    fn sessions_survive_a_restart_while_their_files_do() {
        let dir = std::env::temp_dir().join(format!("ps5u-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pkg = dir.join("game.pkg");
        std::fs::write(&pkg, vec![7u8; 4096]).unwrap();
        let file = dir.join("sessions.json");

        let s = session("a", pkg.clone(), 4096);
        let mut map = HashMap::new();
        map.insert(s.id.clone(), s);
        persist::save_to(&file, &map);
        let back = persist::load_from(&file);
        let got = back.get("a").expect("the session comes back");
        assert_eq!(got.parts, vec![pkg.clone()]);
        assert_eq!(got.total_size, 4096);
        assert!(got.serve_only);

        // A finished or cancelled session is not kept.
        map.get_mut("a").unwrap().cancelled = true;
        persist::save_to(&file, &map);
        assert!(persist::load_from(&file).is_empty());

        // A package that changed size is not served in the old one's place.
        map.get_mut("a").unwrap().cancelled = false;
        persist::save_to(&file, &map);
        std::fs::write(&pkg, vec![7u8; 100]).unwrap();
        assert!(persist::load_from(&file).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The unified orchestrator releases a stream session once the console has
    /// pulled the package. Before this, a finished session stayed "live" (and
    /// survived restarts), so re-installing the same package was refused as
    /// "already running" — measured on the Phat, 0 of 820 MB "in flight".
    #[test]
    fn a_released_session_stops_blocking_and_is_not_restored() {
        let dir = std::env::temp_dir().join(format!("ps5u-release-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pkg = dir.join("game.pkg");
        std::fs::write(&pkg, vec![7u8; 4096]).unwrap();
        // Distinct id: `session()` stages under ps5u-persist-{id}, shared with
        // the other persistence test, so reusing "a" races on one file.
        let s = session("released", pkg, 4096);
        let mut map = HashMap::from([(s.id.clone(), s)]);

        // The pure half (release_serve_session adds persist::save, which would
        // write the real data dir, so the test drives save_to itself).
        assert!(super::mark_released(&mut map, "released"));
        assert!(map["released"].cancelled);
        // Releasing an unknown session is a harmless no-op.
        assert!(!super::mark_released(&mut map, "missing"));

        let file = dir.join("sessions.json");
        persist::save_to(&file, &map);
        assert!(persist::load_from(&file).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    // ── payload-restore (web UI helper redeploy) ──

    /// The browser build's `restoreMainPayload` posts here. An empty address
    /// must be refused before anything is sent to a loader.
    #[tokio::test]
    async fn payload_restore_rejects_an_empty_address() {
        let resp = super::payload_restore_handler(axum::Json(super::PayloadRestoreRequest {
            ps5_addr: "  ".into(),
        }))
        .await;
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    /// The route is wired: the web UI's `payload_restore` command 404'd after
    /// the unified-install cleanup removed it.
    #[test]
    fn payload_restore_route_is_registered() {
        let src = include_str!("pkg_install.rs");
        assert!(
            src.contains(r#".route("/api/pkg/payload-restore", post(payload_restore_handler))"#)
        );
    }

    // ── short aliases for over-long install links ──

    const LONG: &str = "http://192.168.86.199:20080/3A5CA02AFD084A3B8445AC51D3EAE212/\
Marvel's%20Spider-Man%202%20-%20PPSA03016%20-%20v1.4.3%20-%20US%20-%20BASE.pkg";

    /// The link from the field that "let the PS5 download it" could not take.
    #[test]
    fn the_reported_link_is_over_the_installer_limit() {
        assert_eq!(LONG.len(), 139);
        assert!(LONG.len() > super::MAX_INSTALL_SOURCE_LEN);
    }

    /// A link that already fits is handed over untouched: no alias, no
    /// dependency on this computer staying up.
    #[test]
    fn a_link_that_fits_is_not_shortened() {
        let short = "http://192.168.86.199:20081/UP9000-PPSA03016_00-MARVELSPIDERMAN2.pkg";
        assert_eq!(
            super::shorten_for_installer("127.0.0.1:9114", short).unwrap(),
            None
        );
        assert_eq!(
            super::shorten_for_installer("127.0.0.1:9114", "/data/pkg/a.pkg").unwrap(),
            None
        );
    }

    /// An over-long link becomes an alias the installer accepts, and the
    /// alias leads back to the exact link.
    #[test]
    fn an_over_long_link_becomes_an_alias_that_fits() {
        let short = super::shorten_for_installer("127.0.0.1:9114", LONG)
            .unwrap()
            .expect("shortened");
        assert!(
            short.len() <= super::MAX_INSTALL_SOURCE_LEN,
            "{} chars",
            short.len()
        );
        assert!(short.contains("/pkg-host/link/"), "{short}");
        let id = short
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".pkg")
            .to_string();
        assert_eq!(super::link_alias_target(&id).as_deref(), Some(LONG));
    }

    /// Installing the same link twice reuses its alias rather than growing
    /// the table without bound.
    #[test]
    fn the_same_link_reuses_its_alias() {
        let a = super::link_alias_id_for(LONG);
        let b = super::link_alias_id_for(LONG);
        assert_eq!(a, b);
        assert_ne!(a, super::link_alias_id_for(&format!("{LONG}?other=1")));
    }

    /// Through the real router: the alias answers 302 to the link, and an
    /// unknown id answers 404. This also proves the alias route does not
    /// collide with /pkg-host/{session}/{filename}.
    #[tokio::test]
    async fn the_alias_route_redirects_and_unknown_ids_404() {
        use std::io::{Read, Write};
        let id = super::link_alias_id_for(LONG);
        let app = super::router(std::sync::Arc::new(super::PkgInstallState::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let fetch = move |path: String| {
            std::thread::spawn(move || {
                let mut c = std::net::TcpStream::connect(addr).unwrap();
                write!(
                    c,
                    "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                let mut out = String::new();
                c.read_to_string(&mut out).unwrap();
                out
            })
            .join()
            .unwrap()
        };

        let hit = tokio::task::spawn_blocking(move || {
            (
                fetch(format!("/pkg-host/link/{id}.pkg")),
                fetch("/pkg-host/link/nope.pkg".into()),
            )
        })
        .await
        .unwrap();
        assert!(hit.0.starts_with("HTTP/1.1 302"), "{}", hit.0);
        assert!(
            hit.0
                .to_ascii_lowercase()
                .contains(&format!("location: {}", LONG.to_ascii_lowercase())),
            "{}",
            hit.0
        );
        assert!(hit.1.starts_with("HTTP/1.1 404"), "{}", hit.1);
    }

    #[test]
    fn dpi_install_source_accepts_http_links_but_not_other_schemes_or_lines() {
        assert!(super::valid_dpi_install_source("/user/data/game.pkg"));
        assert!(super::valid_dpi_install_source(
            "https://example.org/game.pkg?token=abc"
        ));
        assert!(super::valid_dpi_install_source(
            "http://192.168.1.10/game.pkg"
        ));
        assert!(!super::valid_dpi_install_source("file:///etc/passwd"));
        assert!(!super::valid_dpi_install_source(
            "https://example.org/a.pkg\n/evil"
        ));
        assert!(!super::valid_dpi_install_source(
            "https://example.org/a.pkg#fragment"
        ));
        assert!(!super::valid_dpi_install_source("relative.pkg"));
    }
    use super::*;

    // pkg_host_url_for reads process-global env vars (PS5UPLOAD_ENGINE_PORT,
    // PS5UPLOAD_PKG_HOST_IP). Tests that set them must not run concurrently or
    // one leaks into another; serialize them on this lock.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A session's address is used for EVERY observation it will ever make
    /// (artifact check, free-space, title-dir, Sony log), and every one of
    /// those failures degrades quietly to "absent" rather than to an error.
    /// So a bare IP must be normalized at creation, not left to each caller:
    /// measured on 2026-09-14, a portless addr produced 0 ms status polls,
    /// `installed_bytes: 0`, an `Absent` artifact check and a phase stuck on
    /// `install` for the full 600 s stall — while the exact package was
    /// already installed on the console.
    #[test]
    fn a_session_address_is_always_normalized_to_the_mgmt_port() {
        assert_eq!(normalize_mgmt_addr("192.168.86.100"), "192.168.86.100:9114");
        assert_eq!(
            normalize_mgmt_addr("192.168.86.100:9114"),
            "192.168.86.100:9114"
        );
        // A caller may hand us the transfer port; the mgmt port is what every
        // frame this session sends must target.
        assert_eq!(
            normalize_mgmt_addr("192.168.86.100:9113"),
            "192.168.86.100:9114"
        );
        // Hostnames and IPv6 literals follow the same rule.
        assert_eq!(normalize_mgmt_addr("ps5.lan"), "ps5.lan:9114");
        assert_eq!(normalize_mgmt_addr("[::1]"), "[::1]:9114");
        assert_eq!(normalize_mgmt_addr("[::1]:9113"), "[::1]:9114");
    }

    #[test]
    fn browser_upload_filename_is_a_safe_basename() {
        assert_eq!(sanitize_pkg_filename("../../Game.pkg"), "Game.pkg");
        assert_eq!(
            sanitize_pkg_filename(r"C:\\Downloads\\Game.pkg"),
            "Game.pkg"
        );
        assert_eq!(sanitize_pkg_filename("..."), "package.pkg");
        assert_eq!(sanitize_pkg_filename("bad\0name.pkg"), "badname.pkg");
    }

    #[test]
    fn browser_upload_accepts_pkg_and_fpkg_extensions() {
        assert!(is_install_package_filename("Game.pkg"));
        assert!(is_install_package_filename("Game.FPKG"));
        assert!(!is_install_package_filename("Game.ffpkg"));
        assert!(!is_install_package_filename("Game.ffpfs"));
    }

    #[test]
    fn browser_upload_cleanup_accepts_only_uuid_directories() {
        let id = "f983a63c-e6f7-489c-b2d7-14d994eff321";
        assert_eq!(pkg_upload_path(id), Some(pkg_upload_dir().join(id)));
        assert_eq!(pkg_upload_path("../not-an-upload"), None);
        assert_eq!(pkg_upload_path(""), None);
    }

    // ── progress-driven install tracker (the large-pkg data-loss fix) ──
    //
    // These pin the brain of the tracker — the pure `install_verdict`. The
    // failure they guard against: a 25 GB / 200 GB install reported "done"
    // after a fixed timer, deleting the staged pkg WHILE Sony was still
    // installing from it. The cure is "Complete only on observed completion".

    // ── delete_staging / staging cleanup (the Auto-Delete data-loss fix) ──

    #[test]
    fn staging_path_kept_when_delete_disabled() {
        // Auto Delete OFF → no staging_path → the uploaded pkg is KEPT, even
        // though a real local path was supplied. This is the core guarantee.
        let path = Some("/user/data/ps5upload/pkg_library/game.pkg".to_string());
        assert_eq!(staging_path_for(&path, false), None);
    }

    #[test]
    fn staging_path_cleaned_when_delete_enabled() {
        let path = Some("/user/data/ps5upload/pkg_library/game.pkg".to_string());
        assert_eq!(
            staging_path_for(&path, true),
            Some("/user/data/ps5upload/pkg_library/game.pkg".to_string())
        );
    }

    #[test]
    fn staging_path_none_for_empty_or_missing_path() {
        // Empty string and None both yield None regardless of the flag (there's
        // nothing to clean — e.g. the http-host flow with no local pkg).
        assert_eq!(staging_path_for(&Some(String::new()), true), None);
        assert_eq!(staging_path_for(&None, true), None);
        assert_eq!(staging_path_for(&None, false), None);
    }

    // ── is_retryable_delete_error (the fs_delete_failed retry decision) ──

    #[test]
    fn retryable_delete_error_on_fs_delete_failed_token() {
        // The bare token the payload sends when rm_rf returns non-zero
        // (Sony's installer still holding the staged pkg open). This is
        // the ONLY case we retry — it resolves on its own in ~1-2s.
        assert!(is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_failed"
        ));
    }

    #[test]
    fn retryable_delete_error_not_on_path_not_allowed() {
        // A genuine allowlist rejection — won't resolve on retry.
        assert!(!is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_path_not_allowed"
        ));
    }

    #[test]
    fn retryable_delete_error_not_on_too_many_inflight() {
        // All MAX_FS_OPS slots busy — retrying immediately won't help.
        assert!(!is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_too_many_inflight"
        ));
    }

    #[test]
    fn retryable_delete_error_not_on_socket_timeout() {
        // A wedged console / network error — surfacing immediately is
        // more useful than silently retrying for 6s.
        assert!(!is_retryable_delete_error(
            "read frame header: Resource temporarily unavailable (os error 11)"
        ));
        assert!(!is_retryable_delete_error("connection reset by peer"));
    }

    #[test]
    fn retryable_delete_error_not_on_cancellation() {
        // User hit Stop — cancellation is intentional, not retryable.
        assert!(!is_retryable_delete_error("cancelled"));
        assert!(!is_retryable_delete_error(
            "payload rejected FS_DELETE: fs_delete_cancelled"
        ));
    }

    #[test]
    fn install_start_request_delete_staging_defaults_true() {
        // Back-compat: an older client that omits delete_staging must keep the
        // historical always-clean behaviour (true), not silently flip to keep.
        let json = r#"{"ps5_addr":"1.2.3.4:9114","local_ps5_path":"/x.pkg"}"#;
        let req: InstallStartRequest = serde_json::from_str(json).unwrap();
        assert!(
            req.delete_staging,
            "omitted delete_staging must default true"
        );
    }

    #[test]
    fn install_start_request_delete_staging_false_round_trips() {
        // The current client sends the real preference; false must be honoured.
        let json =
            r#"{"ps5_addr":"1.2.3.4:9114","local_ps5_path":"/x.pkg","delete_staging":false}"#;
        let req: InstallStartRequest = serde_json::from_str(json).unwrap();
        assert!(!req.delete_staging);
        // And it must flow through to a kept pkg.
        assert_eq!(
            staging_path_for(&req.local_ps5_path, req.delete_staging),
            None
        );
    }

    #[test]
    fn strip_host_port_handles_ipv4_and_ipv6() {
        // IPv4 with port — the common case.
        assert_eq!(strip_host_port("192.168.1.42:9114"), "192.168.1.42");
        // IPv6 bracketed with port — the SocketAddr-emitted form.
        assert_eq!(strip_host_port("[2001:db8::1]:9114"), "2001:db8::1");
        assert_eq!(strip_host_port("[::1]:9114"), "::1");
        // No port — should pass through unchanged.
        assert_eq!(strip_host_port("192.168.1.42"), "192.168.1.42");
        // Bare bracketless IPv6 without port — the disambiguation
        // case. Naïve rsplit_once would split "::1" into "::" / "1"
        // (wrong); the port-must-parse-as-u16 check rejects that.
        assert_eq!(strip_host_port("::1"), "::1");
        assert_eq!(strip_host_port("2001:db8::1"), "2001:db8::1");
        // Hostname with port.
        assert_eq!(strip_host_port("my-ps5.local:9114"), "my-ps5.local");
        // Empty input.
        assert_eq!(strip_host_port(""), "");
        // Edge: bracketed IPv6 with empty/invalid port — Round 4 found
        // these fell through unhandled. Should still strip the host.
        assert_eq!(strip_host_port("[::1]:"), "::1");
        assert_eq!(strip_host_port("[2001:db8::1]:foo"), "2001:db8::1");
    }

    #[test]
    fn pkg_url_filename_uses_content_id() {
        assert_eq!(
            pkg_url_filename("IV0002-NPXS39041_00-STOREUPD00000000"),
            "IV0002-NPXS39041_00-STOREUPD00000000.pkg"
        );
        assert_eq!(
            pkg_url_filename("UP9000-CUSA12345_00-GAMECONTENT12345"),
            "UP9000-CUSA12345_00-GAMECONTENT12345.pkg"
        );
    }

    #[test]
    fn pkg_host_url_for_builds_canonical_url() {
        let _g = ENV_LOCK.lock().unwrap();
        // Loopback always resolves — exercises the full URL assembly
        // (LAN IP lookup, port stamping, filename canonicalisation, path
        // pattern) that both install-start and dpi-direct-install share.
        // Pin the shape so a divergence between the two routes would
        // break Sony's installer header cross-check visibly here.
        let url = pkg_host_url_for(
            "127.0.0.1:9114",
            "abc-123",
            "UP9000-CUSA12345_00-GAMECONTENT12345",
        )
        .expect("loopback should resolve");
        assert!(
            url.starts_with("http://127.0.0.1:"),
            "URL should target loopback: {url}"
        );
        assert!(
            url.ends_with("/pkg-host/abc-123/UP9000-CUSA12345_00-GAMECONTENT12345.pkg"),
            "URL should carry session + canonical filename: {url}"
        );
    }

    #[test]
    fn pkg_host_url_for_uses_env_port_when_set() {
        let _g = ENV_LOCK.lock().unwrap();
        // The engine port is overridable via PS5UPLOAD_ENGINE_PORT; the
        // direct-install URL must honour it so the daemon fetches from
        // the same port the engine is actually listening on.
        std::env::set_var("PS5UPLOAD_ENGINE_PORT", "29113");
        let url = pkg_host_url_for("127.0.0.1:9114", "s", "IV0001-X").expect("loopback");
        std::env::remove_var("PS5UPLOAD_ENGINE_PORT");
        assert!(
            url.starts_with("http://127.0.0.1:29113/"),
            "URL should use env-overridden port: {url}"
        );
    }

    #[test]
    fn pkg_host_url_for_honours_advertised_ip_override() {
        let _g = ENV_LOCK.lock().unwrap();
        // Inside a Docker Desktop container the routing-table guess is a NAT
        // address the PS5 can't reach; PS5UPLOAD_PKG_HOST_IP pins the host's
        // LAN IP instead. It must win over lan_ip_for_ps5 regardless of the
        // ps5_addr, and an empty value must fall through to the guess.
        std::env::set_var("PS5UPLOAD_PKG_HOST_IP", "192.168.86.199");
        let url = pkg_host_url_for("192.168.86.100:9114", "s", "IV0001-X").expect("override ip");
        std::env::remove_var("PS5UPLOAD_PKG_HOST_IP");
        assert!(
            url.starts_with("http://192.168.86.199:"),
            "URL must use the advertised-IP override: {url}"
        );
        // An empty override must not be treated as a valid IP.
        std::env::set_var("PS5UPLOAD_PKG_HOST_IP", "   ");
        let url2 =
            pkg_host_url_for("127.0.0.1:9114", "s", "IV0001-X").expect("empty falls through");
        std::env::remove_var("PS5UPLOAD_PKG_HOST_IP");
        assert!(
            url2.starts_with("http://127.0.0.1:"),
            "empty override should fall back to the routing guess: {url2}"
        );
    }

    #[test]
    fn pkg_url_filename_fallback_when_empty() {
        assert_eq!(pkg_url_filename(""), "file.pkg");
        assert_eq!(pkg_url_filename("   "), "file.pkg");
    }

    #[test]
    fn pkg_url_filename_sanitises_meta_chars() {
        // Path-traversal / URL-escape attempts in a corrupt or hostile
        // header. Must not produce slashes, dots (other than the .pkg
        // suffix we add), or query/fragment chars.
        assert_eq!(pkg_url_filename("../etc/passwd"), "___etc_passwd.pkg");
        assert_eq!(pkg_url_filename("foo bar?baz=1"), "foo_bar_baz_1.pkg");
        assert_eq!(pkg_url_filename("a.b.c"), "a_b_c.pkg");
    }

    #[test]
    fn pkg_url_filename_caps_length() {
        // 200-char content_id (impossible per Sony spec but defensive)
        // is truncated to 64 before the .pkg suffix.
        let long = "A".repeat(200);
        let out = pkg_url_filename(&long);
        assert_eq!(out.len(), 64 + 4); // 64 chars + ".pkg"
        assert!(out.ends_with(".pkg"));
    }

    #[test]
    fn pkg_url_filename_handles_all_invalid_chars() {
        // String that sanitises to all-underscores still produces a
        // valid filename (not an empty one that would re-route the
        // request).
        let out = pkg_url_filename("...");
        assert!(out.ends_with(".pkg"));
        assert!(!out.starts_with('.'));
    }

    fn dummy_session(parts: Vec<(PathBuf, u64)>) -> InstallSession {
        let total: u64 = parts.iter().map(|(_, s)| s).sum();
        InstallSession {
            id: "test".into(),
            parts: parts.iter().map(|(p, _)| p.clone()).collect(),
            part_sizes: parts.iter().map(|(_, s)| *s).collect(),
            total_size: total,
            content_id: "TEST".into(),
            title: "Test".into(),
            package_type: "PS4GD".into(),
            package_fingerprint: String::new(),
            ps5_mgmt_addr: "127.0.0.1:0".into(),
            task_id: None,
            err_code: 0,
            detail: String::new(),
            cancelled: false,
            created_at_unix: 0,
            last_activity_unix: now_unix(),
            staging_path: None,
            terminal_status: None,
            launchable: None,
            serve_only: false,
            notified_console: false,
            install_start_free_bytes: None,
            progress_consumed_bytes: 0,
            last_progress_unix: None,
            stalled: false,
            accepted_unverified: false,
            requests_served: 0,
            last_rate_log_unix: 0,
            bytes_served: 0,
            transfer_bytes: 0,
            transfer: TransferCoverage::new(total),
            dpi_ok: None,
            dpi_rc: None,
            dpi_detail: String::new(),
            remote: None,
        }
    }

    /// A long install must not be reaped while the console is still pulling.
    /// Expiry is measured from the last served range, not from creation: a
    /// 200-300 GB package on a modest link runs well past the 2 h ceiling, and
    /// ageing it out by creation time 404'd the console's next range — what
    /// users reported as the transfer losing connection.
    #[test]
    fn a_session_still_being_fetched_is_never_aged_out() {
        let max_age = pkg_session_max_age_sec();
        let now = now_unix();

        // Created 5 hours ago — far past the ceiling — but fetched a second
        // ago, i.e. an install that has been running all night and is fine.
        let mut busy = dummy_session(vec![]);
        busy.created_at_unix = now - (5 * 60 * 60);
        busy.last_activity_unix = now - 1;

        // Same age, but nothing has touched it since it was made.
        let mut abandoned = dummy_session(vec![]);
        abandoned.created_at_unix = now - (5 * 60 * 60);
        abandoned.last_activity_unix = abandoned.created_at_unix;

        let idle =
            |s: &InstallSession| now.saturating_sub(s.last_activity_unix.max(s.created_at_unix));
        assert!(
            idle(&busy) < max_age,
            "an actively fetched session must be kept"
        );
        assert!(
            idle(&abandoned) >= max_age,
            "a genuinely abandoned session must still be reaped"
        );
    }

    /// A remote-backed session must satisfy ranges from the HTTP origin
    /// instead of the (empty) `parts` list. This is the seam between the
    /// range proxy and the pkg-host serve path: if `read_split_range` ever
    /// stopped delegating, an install-from-a-link would serve zero bytes or
    /// hit "No such file", and only a test that goes through a session catches
    /// it.
    #[test]
    fn a_remote_backed_session_serves_ranges_from_the_origin() {
        use crate::remote_pkg::origin_tests::{body, spawn_origin};

        let total = 3 * 1024 * 1024usize;
        let data = body(total);
        let origin = spawn_origin(data.clone(), 0, false);
        let remote = Arc::new(crate::remote_pkg::RemoteSource::new(
            format!("http://{}/game.pkg", origin.addr),
            total as u64,
        ));

        // A link install has no local parts at all — every byte is proxied.
        let mut session = dummy_session(vec![]);
        session.total_size = total as u64;
        session.remote = Some(std::sync::Arc::new(RemotePkg::Http(remote)));

        for (start, len) in [(0u64, 4096u64), (1_000_000, 200_000), (total as u64 - 8, 8)] {
            let end = start + len - 1;
            let got = read_split_range(&session, start, end)
                .unwrap_or_else(|e| panic!("remote session read {start}..={end} failed: {e}"));
            assert_eq!(
                got,
                &data[start as usize..=end as usize],
                "wrong bytes served for {start}..={end}"
            );
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5upload-crc-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_fake_fpkg(dir: &std::path::Path, crc: &[u8]) -> (PathBuf, u64) {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file(
            "config/UP0000-PPSA01234_00-TESTGAME00000000/playgo-chunk.crc",
            opts,
        )
        .unwrap();
        zw.write_all(crc).unwrap();
        let mut bytes = vec![0xCD; 0x10000];
        bytes.extend_from_slice(&zw.finish().unwrap().into_inner());
        let path = dir.join("game.pkg");
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes.len() as u64)
    }

    #[test]
    fn crc_request_served_from_embedded_zip() {
        let dir = scratch_dir("embedded");
        let crc: Vec<u8> = (0u8..76).collect();
        let (path, size) = write_fake_fpkg(&dir, &crc);
        let s = dummy_session(vec![(path, size)]);
        match resolve_crc(&s, "UP0000-PPSA01234_00-TESTGAME00000000.crc").unwrap() {
            Some(ServeSource::EmbeddedCrc { offset, len }) => {
                assert_eq!(len, 76);
                assert_eq!(read_split_range(&s, offset, offset + len - 1).unwrap(), crc);
            }
            _ => panic!("expected the embedded member"),
        }
    }

    #[test]
    fn crc_request_falls_back_to_sibling_file() {
        let dir = scratch_dir("sibling");
        let pkg = dir.join("game.pkg");
        std::fs::write(&pkg, vec![0u8; 0x20000]).unwrap();
        std::fs::write(
            dir.join("UP0000-PPSA01234_00-TESTGAME00000000.crc"),
            b"sidecar",
        )
        .unwrap();
        let s = dummy_session(vec![(pkg, 0x20000)]);
        match resolve_crc(&s, "UP0000-PPSA01234_00-TESTGAME00000000.crc").unwrap() {
            Some(ServeSource::SiblingCrc(bytes)) => assert_eq!(&bytes[..], b"sidecar"),
            _ => panic!("expected the sibling file"),
        }
    }

    #[test]
    fn crc_request_without_any_source_is_none() {
        let dir = scratch_dir("none");
        let pkg = dir.join("game.pkg");
        std::fs::write(&pkg, vec![0u8; 0x20000]).unwrap();
        let s = dummy_session(vec![(pkg, 0x20000)]);
        assert!(resolve_crc(&s, "UP0000-PPSA01234_00-TESTGAME00000000.crc")
            .unwrap()
            .is_none());
    }

    #[test]
    fn crc_request_never_leaves_the_package_directory() {
        let dir = scratch_dir("traversal");
        let pkg = dir.join("game.pkg");
        std::fs::write(&pkg, vec![0u8; 0x20000]).unwrap();
        std::fs::write(dir.parent().unwrap().join("evil.crc"), b"x").unwrap();
        let s = dummy_session(vec![(pkg, 0x20000)]);
        for name in ["../evil.crc", "..\\evil.crc", "sub/evil.crc", ".crc"] {
            assert!(resolve_crc(&s, name).unwrap().is_none(), "{name}");
        }
    }

    #[test]
    fn range_header_full_when_absent() {
        let h = HeaderMap::new();
        assert_eq!(parse_range_header(&h, 100).unwrap(), (0, 99));
    }

    #[test]
    fn range_header_explicit() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=10-20".parse().unwrap());
        assert_eq!(parse_range_header(&h, 100).unwrap(), (10, 20));
    }

    #[test]
    fn range_header_open_end() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=50-".parse().unwrap());
        assert_eq!(parse_range_header(&h, 100).unwrap(), (50, 99));
    }

    #[test]
    fn range_header_invalid() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=200-300".parse().unwrap());
        assert!(parse_range_header(&h, 100).is_err());
    }

    #[test]
    fn a_large_range_is_served_whole_never_trimmed() {
        // Sony reads a package's trailing metadata in ONE request whose size
        // scales with the package, and does not re-request a tail it did not
        // get. We used to trim anything over 16 MiB, which is legal HTTP but
        // failed every large install: a 121 GB FF16 asked for 249.6 MiB in one
        // range and was refused with 0x80b211cd immediately after our 16 MiB
        // answer. The body is streamed, so serving it whole is bounded work.
        let mut h = HeaderMap::new();
        let total: u64 = 121 * 1024 * 1024 * 1024;
        let start_at: u64 = 120 * 1024 * 1024 * 1024;
        let want: u64 = 262 * 1024 * 1024; // ~the measured footer read
        h.insert(
            header::RANGE,
            format!("bytes={start_at}-{}", start_at + want - 1)
                .parse()
                .unwrap(),
        );
        let (start, end) = parse_range_header(&h, total).unwrap();
        assert_eq!(start, start_at);
        assert_eq!(
            end - start + 1,
            want,
            "the full requested range must be served"
        );
    }

    #[test]
    fn a_whole_file_range_is_served_whole() {
        // The extreme case: `bytes=0-{total-1}` on a 50 GB package. Streaming
        // means this no longer implies a 50 GB allocation.
        let mut h = HeaderMap::new();
        let total: u64 = 50 * 1024 * 1024 * 1024;
        h.insert(
            header::RANGE,
            format!("bytes=0-{}", total - 1).parse().unwrap(),
        );
        let (start, end) = parse_range_header(&h, total).unwrap();
        assert_eq!((start, end), (0, total - 1));
    }

    #[test]
    fn no_range_header_serves_the_whole_file() {
        // A bare GET now yields the entire package rather than a capped
        // prefix. Returning a truncated body with a 206 was how a non-Range
        // consumer could mistake an incomplete package for a complete one.
        let h = HeaderMap::new();
        let total: u64 = 50 * 1024 * 1024 * 1024;
        let (start, end) = parse_range_header(&h, total).unwrap();
        assert_eq!((start, end), (0, total - 1));
        // And with the whole file covered, it is a plain 200, not a 206.
        assert!(!serve_is_partial(false, end, total));
    }

    #[test]
    fn small_total_no_range_returns_full_file() {
        // Tiny pkg with no Range header: cap doesn't kick in, full
        // file is returned.
        let h = HeaderMap::new();
        let (start, end) = parse_range_header(&h, 100).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 99);
    }

    #[test]
    fn serve_partial_status_matches_body_coverage() {
        let total: u64 = 50 * 1024 * 1024 * 1024;
        // A bare GET now covers the whole file, so it is a plain 200 — there is
        // no longer a truncated body that would need a 206 to be honest about.
        let (s, e) = parse_range_header(&HeaderMap::new(), total).unwrap();
        assert_eq!((s, e), (0, total - 1));
        assert!(
            !serve_is_partial(false, e, total),
            "a whole-file bare GET must be 200"
        );
        // The 206 rule still holds for any body that really is a subset.
        assert!(serve_is_partial(false, total - 2, total));
        // Bare GET on a small pkg that fits → bare 200.
        let (_s2, e2) = parse_range_header(&HeaderMap::new(), 100).unwrap();
        assert!(
            !serve_is_partial(false, e2, 100),
            "whole-file bare GET must be 200"
        );
        // Any explicit Range → 206, even if it happens to cover the file.
        assert!(serve_is_partial(true, 99, 100));
    }

    #[test]
    fn an_ordinary_small_range_passes_through_unchanged() {
        // A reasonable BGFT fetch (a few MB) is returned exactly as asked.
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=1000-2000".parse().unwrap());
        let (start, end) = parse_range_header(&h, 10_000_000).unwrap();
        assert_eq!(start, 1000);
        assert_eq!(end, 2000);
    }

    #[test]
    fn split_range_reads_within_one_part() {
        let dir = std::env::temp_dir().join(format!("pkg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p1 = dir.join("a.pkg");
        std::fs::write(&p1, b"abcdefghij").unwrap();
        let s = dummy_session(vec![(p1, 10)]);
        let chunk = read_split_range(&s, 2, 5).unwrap();
        assert_eq!(chunk, b"cdef");
    }

    #[test]
    fn split_range_crosses_parts() {
        let dir = std::env::temp_dir().join(format!("pkg-test2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p1 = dir.join("a.pkg");
        let p2 = dir.join("a.pkg.0");
        let p3 = dir.join("a.pkg.1");
        std::fs::write(&p1, b"AAAA").unwrap();
        std::fs::write(&p2, b"BBBB").unwrap();
        std::fs::write(&p3, b"CCCC").unwrap();
        let s = dummy_session(vec![(p1, 4), (p2, 4), (p3, 4)]);
        // Take the last byte of part0, all of part1, first byte of part2.
        let chunk = read_split_range(&s, 3, 8).unwrap();
        assert_eq!(chunk, b"ABBBBC");
    }

    #[test]
    fn sessions_lock_recovers_from_poison() {
        // Pin the poison-recovery contract for the sessions Mutex.
        // Every route handler now uses `.lock().unwrap_or_else(|e|
        // e.into_inner())` so a panic that propagates while the lock
        // is held doesn't permanently wedge the install API. This test
        // simulates that exact failure mode: hold the lock, panic
        // (mutex becomes poisoned), then verify a fresh `.lock()`
        // call still recovers the inner data via the recovery
        // pattern.
        let state = PkgInstallState::default();

        // Insert one session before the simulated panic so we can
        // assert the data isn't lost on recovery.
        {
            let mut sessions = state.sessions.lock().unwrap();
            sessions.insert(
                "before-panic".to_string(),
                dummy_session(vec![(PathBuf::from("/tmp/x"), 1)]),
            );
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Hold the lock, panic — this is what poisons the mutex.
            let _guard = state.sessions.lock().unwrap();
            panic!("simulated route-handler panic");
        }));
        assert!(result.is_err(), "the panic should propagate");
        assert!(
            state.sessions.is_poisoned(),
            "mutex must be poisoned after a panic-while-holding"
        );

        // The recovery pattern used at every call site in the engine:
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            sessions.len(),
            1,
            "data inserted before the panic must still be reachable"
        );
        assert!(sessions.contains_key("before-panic"));
    }

    #[test]
    fn install_start_request_serve_only_defaults_false() {
        // A normal install request (no serve_only key) must default to a
        // real install — serve_only=false — so the staged/normal path is
        // untouched. This is the safety default: any caller that forgets the
        // field gets the install, not a silent no-op.
        let req: InstallStartRequest = serde_json::from_str(
            r#"{"ps5_addr":"1.2.3.4:9114","path":"/x.pkg","delete_staging":true}"#,
        )
        .expect("parse");
        assert!(!req.serve_only);
        assert!(req.delete_staging);
    }

    #[test]
    fn install_start_request_serve_only_true_parses() {
        // The Stream-beta client sends serve_only=true to register the
        // pkg-host session WITHOUT the in-process InstallByPackage (which
        // hangs the FW<11 helper). Pin that the field round-trips so the
        // client↔engine contract can't silently regress to the crashing path.
        let req: InstallStartRequest = serde_json::from_str(
            r#"{"ps5_addr":"1.2.3.4:9114","path":"/x.pkg","serve_only":true}"#,
        )
        .expect("parse");
        assert!(req.serve_only);
        // delete_staging still defaults true even when omitted here.
        assert!(req.delete_staging);
    }

    // ── parse_range_header ─────────────────────────────────────────────
    //
    // Pin the RFC 9110 range-header parser so a regression in the suffix-
    // range support or the open-end handling is caught immediately.

    fn range_headers(spec: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::RANGE,
            axum::http::HeaderValue::from_str(spec).unwrap(),
        );
        h
    }

    #[test]
    fn range_no_header_returns_prefix_capped() {
        let total = 100_000_000u64;
        let (start, end) = parse_range_header(&HeaderMap::new(), total).unwrap();
        assert_eq!(start, 0);
        assert!(end < total, "end must be capped below total");
    }

    #[test]
    fn range_start_end() {
        let (start, end) = parse_range_header(&range_headers("bytes=100-199"), 1000).unwrap();
        assert_eq!(start, 100);
        assert_eq!(end, 199);
    }

    #[test]
    fn range_open_end() {
        // bytes=500- → start=500, end=total-1
        let (start, end) = parse_range_header(&range_headers("bytes=500-"), 1000).unwrap();
        assert_eq!(start, 500);
        assert_eq!(end, 999);
    }

    #[test]
    fn range_end_past_eof_is_clamped() {
        // What the console's installer sends for the final block of a package
        // that is not 64 KiB-aligned: whole blocks, so the end runs past EOF.
        let (start, end) =
            parse_range_header(&range_headers("bytes=2293760-2359295"), 2319261).unwrap();
        assert_eq!(start, 2293760);
        assert_eq!(end, 2319260);
        // A start past the end is still unsatisfiable.
        assert!(parse_range_header(&range_headers("bytes=2319261-2359295"), 2319261).is_err());
    }

    #[test]
    fn range_suffix() {
        // bytes=-200 → last 200 bytes of a 1000-byte file
        let (start, end) = parse_range_header(&range_headers("bytes=-200"), 1000).unwrap();
        assert_eq!(start, 800);
        assert_eq!(end, 999);
    }

    #[test]
    fn range_suffix_larger_than_total() {
        // bytes=-2000 on a 1000-byte file → saturating_sub gives 0
        let (start, end) = parse_range_header(&range_headers("bytes=-2000"), 1000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 999);
    }

    #[test]
    fn range_invalid_missing_bytes_prefix() {
        let result = parse_range_header(&range_headers("0-99"), 1000);
        assert!(result.is_err());
    }

    #[test]
    fn range_invalid_no_dash() {
        let result = parse_range_header(&range_headers("bytes=100"), 1000);
        assert!(result.is_err());
    }

    #[test]
    fn range_start_after_end_rejected() {
        let result = parse_range_header(&range_headers("bytes=200-100"), 1000);
        assert!(result.is_err());
    }

    fn sample_artifact(kind: &str, size: u64, fp: &str, cid: &str) -> InstalledPkgArtifact {
        InstalledPkgArtifact {
            kind: kind.to_string(),
            path: "/mnt/ext1/user/app/PPSA17221/app.pkg".into(),
            size,
            fingerprint: fp.into(),
            content_id: cid.into(),
        }
    }

    #[test]
    fn ps4_artifact_matches_on_fingerprint() {
        let a = sample_artifact(
            "base",
            3827433472,
            "49983d5f",
            "UP9000-CUSA07842_00-SCUS974290000001",
        );
        assert!(artifact_identity_matches(
            &a,
            "base",
            "UP9000-CUSA07842_00-SCUS974290000001",
            3827433472,
            "49983d5f",
            "PS4GD",
        ));
        assert!(!artifact_identity_matches(
            &a,
            "base",
            "UP9000-CUSA07842_00-SCUS974290000001",
            3827433472,
            "deadbeef",
            "PS4GD",
        ));
    }

    #[test]
    fn ps5_fpkg_matches_inner_app_pkg_by_content_id() {
        // Outer FIH we streamed vs inner image Sony wrote.
        let inner = sample_artifact(
            "base",
            1_333_460_992,
            "c08d913daab17da3764b0627f70ffa72122f366f959e8e296c3d69adf07ed1cc",
            "UP4433-PPSA17221_00-MINECRAFTPS50000",
        );
        assert!(artifact_identity_matches(
            &inner,
            "base",
            "UP4433-PPSA17221_00-MINECRAFTPS50000",
            1_345_936_761,
            "f9545e9cba776ca44864243b950466e7c55017644e400e1ebfe25e2ec8b3987f",
            "PS5GD",
        ));
        assert!(!artifact_identity_matches(
            &inner,
            "base",
            "UP0000-PPSA00000_00-SOMEOTHERGAME000",
            1_345_936_761,
            "f9545e9cba776ca44864243b950466e7c55017644e400e1ebfe25e2ec8b3987f",
            "PS5GD",
        ));
    }

    /// Coverage is what makes the stream progress bar honest. Both of the
    /// obvious alternatives were wrong on hardware, in opposite directions.
    #[test]
    fn coverage_counts_distinct_bytes_only() {
        let total = 1_345_936_761u64; // the Minecraft pkg that exposed this
        let mut c = TransferCoverage::new(total);

        // Sony reads the container's trailing index first. A furthest-offset
        // metric read 100% here; coverage says almost nothing has arrived.
        c.mark(total - 4096, total - 1);
        assert!(c.bytes() < total / 100, "tail fetch must not read as done");

        // The bulk, in order.
        c.mark(0, 100_000_000);
        assert!(c.bytes() >= 100_000_000);
        assert!(c.bytes() < total);
    }

    #[test]
    fn coverage_ignores_a_refetch() {
        // Measured: 1.53 GB served for a 1.35 GB package, because Sony
        // re-requests ranges. A summed byte count passes 100% during the
        // transfer; coverage cannot.
        let total = 1_345_936_761u64;
        let mut c = TransferCoverage::new(total);
        c.mark(0, total - 1);
        assert_eq!(c.bytes(), total);
        c.mark(0, total - 1);
        c.mark(500, 900);
        assert_eq!(c.bytes(), total);
    }

    #[test]
    fn coverage_is_never_short_of_the_package() {
        let mut c = TransferCoverage::new(1000);
        c.mark(0, 999);
        assert_eq!(c.bytes(), 1000);
        // A request past the end (a stale Range, or the .crc sidecar's offset
        // space) must not inflate the total.
        c.mark(1000, 5000);
        assert_eq!(c.bytes(), 1000);
        // Degenerate package.
        let empty = TransferCoverage::new(0);
        assert_eq!(empty.bytes(), 0);
    }

    #[test]
    fn coverage_is_bounded_whatever_the_package_size() {
        // The bitmap is what keeps this per-session state small: 8 KiB whether
        // the package is 100 MB or 200 GB.
        for total in [1u64 << 20, 1 << 30, 200 << 30] {
            let c = TransferCoverage::new(total);
            assert!(
                c.words.len() <= 1024,
                "{} bytes → {} words",
                total,
                c.words.len()
            );
        }
    }
}
