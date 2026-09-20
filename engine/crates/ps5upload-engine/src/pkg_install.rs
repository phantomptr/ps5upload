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
use ps5upload_core::app_lifecycle::{toast_send, ToastRequest};
use ps5upload_core::pkg_install::{
    err_code_message, pkg_install, pkg_install_status, InstallPhase, PkgInstallRequest,
    PkgInstallResponse, PkgInstallStatus, APPINST_VIA_LOCAL_FLAG, APPINST_VIA_SHELLUI_FLAG,
    APPINST_VIA_TIER0_FLAG,
};
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
pub type RemotePkg = crate::remote_pkg::RemoteSource;

#[cfg(target_os = "android")]
#[derive(Debug)]
pub enum RemotePkg {}

#[cfg(target_os = "android")]
impl RemotePkg {
    pub fn read_range(&self, _start: u64, _end: u64) -> std::io::Result<Vec<u8>> {
        match *self {}
    }
}

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
}

pub type PkgInstallStateHandle = Arc<PkgInstallState>;

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
        .route("/api/pkg/install/start", post(install_start_handler))
        .route("/api/pkg/install/status", get(install_status_handler))
        .route("/api/pkg/install/sessions", get(install_sessions_handler))
        .route("/api/pkg/install/cancel", post(install_cancel_handler))
        .route("/api/pkg/installed", get(installed_pkg_inventory_handler))
        // "Do you already have this?" answered by the same artifact matching the
        // install tracker uses, so the UI and the completion check can't
        // disagree. Read-only; safe to poll from the package list.
        .route("/api/pkg/install/preflight", get(install_preflight_handler))
        // Install a staged .pkg through the standalone DPI daemon (:9040).
        // The daemon runs sceAppInstUtilAppInstallPkg from a clean loader
        // process — installs without the PlayGo gate. Caller stages the
        // pkg first and passes the bare PS5 path. See payload/dpi/.
        .route("/api/pkg/dpi-install", post(dpi_install_handler))
        // Bring that daemon up in the first place, and put the ps5upload
        // helper back afterwards. The desktop client does both itself from
        // its own embedded ELFs; a browser can do neither, which left the
        // web UI with no DPI fallback at all — so no way to install a game
        // patch (the web UI half of #152). See `bundled_payload`.
        .route("/api/pkg/dpi-ensure", post(dpi_ensure_handler))
        .route("/api/pkg/payload-restore", post(payload_restore_handler))
        // Direct/streaming install (beta, #81): skip the staging upload
        // entirely — the engine serves the pkg at /pkg-host/ and the DPI
        // daemon pulls it straight over HTTP. Useful when PS5 disk space
        // is tight or for a quick one-shot install from a machine that
        // already has the pkg mounted.
        .route(
            "/api/pkg/dpi-direct-install",
            post(dpi_direct_install_handler),
        )
        // The session UUID is the lookup key. We allow ANY {filename} so the
        // URL can carry the pkg's canonical `<ContentID>.pkg` name that
        // Sony's installer cross-checks against the pkg header. Without
        // this Sony rejects with 0x80B21106 on user-renamed pkgs (file
        // header says "FOO" but URL ends in "bar.pkg" — installer treats
        // them as inconsistent). A name ending in `.crc` is the console
        // asking for the package's PlayGo CRC table (#319); see serve_handler.
        .route("/pkg-host/{session}/{filename}", get(serve_handler))
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

/// Internal hand-off code shared with the payload's `BGFT_ERR_DPI_REQUIRED`.
/// It is intentionally non-zero so every existing client follows its normal
/// rejected-start -> standalone-DPI fallback without mistaking the hand-off
/// for a completed install.
const DLC_DPI_REQUIRED_ERR: u32 = 0xE000_0008;

/// Whether a staged install should skip the main payload and go straight to
/// the standalone DPI daemon. Stream mode already invokes DPI directly, and a
/// non-local install has no staged path to hand to the local-path DPI route.
///
/// DLC (`…AC`) always: on FW 9.60 the in-process call can remove an
/// already-installed add-on before returning a rejection, so the payload's
/// whole cascade is unsafe for it.
///
/// Patch (`…DP`) only when DPI is `dpi_up`: the in-process attempt is measured
/// to fail on both of our consoles (0x80B2150F on FW 5.10, 0x80B2116F on FW
/// 9.60), so with DPI already listening the attempt is a guaranteed rejection
/// worth skipping. Without DPI it is NOT safe to skip — the in-process path
/// does apply patches on some firmware points, and refusing up front would
/// turn a working install into a failure on a console whose loader is simply
/// not running.
fn staged_requires_dpi(is_local: bool, serve_only: bool, package_type: &str, dpi_up: bool) -> bool {
    if !is_local || serve_only {
        return false;
    }
    package_type.ends_with("AC") || (package_type.ends_with("DP") && dpi_up)
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

#[derive(Debug, Serialize)]
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
    /// Per-tier err codes — null when tier wasn't attempted, 0 when
    /// it completed cleanly, otherwise the tier's err_code. Lets the
    /// host UI distinguish "Tier 1 silently bailed out" from "Tier 1
    /// reached Sony, who returned X". See `PkgInstallResponse`.
    pub shellui_err: Option<u32>,
    pub appinst_err: Option<u32>,
    /// Which install tier accepted this task — derived from the task_id
    /// bits set by the payload's bgft.c. Surfaced to the desktop's
    /// "Why?" diagnostic disclosure so the user (and us, during
    /// bug reports) sees whether the in-process appinst path took it,
    /// the SceShellUI RPC fallback did, or the legacy direct-BGFT
    /// path. See `ps5upload_core::pkg_install::via_tier`.
    pub via: String,
    /// True when the install was accepted via the unlaunchable last-resort
    /// path (`register_path == "appinst-local"`). The title installs but may
    /// fail to start ("can't start the game or app") on some firmwares —
    /// notably FW 12.xx. The UI shows a warning and points the user at the
    /// PS5's Settings → Package Installer to re-install if it won't boot.
    /// See `ps5upload_core::pkg_install::install_may_not_launch`.
    pub may_not_launch: bool,
    /// The package_type the install actually ran with, AFTER the engine's
    /// staged-pkg category parse (so a "…DP" here means "this was treated as a
    /// patch"). Lets the client recognise guarded patch/DLC hand-offs even on
    /// USB/queue/File-System paths where it sent no type, then select the safe
    /// category-aware DPI fallback.
    #[serde(default)]
    pub package_type: String,
}

async fn install_start_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<InstallStartRequest>,
) -> Response<Body> {
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
    if is_local && !type_declared {
        if let Some(local_path) = req.local_ps5_path.clone() {
            let addr = req.ps5_addr.clone();
            let parsed = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                tokio::task::spawn_blocking(move || {
                    ps5upload_pkg::metadata_from_reader(|off, len| {
                        ps5upload_core::fs_ops::fs_read(&addr, &local_path, off, len).ok()
                    })
                    .and_then(|meta| {
                        ps5upload_pkg::package_type_for_category_and_platform(
                            &meta.category,
                            &meta.platform,
                        )
                        .map(|pt| (meta.category, pt))
                    })
                }),
            )
            .await;
            if let Ok(Ok(Some((cat, pt)))) = parsed {
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
    {
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
        sessions.insert(session_id.clone(), session.clone());
    }

    // Serve-only (Stream beta): the session + its /pkg-host/ listener are now
    // live, so the DPI daemon can pull bytes. Return WITHOUT running the
    // in-process InstallByPackage — on FW < 11 that call against an http:// URL
    // hangs the payload until the watchdog kills the helper. The client's next
    // step (`/api/pkg/dpi-direct-install`) performs the real install.
    if req.serve_only {
        crate::log_info!(
            "pkg_install serve-only: addr={} session={} url={} content_id={} title={:?} — skipping in-process install; DPI daemon will pull",
            req.ps5_addr,
            session_id,
            url,
            session.content_id,
            session.title,
        );
        return json_ok(&InstallStartResponse {
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
        });
    }

    // Do not even send a staged DLC install frame to the main payload. This is
    // deliberately enforced engine-side as well as payload-side so a current
    // desktop remains safe when the console still has an older payload loaded.
    // Return a typed start rejection; runPkgInstallCore then starts standalone
    // DPI and verifies the exact add-on fingerprint before showing success.
    //
    // A staged patch is routed the same way, but only when the DPI daemon is
    // already listening — see `staged_requires_dpi`. Probing :9040 is safe
    // (it is our own accept loop, and `dpi_ensure` probes it the same way);
    // probing :9021 is NOT, because a loader that gets a connect-and-close
    // with no bytes can execute an empty image.
    let dpi_up = if package_type.ends_with("DP") && is_local && !req.serve_only {
        let dpi_addr = ps5upload_core::payload_lifecycle::join_host_port(
            &strip_host_port(&req.ps5_addr),
            ps5upload_core::payload_lifecycle::DPI_DAEMON_PORT,
        );
        // spawn_blocking: `port_is_open` is a synchronous connect with a
        // 1.5 s timeout, and this handler runs on the tokio runtime.
        tokio::task::spawn_blocking(move || {
            ps5upload_core::payload_lifecycle::port_is_open(&dpi_addr, DPI_PROBE_TIMEOUT)
        })
        .await
        .unwrap_or(false)
    } else {
        false
    };
    if staged_requires_dpi(is_local, req.serve_only, &package_type, dpi_up) {
        crate::log_info!(
            "pkg_install: staged {} session={} routed directly to standalone DPI; main payload skipped",
            if package_type.ends_with("AC") { "DLC" } else { "patch" },
            session_id,
        );
        return json_ok(&InstallStartResponse {
            session_id,
            url,
            task_id: -1,
            err_code: DLC_DPI_REQUIRED_ERR,
            err_message: err_code_message(DLC_DPI_REQUIRED_ERR).map(str::to_string),
            detail: "safe staged-DLC hand-off".to_string(),
            may_not_launch: false,
            register_path: "dpi-required".to_string(),
            intdebug_avail: false,
            kernel_rw: false,
            shellui_err: None,
            appinst_err: None,
            via: "dpi-required".to_string(),
            package_type,
        });
    }

    let install_req = PkgInstallRequest {
        url: url.clone(),
        content_id: session.content_id.clone(),
        size: session.total_size,
        title: session.title.clone(),
        package_type: package_type.clone(),
        method: None,
    };

    crate::log_info!(
        "pkg_install: addr={} session={} url={} content_id={} title={:?} package_type={} parts={} total={} bytes delete_staging={} staging_path={:?}",
        req.ps5_addr,
        session_id,
        url,
        session.content_id,
        session.title,
        install_req.package_type,
        session.parts.len(),
        total_size,
        req.delete_staging,
        session.staging_path,
    );

    // Run the blocking PS5 frame exchange OFF the async reactor. `pkg_install`
    // does synchronous TCP I/O (connect backoff + up-to-30s read timeout); the
    // bare `#[tokio::main]` runtime has only num_cpus worker threads, so calling
    // it inline would park a reactor thread for the whole RPC. Against a wedged
    // or unreachable console a few concurrent installs would occupy every
    // worker thread and stall the ENTIRE engine — SSE, /pkg-host serving, and
    // every OTHER console's requests. spawn_blocking keeps the reactor free so
    // 12 consoles stay independent. (Mirrors dpi_install_handler.)
    let resp: PkgInstallResponse = {
        let addr = req.ps5_addr.clone();
        let rollback = |e: String| {
            state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&session_id);
            crate::log_warn!(
                "pkg_install RPC failed: session={} addr={} err={}",
                session_id,
                req.ps5_addr,
                e,
            );
            json_err(
                StatusCode::BAD_GATEWAY,
                &format!("payload PKG_INSTALL failed: {e}"),
            )
        };
        match tokio::task::spawn_blocking(move || pkg_install(&addr, &install_req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return rollback(e.to_string()),
            Err(e) => return rollback(format!("install task panicked/cancelled: {e}")),
        }
    };

    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sess) = sessions.get_mut(&session_id) {
            sess.task_id = Some(resp.task_id);
            sess.err_code = resp.err_code;
            sess.detail = resp.detail.clone();
        }
    }

    let err_message = err_code_message(resp.err_code).map(|s| s.to_string());

    if resp.err_code == 0 {
        crate::log_info!(
            "pkg_install ok: session={} task_id={} register_path={} intdebug_avail={} kernel_rw={}",
            session_id,
            resp.task_id,
            resp.register_path,
            resp.intdebug_avail,
            resp.kernel_rw,
        );
    } else {
        // Sony rejected the register call. Log enough context to
        // diagnose post-mortem without ssh — the diagnostic disclosure
        // in the UI shows the same fields, but engine.log gives an
        // append-only history per attempt.
        // `error`: Sony refused the install outright, so the user's package
        // did not land. Terminal and user-visible — see the note in
        // `job_failed_from_err` on why this is not a `warn`.
        crate::log_error!(
            "pkg_install rejected: session={} err_code=0x{:08x} detail={:?} register_path={} intdebug_avail={} kernel_rw={} shellui_err={} appinst_err={}",
            session_id,
            resp.err_code,
            resp.detail,
            resp.register_path,
            resp.intdebug_avail,
            resp.kernel_rw,
            resp.shellui_err.map_or("null".to_string(), |e| format!("0x{e:08x}")),
            resp.appinst_err.map_or("null".to_string(), |e| format!("0x{e:08x}")),
        );
        // A rejected start can still continue through the standalone DPI
        // daemon for every package type. DPI consumes this SAME on-console
        // path, so register rejection must never take()/delete it first. This
        // also matches the setting's promise: "Auto Delete after installation"
        // cannot fire when no installation has completed. The client keeps the
        // package for retry and terminal confirmed-complete cleanup remains the
        // sole deletion authority.
        debug_assert!(ps5upload_core::pkg_install::preserve_staging_on_reject(
            &package_type
        ));
        crate::log_info!(
            "register-reject staging PRESERVED for DPI fallback: session={} package_type={}",
            session_id,
            package_type,
        );
    }

    json_ok(&InstallStartResponse {
        session_id,
        url,
        task_id: resp.task_id,
        err_code: resp.err_code,
        err_message,
        detail: resp.detail,
        // Borrow register_path for may_not_launch BEFORE the move below —
        // struct fields evaluate in source order.
        may_not_launch: ps5upload_core::pkg_install::install_may_not_launch(&resp.register_path),
        register_path: resp.register_path,
        intdebug_avail: resp.intdebug_avail,
        kernel_rw: resp.kernel_rw,
        shellui_err: resp.shellui_err,
        appinst_err: resp.appinst_err,
        via: ps5upload_core::pkg_install::via_tier(resp.task_id).to_string(),
        package_type,
    })
}

// ─── /api/pkg/install/status ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    pub session: String,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub session_id: String,
    pub phase: InstallPhase,
    pub downloaded: u64,
    pub total: u64,
    pub err_code: u32,
    pub err_message: Option<String>,
    pub detail: String,
    pub cancelled: bool,
    /// Live diagnostic snapshot (matches the install/start ack shape).
    /// Pre-2.2.52-fix-round-2 the host only saw these from install/start;
    /// if BGFT transitioned to phase=error mid-install the user's
    /// "Why?" disclosure showed start-time values that said everything
    /// was fine. Now they refresh on every status poll. Empty / false
    /// for installs against pre-2.2.52 payloads (serde defaults).
    pub register_path: String,
    pub intdebug_avail: bool,
    pub kernel_rw: bool,
    /// Live per-tier err breakdown — same semantic as the install/start
    /// response, refreshed every status poll. See `InstallStartResponse`.
    pub shellui_err: Option<u32>,
    pub appinst_err: Option<u32>,
    /// Same tier identifier surfaced from install/start — re-derived
    /// here so the status poll's response is self-contained (the UI
    /// can read it without correlating against the start ack). See
    /// `ps5upload_core::pkg_install::via_tier`.
    pub via: String,
    /// Bytes the progress tracker has observed the install consume so far
    /// (`max(free-space drop, title-dir size)`). Drives the client's live
    /// install % (`installed_bytes / total`) for large titles where Sony's
    /// BGFT progress isn't meaningful (the file:// staging path). 0 until the
    /// first post-accept poll. See `install_verdict` / `observe_consumed`.
    #[serde(default)]
    pub installed_bytes: u64,
    /// True when the install was declared *stalled* — no disk progress past the
    /// adaptive deadline. Terminal like an error, but the staged pkg is KEPT so
    /// the user can retry. The client shows a "stalled — package kept" message
    /// instead of a generic failure, and must NOT delete the pkg. See
    /// `InstallVerdict::Stalled`.
    #[serde(default)]
    pub stalled: bool,
    /// True when Sony accepted a synthetic-DONE request but host-side
    /// registration/byte signals could not prove it finished. The client
    /// surfaces a warning and keeps the package.
    #[serde(default)]
    pub accepted_unverified: bool,
    /// path (`register_path == "appinst-local"`) — re-derived every poll so
    /// the status response is self-contained. The title installs but may not
    /// start on some firmwares (notably FW 12.xx). See
    /// `ps5upload_core::pkg_install::install_may_not_launch`.
    pub may_not_launch: bool,
    /// Definitive launchability from the engine's app.db verification
    /// (elf-arsenal `wait_for_install_row` analogue), once the install
    /// reaches Done:
    ///   `Some(true)`  — the title appeared in the PS5's app.db; it is
    ///                   launchable (overrides the `may_not_launch`
    ///                   heuristic — even an `appinst-local` install shows
    ///                   a clean success once verified).
    ///   `Some(false)` — the title never registered within the verification
    ///                   window; treat as "installed but won't launch".
    ///   `None`        — verification not applicable (app.db unreadable on
    ///                   this firmware, or no real title_id) — fall back to
    ///                   the `may_not_launch` heuristic. Pre-verification
    ///                   payloads/clients see this and behave as before.
    #[serde(default)]
    pub launchable: Option<bool>,
    /// Bytes the console has fetched from `/pkg-host/`, as the furthest byte it
    /// asked for (monotonic, clamped to `total`). Only a Stream/serve-only
    /// session moves this — a staged install reads from PS5-local disk and
    /// leaves it 0. Drives the client's transfer bar and its speed readout for
    /// the window where the install has not started writing yet.
    #[serde(default)]
    pub transfer_bytes: u64,
    /// `pkg-host` responses answered for this session. 0 means the console
    /// never asked for a byte — which separates "Sony refused the request
    /// before fetching" from a failure after the transfer began.
    #[serde(default)]
    pub served_requests: u64,
    /// The DPI daemon's answer for a Stream session, replayed here so a caller
    /// that stopped waiting (client timeout, proxy, browser navigation) still
    /// learns the verdict. `None` until the daemon replies.
    #[serde(default)]
    pub dpi_ok: Option<bool>,
    #[serde(default)]
    pub dpi_rc: Option<i32>,
}

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

/// Seconds of zero disk progress before a *not-yet-writing* install (no bytes
/// consumed at all) is declared stalled — gives Sony time to even begin.
///
/// Generous (10 min) because on newer firmware (FW 12.x, hardware-observed) an
/// install registers and then the PS5 downloads/extracts the content in the
/// BACKGROUND — it can sit queued for minutes before it begins writing where we
/// can see it (free-space drop / title-dir growth). Giving up at the old 120s
/// false-stalled a perfectly good install while the console's own "Downloading…"
/// tile was still ticking. This only delays a *stall* verdict: completion is
/// still driven by the on-disk launch check (`registered`), never a timer, so a
/// longer wait can't manufacture a false success. Env-tunable.
const INSTALL_STALL_STARTUP_SEC_DEFAULT: u64 = 600;
/// Seconds of zero progress before a *mid-install* flatline (some bytes landed,
/// but well short of the target) is declared stalled — abnormal, so stricter.
const INSTALL_STALL_MID_SEC_DEFAULT: u64 = 240;
/// Seconds of zero progress tolerated once *near done* — the final commit /
/// registration phase writes almost nothing, so we wait patiently for the
/// title to register rather than crying stall.
const INSTALL_STALL_NEARDONE_SEC_DEFAULT: u64 = 600;
/// consumed/expected past which we consider the install "near done" and switch
/// to the patient deadline.
const INSTALL_NEARDONE_FRACTION: f64 = 0.90;
/// On firmware where launchability can't be verified (no derivable title_id,
/// both app.db and the /user/app scan unreadable), byte-accounting IS the
/// completion signal: this fraction of expected bytes consumed …
///
/// 0.99 (not 0.97) because `consumed` is monotonic — a transient free-space
/// drop from another process inflates it permanently, so a lower threshold
/// risks declaring Complete on a 97%-installed pkg after a noisy measurement.
const INSTALL_SETTLE_FRACTION: f64 = 0.99;
/// … AND this many seconds of no further writes ⇒ treat as complete.
const INSTALL_SETTLE_SEC_DEFAULT: u64 = 60;

/// For synthetic-DONE installs (shellui-rpc and appinst-local tiers),
/// the payload reports Done the instant Sony accepts the task — long
/// before the title is actually written. The tracker then verifies
/// completion via the on-disk launch check. But on some firmware/IO
/// combinations, `verify_launchable` can't see the title even after
/// the PS5 notification says "ready" (extended storage installs,
/// stale mount views, sqlite unreadable + FS probe race). Rather than
/// spin until the stall deadline (10+ minutes of "installing" after
/// the user already sees the game on their XMB), stop polling after this
/// grace period as AcceptedUnverified. This NEVER means Complete and NEVER
/// permits deleting the staged pkg. Env-tunable.
const INSTALL_SYNTHETIC_DONE_GRACE_SEC_DEFAULT: u64 = 180;

fn install_synthetic_done_grace_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_SYNTHETIC_DONE_GRACE_SEC",
        INSTALL_SYNTHETIC_DONE_GRACE_SEC_DEFAULT,
    )
}

/// Whether this task_id indicates a synthetic-DONE tier — one where
/// the payload reports Done immediately (shellui-rpc, appinst-local)
/// rather than real-polling Sony's install status.
fn is_synthetic_done_tier(task_id: Option<i32>) -> bool {
    match task_id {
        Some(tid) if tid >= 0 => {
            (tid & APPINST_VIA_SHELLUI_FLAG) != 0
                || (tid & APPINST_VIA_LOCAL_FLAG) != 0
                || (tid & APPINST_VIA_TIER0_FLAG) != 0
        }
        _ => false,
    }
}

fn env_sec_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default)
}
fn install_stall_startup_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_STALL_STARTUP_SEC",
        INSTALL_STALL_STARTUP_SEC_DEFAULT,
    )
}
fn install_stall_mid_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_STALL_MID_SEC",
        INSTALL_STALL_MID_SEC_DEFAULT,
    )
}
fn install_stall_neardone_sec() -> u64 {
    env_sec_or(
        "PS5UPLOAD_INSTALL_STALL_NEARDONE_SEC",
        INSTALL_STALL_NEARDONE_SEC_DEFAULT,
    )
}
fn install_settle_sec() -> u64 {
    env_sec_or("PS5UPLOAD_INSTALL_SETTLE_SEC", INSTALL_SETTLE_SEC_DEFAULT)
}

/// Launchability check, normalized for the tracker:
/// `Some(true)` = Registered (title on disk, definitively done),
/// `Some(false)` = Absent (reachable, title not yet there),
/// `None` = Unsupported (can't verify on this firmware).
type RegisteredObs = Option<bool>;

/// One poll's worth of observations, fed to the pure [`install_verdict`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct TrackerObs {
    registered: RegisteredObs,
    /// Legacy installs without an exact package fingerprint may fall back to
    /// byte-settle on firmware where the target path is unreadable. Exact
    /// variant installs must not: free-space movement cannot prove which
    /// same-version patch/DLC landed.
    allow_byte_settle: bool,
    /// Max bytes observed consumed so far (monotonic) — `max(free-space drop,
    /// title-dir size)`. Monotonic so a noisy free-space blip up can't look
    /// like a regression.
    consumed: u64,
    /// Expected install size (the pkg size we uploaded). 0 ⇒ unknown.
    expected: u64,
    /// Seconds since `consumed` last increased (the stall clock).
    idle_sec: u64,
    /// Tuning, pulled from env once by the caller so the function stays pure.
    startup_sec: u64,
    mid_sec: u64,
    neardone_sec: u64,
    settle_sec: u64,
    /// For synthetic-DONE tiers (shellui-rpc / appinst-local), `Some(grace_sec)`
    /// — after this many seconds of idle since the payload first reported Done,
    /// stop polling as AcceptedUnverified. `None` for real-polled installs.
    synthetic_done_grace_sec: Option<u64>,
}

/// What the tracker decides on a single poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallVerdict {
    /// Confirmed complete — the ONLY state in which the staging pkg may be
    /// deleted. (`registered == Some(true)`, or unverifiable-FW byte-settle.)
    Complete,
    /// Sony accepted a fire-and-forget tier but host-side signals could not
    /// prove completion. Stop polling and KEEP the staged pkg.
    AcceptedUnverified,
    /// Still progressing, or in the final commit window — keep polling, KEEP
    /// the staging pkg (the install may still be reading from it).
    Installing,
    /// No disk progress past the adaptive deadline — terminal, but KEEP the
    /// staging pkg so the user can retry.
    Stalled,
}

/// Phase to report while an install is still in flight — i.e. the tracker has
/// ruled out done, error and stall, and the install is simply not finished.
///
/// A staged install reports `install` throughout, because BGFT's own phase
/// already says `download` while it pulls and there is nothing to add.
///
/// A Stream (serve-only) session has no BGFT task to ask, so the transfer's own
/// counters are the only evidence of where it is:
///
///   * no `pkg-host` request yet → `queued`. Sony has not started; a stall here
///     is "the console never asked", which is a different problem from a slow
///     transfer and must not look like one.
///   * bytes still arriving     → `download`. This is the window a Stream
///     install spends most of its wall clock in, and the one the UI needs to
///     label "streaming" and drive a progress bar from.
///   * everything fetched       → `install`. Sony is writing.
///
/// The transition is driven by `transfer_bytes`, not `bytes_served`: the summed
/// byte count overshoots the package when Sony re-fetches a range (measured
/// 1.53 GB over a 1.35 GB package), so it can read as complete well before it is.
fn in_flight_phase(
    is_serve_only: bool,
    requests_served: u64,
    transfer_bytes: u64,
    total: u64,
) -> InstallPhase {
    if !is_serve_only {
        return InstallPhase::Install;
    }
    if requests_served == 0 {
        InstallPhase::Queued
    } else if total > 0 && transfer_bytes < total {
        InstallPhase::Download
    } else {
        InstallPhase::Install
    }
}

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

/// `SCE_APP_INSTALLER_ERROR_ALREADY_RUNNING`-class "another install is in
/// flight, ask again" rejection. The client retries these (it waits for the
/// console to go ready, up to `DPI_MAX_ATTEMPTS`); the engine must not turn one
/// into a terminal failure when it replays the DPI verdict from the session.
const DPI_TRANSIENT_BUSY_RC: i32 = 0x8002_0002u32 as i32;

/// What Sony's PlayGo/BGFT log says about one content id. Stream (serve-only)
/// installs have no BGFT task_id we can poll, so this is the only way to see a
/// post-transfer refusal such as 0x80A3000D instead of spinning until stall.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SonyLogVerdict {
    Installed,
    Refused { err_code: u32, detail: String },
}

/// Latest terminal PlayGo/BGFT line for `content_id` in a syslog snapshot.
///
/// The verdict lines name a *request number*, not the content id. Only the
/// `[RequestInstall] begin` line carries the id, so that attempt is found
/// first and its own lines are read after. Every line is re-scanned: the
/// console's log rotates, and a marker kept by position can fall off between
/// polls. Mirrors `scripts/ps5-install-watch.py`.
fn sony_log_verdict(text: &str, content_id: &str) -> Option<SonyLogVerdict> {
    if content_id.is_empty() {
        return None;
    }
    let title_id =
        ps5upload_core::pkg_install::title_id_from_content_id(content_id).unwrap_or_default();
    let mut request: Option<String> = None;
    for line in text.lines() {
        let Some(rest) = line.split("[RequestInstall] begin (#").nth(1) else {
            continue;
        };
        let Some((num, rest)) = rest.split_once(", ") else {
            continue;
        };
        let cid = rest
            .trim()
            .trim_end_matches(')')
            .trim_end_matches(',')
            .trim();
        if cid == content_id {
            request = Some(num.trim().to_string());
        }
    }
    let mut latest: Option<SonyLogVerdict> = None;
    if let Some(req) = request.as_deref() {
        let mine = format!("[Request #{req}]");
        for line in text.lines() {
            if !line.contains(&mine) {
                continue;
            }
            if let Some(v) = parse_request_ended_line(line) {
                latest = Some(v);
            } else if let Some(v) = parse_transfer_ended_line(line) {
                latest = Some(v);
            }
        }
    }
    for line in text.lines() {
        if !line.contains(content_id) && (title_id.is_empty() || !line.contains(&title_id)) {
            continue;
        }
        if let Some(v) = parse_bgft_task_ended_line(line) {
            latest = Some(v);
        }
        if let Some(v) = parse_playgo_progress_error_line(line) {
            latest = Some(v);
        }
    }
    latest
}

fn parse_hex_err(s: &str) -> Option<u32> {
    u32::from_str_radix(
        s.trim().trim_start_matches("0x").trim_start_matches("0X"),
        16,
    )
    .ok()
}

/// `[PlayGoCore][Request #2] request ended (state = 9, error = 0x80a3000d, …)`
fn parse_request_ended_line(line: &str) -> Option<SonyLogVerdict> {
    let rest = line.split("request ended (").nth(1)?;
    let state = rest.split("state = ").nth(1)?.split(',').next()?.trim();
    let err_hex = rest
        .split("error = ")
        .nth(1)?
        .split([',', ')'])
        .next()?
        .trim();
    let err_code = parse_hex_err(err_hex)?;
    if state == "7" && err_code == 0 {
        Some(SonyLogVerdict::Installed)
    } else {
        Some(SonyLogVerdict::Refused {
            err_code,
            detail: format!("request ended state={state} error=0x{err_code:08x}"),
        })
    }
}

/// `[PlayGoCore][Request #2] transfer ended (0x80b22416)` — ignore 0x0.
fn parse_transfer_ended_line(line: &str) -> Option<SonyLogVerdict> {
    let rest = line.split("transfer ended (").nth(1)?;
    let hex = rest.split(')').next()?.trim();
    let err_code = parse_hex_err(hex)?;
    if err_code == 0 {
        None
    } else {
        Some(SonyLogVerdict::Refused {
            err_code,
            detail: format!("transfer ended 0x{err_code:08x}"),
        })
    }
}

/// `Task 2000002f : …ended (state=0,runstate=2,error=0x80a3000d)`
fn parse_bgft_task_ended_line(line: &str) -> Option<SonyLogVerdict> {
    let rest = line.split("ended (state=").nth(1)?;
    let state = rest.split(',').next()?.trim();
    let run = rest.split("runstate=").nth(1)?.split(',').next()?.trim();
    let err_hex = rest.split("error=").nth(1)?.split(')').next()?.trim();
    let err_code = parse_hex_err(err_hex)?;
    if state == "3" || (err_code == 0 && run == "4") {
        Some(SonyLogVerdict::Installed)
    } else {
        Some(SonyLogVerdict::Refused {
            err_code,
            detail: format!("task ended state={state} runstate={run} error=0x{err_code:08x}"),
        })
    }
}

/// `Task 2000002f : playgo.progress.state=9, progress.error_code=0x80a3000d`
fn parse_playgo_progress_error_line(line: &str) -> Option<SonyLogVerdict> {
    let rest = line.split("progress.error_code=").nth(1)?;
    let err_code = parse_hex_err(rest.split(',').next()?)?;
    if err_code == 0 {
        None
    } else {
        Some(SonyLogVerdict::Refused {
            err_code,
            detail: format!("playgo progress error=0x{err_code:08x}"),
        })
    }
}

/// Pure completion/stall decision — no I/O, fully unit-testable. This is the
/// brain of the tracker; the handler only feeds it observations and acts on
/// the verdict.
/// The phase each verdict reports. Kept as one exhaustive mapping so a verdict
/// cannot leave the phase stale: the arms of the tracker used to set the phase
/// individually, and the one that forgot (Complete) silently reported a
/// finished install as still installing for as long as it was polled.
fn verdict_phase(
    verdict: InstallVerdict,
    is_serve_only: bool,
    requests_served: u64,
    transfer_bytes: u64,
    total: u64,
) -> InstallPhase {
    match verdict {
        InstallVerdict::Complete | InstallVerdict::AcceptedUnverified => InstallPhase::Done,
        InstallVerdict::Stalled => InstallPhase::Error,
        InstallVerdict::Installing => {
            in_flight_phase(is_serve_only, requests_served, transfer_bytes, total)
        }
    }
}

fn install_verdict(obs: &TrackerObs) -> InstallVerdict {
    // Authoritative: the title's app.pkg landed on disk. Always wins, instantly
    // — independent of the byte math, which can only ever be an estimate.
    if obs.registered == Some(true) {
        return InstallVerdict::Complete;
    }
    let fraction = if obs.expected > 0 {
        obs.consumed as f64 / obs.expected as f64
    } else {
        0.0
    };
    // Byte-accounting completion — the authoritative fallback when the on-disk
    // launch check did NOT confirm `Registered`. This covers two real cases:
    //   • Unverifiable firmware (registered == None): app.db + /user/app both
    //     unreadable, byte-accounting is all we have.
    //   • EXTENDED-STORAGE installs (registered == Some(false)/Absent): the
    //     title's app.pkg lands on `/mnt/ext*`, which the payload's FS_LIST_DIR
    //     sees only through a stale/namespaced mount — so the filesystem check
    //     false-reports Absent for a perfectly good install (HW-confirmed: a
    //     game that plays, with app.pkg on /mnt/ext1, probes as Absent).
    // In BOTH, "essentially all expected bytes landed AND writing settled" means
    // the content really copied — which also cleanly rejects a "dead tile" (it
    // registers appmeta but writes ~no content, so it never settles near 100%).
    if obs.allow_byte_settle
        && obs.registered != Some(true)
        && fraction >= INSTALL_SETTLE_FRACTION
        && obs.idle_sec >= obs.settle_sec
    {
        return InstallVerdict::Complete;
    }
    // Synthetic-DONE grace: these tiers only prove Sony accepted the request.
    // After the grace, end polling as AcceptedUnverified so the UI unblocks,
    // but never manufacture Complete or delete the source package. The
    // byte-settle/registration checks above remain the only success signals.
    //
    // We accept the grace on BOTH `Some(false)` (verified Absent — common on
    // extended-storage installs where the mount is namespaced away from our
    // FS_LIST_DIR view) AND `None` (unverifiable firmware — common on newer
    // FW like 12.xx where app.db is unreadable). Previously only `Some(false)`
    // was accepted, which caused installs on unverifiable FW to stall forever
    // (issue #230 — "stuck on installing" on macOS/FW 12.20).
    //
    // We still require the grace period so a genuine install failure (Sony
    // returns error shortly after accept) is caught by the stall deadline.
    if let Some(grace) = obs.synthetic_done_grace_sec {
        if obs.registered != Some(true) && obs.idle_sec >= grace {
            return InstallVerdict::AcceptedUnverified;
        }
    }
    // Adaptive stall deadline by how far along we are. Near the end the install
    // writes little (commit/register), so we're patient; an early/mid flatline
    // is abnormal, so we're stricter; before any byte lands we allow startup.
    let deadline = if fraction >= INSTALL_NEARDONE_FRACTION {
        obs.neardone_sec
    } else if obs.consumed == 0 {
        obs.startup_sec
    } else {
        obs.mid_sec
    };
    if obs.idle_sec >= deadline {
        InstallVerdict::Stalled
    } else {
        InstallVerdict::Installing
    }
}

/// Observe how many bytes the install has consumed so far, from the two
/// physical signals. Returns the larger of (free-space drop on the data
/// volume since the baseline) and (sum of the title dir's file sizes). Both
/// are best-effort: an unreadable signal contributes 0, not an error — the
/// tracker degrades to whichever signal is available.
fn observe_consumed(addr: &str, title_id: &str, baseline_free: Option<u64>) -> u64 {
    // Signal A — global free-space drop on the volume hosting /user/app. Noisy
    // (other writes move it) but available from the first poll, before Sony
    // even creates the title dir.
    let free_drop = match (baseline_free, current_free_bytes(addr)) {
        (Some(base), Some(now)) => base.saturating_sub(now),
        _ => 0,
    };
    // Signal B — the title dir's own size. Clean (only THIS install writes
    // there) but only exists once Sony creates /user/app/<title_id>/.
    let dir_size = title_dir_size(addr, title_id);
    free_drop.max(dir_size)
}

/// Free bytes across the possible install-target volumes: the volume hosting
/// `/user/app` (internal) PLUS every extended-storage drive (`/mnt/ext*`). A
/// title installs to whichever the console's storage setting selects, so we sum
/// them — the free-space drop then shows up wherever the content actually lands
/// (HW-confirmed: a Pro installs to `/mnt/ext1`, where an internal-only baseline
/// would never move and the tracker would false-stall a large install).
/// `None` if volumes can't be listed.
fn current_free_bytes(addr: &str) -> Option<u64> {
    let vols = ps5upload_core::volumes::list_volumes(addr).ok()?;
    let mut total = 0u64;
    let mut found = false;
    if let Some(v) = vols
        .find_for_path("/user/app")
        .or_else(|| vols.find_for_path("/user"))
    {
        total = total.saturating_add(v.free_bytes);
        found = true;
    }
    for v in &vols.volumes {
        if v.path.starts_with("/mnt/ext") {
            total = total.saturating_add(v.free_bytes);
            found = true;
        }
    }
    if found {
        Some(total)
    } else {
        None
    }
}

/// Sum of immediate file sizes under `…/user/app/<title_id>/` (the dominant
/// being `app.pkg`), across internal `/user/app` AND every extended-storage
/// mount (`/mnt/ext*/user/app`) — the install lands on whichever drive the
/// console targets. 0 if the dir doesn't exist yet or can't be read.
fn title_dir_size(addr: &str, title_id: &str) -> u64 {
    if title_id.is_empty() {
        return 0;
    }
    let mut dirs = vec![format!("/user/app/{title_id}")];
    if let Ok(vols) = ps5upload_core::volumes::list_volumes(addr) {
        for v in &vols.volumes {
            if v.path.starts_with("/mnt/ext") {
                dirs.push(format!("{}/user/app/{title_id}", v.path));
            }
        }
    }
    let mut total = 0u64;
    for dir in dirs {
        if let Ok(listing) = ps5upload_core::fs_ops::list_dir(
            addr,
            &dir,
            ps5upload_core::fs_ops::ListDirOptions::default(),
        ) {
            total = total.saturating_add(
                listing
                    .entries
                    .iter()
                    .filter(|e| e.kind == "file")
                    .map(|e| e.size)
                    .sum(),
            );
        }
    }
    total
}

/// Drop sessions older than the configured GC threshold. Called as a
/// best-effort sweep at the start of every status handler invocation.
/// Cheap (linear in active session count, which is bounded by the
/// queue UI to <100 in practice).
fn gc_old_sessions(state: &PkgInstallStateHandle) {
    let now = now_unix();
    let max_age = pkg_session_max_age_sec();
    let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    sessions.retain(|_, s| {
        // Always keep sessions younger than the GC threshold; drop
        // older ones regardless of state. A session that's still
        // actively polling but past the threshold is treated as
        // orphaned (queue UI's worker loop terminates at done/error/
        // cancelled or after pollErrors >= 5, so it shouldn't be
        // legitimately polling a 2h-old session anyway).
        now.saturating_sub(s.last_activity_unix.max(s.created_at_unix)) < max_age
    });
}

async fn install_status_handler(
    State(state): State<PkgInstallStateHandle>,
    Query(q): Query<StatusQuery>,
) -> Response<Body> {
    // A client that is still watching counts as activity too, so a session
    // whose console has gone quiet mid-install is kept while anyone is looking.
    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = sessions.get_mut(&q.session) {
            s.last_activity_unix = now_unix();
        }
    }
    gc_old_sessions(&state);
    let (
        ps5_addr,
        task_id,
        total,
        cancelled,
        terminal,
        content_id,
        package_type,
        package_fingerprint,
        cached_launchable,
        cached_consumed,
        cached_stalled,
        cached_accepted_unverified,
        register_err_code,
        is_serve_only,
        requests_served,
        transfer_bytes,
        dpi_ok,
        dpi_rc,
        dpi_detail,
    ) = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get(&q.session) {
            None => {
                return json_err(
                    StatusCode::NOT_FOUND,
                    &format!("no install session {}", q.session),
                )
            }
            Some(s) => (
                s.ps5_mgmt_addr.clone(),
                s.task_id,
                s.total_size,
                s.cancelled,
                s.terminal_status.clone(),
                s.content_id.clone(),
                s.package_type.clone(),
                s.package_fingerprint.clone(),
                s.launchable,
                s.progress_consumed_bytes,
                s.stalled,
                s.accepted_unverified,
                s.err_code,
                s.serve_only,
                s.requests_served,
                s.transfer_bytes,
                s.dpi_ok,
                s.dpi_rc,
                s.dpi_detail.clone(),
            ),
        }
    };
    // Once the install has finished, replay the cached terminal status and
    // do NOT re-poll: the payload may have reaped the BGFT task_id, so a
    // fresh PKG_INSTALL_STATUS would fail and turn a succeeded install into
    // a spurious 502 on the client's next poll.
    if let Some(status) = terminal {
        // For terminal responses we may not have a task_id (cancelled
        // before BGFT register, or a Tier-3 reject). Pass 0 — via_tier()
        // returns "direct-bgft" for 0, which is the most accurate
        // fallback (no synthetic flags = whatever Sony BGFT returned
        // raw, including "never got one").
        let tid = task_id.unwrap_or(0);
        return json_ok(&build_status_response(
            q.session,
            status,
            total,
            cancelled,
            tid,
            cached_launchable,
            cached_consumed,
            cached_stalled,
            cached_accepted_unverified,
            TransferView {
                bytes: transfer_bytes,
                requests: requests_served,
                dpi_ok,
                dpi_rc,
            },
        ));
    }
    // Off the reactor: this handler is polled ~1/s per active install, and the
    // blocking STATUS frame exchange against a slow/wedged console would
    // otherwise park a worker thread per poll — with several installs that
    // starves the whole engine. (See install_start_handler.)
    //
    // A serve-only (Stream beta) session has NO BGFT task_id — the in-process
    // installer never ran; the DPI daemon did, in its own process. We can't
    // query BGFT phase, but completion is verifiable the SAME way the normal
    // path verifies a Done: the on-disk launch-check (`verify_launchable`) plus
    // byte observation, both filesystem-based and task_id-free. Synthesize a
    // Done phase so the progress-driven tracker below runs (Registered ⇒
    // complete, Absent ⇒ still installing, flatline ⇒ stall). This replaces the
    // old CONFLICT that made the client's stream-install verify a silent no-op
    // (and thus couldn't catch a FW-11+ hollow tile).
    let mut status: PkgInstallStatus = match task_id {
        Some(task_id) => {
            let addr = ps5_addr.clone();
            match tokio::task::spawn_blocking(move || pkg_install_status(&addr, task_id)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    return json_err(
                        StatusCode::BAD_GATEWAY,
                        &format!("payload PKG_INSTALL_STATUS failed: {e}"),
                    )
                }
                Err(e) => {
                    return json_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("status task panicked/cancelled: {e}"),
                    )
                }
            }
        }
        None => PkgInstallStatus {
            // The tracker below owns the terminal answer; this is only the
            // phase to report while nothing is finished yet.
            phase: in_flight_phase(is_serve_only, requests_served, transfer_bytes, total),
            downloaded: 0,
            total,
            // Carry the REGISTER rejection code instead of hardcoding 0.
            //
            // This arm exists for serve-only (stream) sessions, which
            // legitimately have no BGFT task_id — but a register-REJECT has no
            // task_id either, so it landed here too and had its rejection
            // silently rewritten to "err_code: 0, phase: Done". A staged
            // install Sony refused (0x80B2116F — PlayGo INVALID_SLOT,
            // cause not established) therefore surfaced to the
            // client as a clean success with no hint that anything had been
            // declined, and no pointer to the Debug Settings workaround that
            // message carries. The progress tracker below still
            // decides completion; this only stops the reason being erased.
            err_code: register_err_code,
            detail: String::new(),
            register_path: String::new(),
            intdebug_avail: false,
            kernel_rw: false,
            shellui_err: None,
            appinst_err: None,
        },
    };

    // ── Register-reject fast-fail ───────────────────────────────────────
    // A STAGED install whose register was refused has no BGFT task and no
    // in-process install running: nothing will ever write a byte. Before this,
    // such a session synthesized a Done, handed it to the progress tracker,
    // and the tracker dutifully watched a flatline for ~10 MINUTES before
    // calling it a stall — with the actual rejection code discarded, so the
    // user never saw the reason or its documented workaround.
    //
    // Measured on both consoles: FW 5.10 refused with 0x80B2150F and FW 9.60
    // with 0x80B2116F, each after a 600s spinner, while the same package
    // stream-installed fine seconds later.
    //
    // Serve-only sessions are EXEMPT: they legitimately have no task_id
    // because the DPI daemon does the install, and a non-zero register rc
    // there is not fatal.
    if !is_serve_only && task_id.is_none() && register_err_code != 0 {
        status.phase = InstallPhase::Error;
        status.err_code = register_err_code;
        if status.detail.is_empty() {
            status.detail = ps5upload_core::pkg_install::err_code_message(register_err_code)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "PS5 refused the install request (0x{register_err_code:08X}); the staged package was kept so you can retry."
                    )
                });
        }
    }

    // Stream (serve-only) has no BGFT task_id. Sony still writes the real
    // outcome to the console log after the HTTP transfer — including
    // post-transfer refusals such as 0x80A3000D (firmware too old) that used
    // to leave this handler spinning for the 600s stall. Require at least one
    // pkg-host fetch so a retry cannot inherit the previous attempt's line.
    if is_serve_only
        && requests_served > 0
        && !matches!(status.phase, InstallPhase::Error)
        && !cancelled
    {
        let addr = ps5_addr.clone();
        let cid = content_id.clone();
        let log_verdict =
            tokio::task::spawn_blocking(move || match ps5upload_core::hw::syslog_tail(&addr) {
                Ok(text) => sony_log_verdict(&text, &cid),
                Err(_) => None,
            })
            .await
            .ok()
            .flatten();
        if let Some(SonyLogVerdict::Refused { err_code, detail }) = log_verdict {
            crate::log_warn!(
                "stream install refused by console: session={} content_id={} {}",
                q.session,
                content_id,
                detail
            );
            status.phase = InstallPhase::Error;
            status.err_code = err_code;
            status.detail = ps5upload_core::pkg_install::err_code_message(err_code)
                .map(|s| format!("{s} ({detail})"))
                .unwrap_or(detail);
            // A refused stream often still writes an app.db row with no
            // app.pkg (a hollow tile). That blocks a later staged retry via
            // the destructive-reinstall guard. Remove it when nothing landed.
            if let Some(tid) = ps5upload_core::pkg_install::title_id_from_content_id(&content_id) {
                let addr = ps5_addr.clone();
                tokio::task::spawn_blocking(move || {
                    let inv = installed_pkg_inventory(&addr, &tid);
                    if inv.artifacts.is_empty() {
                        match ps5upload_core::fs_ops::app_unregister(&addr, &tid) {
                            Ok(_) => {
                                crate::log_info!("cleared hollow tile after refused stream: {tid}")
                            }
                            Err(e) => crate::log_warn!("could not clear hollow tile {tid}: {e}"),
                        }
                    }
                });
            }
        }
    }

    // The DPI daemon's answer, replayed from the session. A Stream caller that
    // stopped waiting — an HTTP client timeout, a reverse proxy's 60 s default,
    // a browser navigation — never saw the reply to `/api/pkg/dpi-direct-install`,
    // and used to leave the session reporting "installing" for the full 600 s
    // startup stall even though the daemon had already refused. Re-reading it
    // here makes the hand-off's outcome independent of who is still listening.
    if is_serve_only && !cancelled && !matches!(status.phase, InstallPhase::Error) {
        if let Some(rc) = dpi_rc {
            if dpi_ok == Some(false) && rc != DPI_TRANSIENT_BUSY_RC {
                crate::log_warn!(
                    "stream install refused by DPI daemon: session={} rc=0x{:08x} detail={}",
                    q.session,
                    rc as u32,
                    dpi_detail
                );
                status.phase = InstallPhase::Error;
                status.err_code = rc as u32;
                status.detail = if dpi_detail.is_empty() {
                    err_code_message(rc as u32)
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            format!("the PS5 installer refused (0x{:08X})", rc as u32)
                        })
                } else {
                    dpi_detail
                };
            }
        }
    }

    // (`total` from the session is the fallback for build_status_response,
    // which prefers the BGFT-reported size when non-zero — BGFT reports 0
    // before the download starts.)

    // ── progress-driven completion tracking ────────────────────────────
    // The payload reports a *synthetic* Done the instant Sony *accepts* the
    // task (shellui-rpc / appinst-local) — long before a large title is
    // actually written. So on Done we don't trust the timer; we OBSERVE the
    // install to completion: poll the on-disk launch check (authoritative
    // "done") AND the bytes landing (free-space drop / title-dir growth), and
    // only declare the install *complete* — the sole state that lets the
    // staging cleanup below delete the uploaded pkg — when the title genuinely
    // registered (or, on unverifiable FW, when ~all expected bytes settled).
    // A flatline past the adaptive deadline is a *stall*: terminal, but the
    // pkg is KEPT so the user can retry. See `install_verdict`.
    let mut launchable: Option<bool> = None;
    // True only on confirmed completion — gates staging cleanup. Stalls,
    // errors, and accepted-but-unverified outcomes are terminal but not safe
    // to delete.
    let mut terminal_complete = false;
    let mut stalled = false;
    let mut accepted_unverified = false;
    // A serve-only session has no BGFT task to report a phase, so this tracker
    // is the only thing that can say "done" for it — run it whenever the
    // session is still live, not just on a synthesized Done. A staged session
    // reaches here only when its payload reported Done (or, for a register
    // reject with no task, when it has already failed and is excluded above).
    let track = matches!(status.phase, InstallPhase::Done)
        || (is_serve_only && !matches!(status.phase, InstallPhase::Error) && !cancelled);
    if track {
        // A finished install changes which artwork exists on the console.
        // Drop the cached images for it so the new title's cover appears
        // immediately instead of waiting out the cache's TTL.
        crate::icon_cache::invalidate_console(&ps5_addr);
        let addr = ps5_addr.clone();
        let cid = content_id.clone();
        let pt = package_type.clone();
        let fp = package_fingerprint.clone();
        let check = tokio::task::spawn_blocking(move || {
            verify_installed_artifact(&addr, &cid, &pt, total, &fp)
        })
        .await
        .unwrap_or(InstalledArtifactCheck::Unsupported);

        // Normalize to the tracker's registered-observation.
        let registered: RegisteredObs = match check {
            InstalledArtifactCheck::Match => Some(true),
            InstalledArtifactCheck::Absent | InstalledArtifactCheck::Different => Some(false),
            InstalledArtifactCheck::Unsupported => None,
        };

        // Observe bytes consumed this poll (off-reactor: two blocking FS
        // frames). Best-effort — an unreadable signal contributes 0.
        let title_id =
            ps5upload_core::pkg_install::title_id_from_content_id(&content_id).unwrap_or_default();
        let baseline = {
            let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .get(&q.session)
                .and_then(|s| s.install_start_free_bytes)
        };
        let (consumed_now, baseline_free) = {
            let addr = ps5_addr.clone();
            let tid = title_id.clone();
            tokio::task::spawn_blocking(move || {
                // Capture the free-space baseline on the first poll, then
                // measure drop against it on every subsequent poll.
                let base = baseline.or_else(|| current_free_bytes(&addr));
                let consumed = observe_consumed(&addr, &tid, base);
                (consumed, base)
            })
            .await
            .unwrap_or((0, baseline))
        };

        let now = now_unix();
        // Update the session's monotonic progress + stall clock, and read back
        // the values the verdict needs.
        let (consumed, idle_sec) = {
            let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            match sessions.get_mut(&q.session) {
                Some(s) => {
                    if s.install_start_free_bytes.is_none() {
                        s.install_start_free_bytes = baseline_free;
                    }
                    // Monotonic: a noisy free-space blip up can't look like a
                    // regression and falsely advance/reset anything.
                    if consumed_now > s.progress_consumed_bytes {
                        s.progress_consumed_bytes = consumed_now;
                        s.last_progress_unix = Some(now);
                    }
                    let started = *s.last_progress_unix.get_or_insert(now);
                    (s.progress_consumed_bytes, now.saturating_sub(started))
                }
                None => (consumed_now, 0),
            }
        };

        let obs = TrackerObs {
            registered,
            allow_byte_settle: package_fingerprint.is_empty(),
            consumed,
            expected: total,
            idle_sec,
            startup_sec: install_stall_startup_sec(),
            mid_sec: install_stall_mid_sec(),
            neardone_sec: install_stall_neardone_sec(),
            settle_sec: install_settle_sec(),
            synthetic_done_grace_sec: {
                let tid = {
                    let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
                    sessions.get(&q.session).and_then(|s| s.task_id)
                };
                if is_synthetic_done_tier(tid) {
                    Some(install_synthetic_done_grace_sec())
                } else {
                    None
                }
            },
        };
        let verdict = install_verdict(&obs);
        // One mapping decides the phase for every verdict, so no arm can
        // leave it stale. See `verdict_phase`.
        status.phase = verdict_phase(
            verdict,
            is_serve_only,
            requests_served,
            transfer_bytes,
            total,
        );
        match verdict {
            InstallVerdict::Complete => {
                // Confirmed done — the phase is already `Done`; see
                // verdict_phase, the single place a verdict becomes a phase. `Some(true)` when the title registered;
                // `None` on unverifiable FW that settled by byte-accounting
                // (fall back to the may_not_launch heuristic, as before).
                let via = if registered == Some(true) {
                    "registered"
                } else {
                    "byte-settle"
                };
                launchable = if registered == Some(true) {
                    Some(true)
                } else {
                    None
                };
                terminal_complete = true;
                // Always log completion (even when auto-delete is off and the
                // "staging cleaned" line won't fire) so a bug bundle shows the
                // install ran to genuine completion, by which signal, and how
                // many bytes it took — the timeline the old code never recorded.
                crate::log_info!(
                    "install complete: session={} content_id={} via={} consumed={} expected={} idle_sec={}",
                    q.session,
                    content_id,
                    via,
                    consumed,
                    total,
                    idle_sec
                );
            }
            InstallVerdict::AcceptedUnverified => {
                // Synthetic DONE proves only that Sony accepted the request.
                // End the spinner but keep the source package and avoid a
                // green success until registration or byte-settle proves it.
                accepted_unverified = true;
                launchable = None;
                status.detail =
                    "PS5 accepted the install, but completion could not be verified; staged package kept"
                        .to_string();
                crate::log_warn!(
                    "install accepted but unverified (pkg KEPT): session={} content_id={} consumed={} expected={} idle_sec={}",
                    q.session,
                    content_id,
                    consumed,
                    total,
                    idle_sec
                );
            }
            InstallVerdict::Installing => {
                // Not terminal. Its phase — `queued` before the console fetches
                // anything, `download` while it pulls, `install` once the
                // transfer is done — comes from verdict_phase, so the UI can
                // name the state instead of showing one static "Installing…"
                // through a minutes-long transfer. Skips the terminal/cleanup
                // blocks, so the next poll re-observes and any staging pkg is
                // left in place (Sony's installer may still be reading it).
            }
            InstallVerdict::Stalled => {
                // No disk progress past the adaptive deadline. Terminal, but
                // NOT complete: report an error AND KEEP the pkg (retry path).
                stalled = true;
                launchable = Some(false);
                if status.detail.is_empty() {
                    status.detail = format!(
                        "install stalled: no disk progress for {}s ({} of {} bytes written)",
                        idle_sec, consumed, total
                    );
                }
                crate::log_warn!(
                    "install stalled (pkg KEPT): session={} content_id={} consumed={} expected={} idle_sec={}",
                    q.session,
                    content_id,
                    consumed,
                    total,
                    idle_sec
                );
            }
        }
    }

    // Tier-1 staging cleanup — delete the uploaded pkg ONLY on confirmed
    // completion (`terminal_complete`). Previously this fired on any terminal
    // phase (Done|Error), which deleted the pkg mid-install for large titles
    // (Sony reads it for the whole install) AND deleted it on a failed install
    // — the reported data-loss. Now a stall / error / not-yet-confirmed Done
    // KEEPS the pkg; only a genuine "the title registered" deletes it. We
    // `take()` the path so we never re-issue a delete on later polls.
    if terminal_complete {
        let path_to_clean = {
            let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .get_mut(&q.session)
                .and_then(|s| s.staging_path.take())
        };
        if let Some(path) = path_to_clean {
            let addr = ps5_addr.clone();
            tokio::task::spawn_blocking(move || {
                // Retry on `fs_delete_failed` — Sony's installer briefly
                // holds the staged pkg open right after terminal_complete
                // on FW 10.40+; see delete_staging_with_retry.
                match delete_staging_with_retry(&addr, &path, "terminal") {
                    Ok(()) => crate::log_info!("staging cleaned: addr={} path={}", addr, path),
                    Err(e) => crate::log_warn!(
                        "staging cleanup failed: addr={} path={} err={}",
                        addr,
                        path,
                        e
                    ),
                }
            });
        }
    }

    // Snapshot the terminal status so later polls replay it instead of
    // re-hitting the (soon-to-be-reaped) BGFT task — see the short-circuit
    // at the top of this handler. Terminal = a confirmed-complete Done, a
    // stall (phase forced to Error above), or a genuine BGFT error. A Done
    // that the tracker downgraded to Install is NOT terminal — keep polling.
    let installed_bytes = if matches!(status.phase, InstallPhase::Done | InstallPhase::Error) {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = sessions.get_mut(&q.session) {
            let first_terminal = !s.notified_console;
            s.terminal_status = Some(status.clone());
            s.launchable = launchable;
            s.stalled = stalled;
            s.accepted_unverified = accepted_unverified;
            if first_terminal {
                s.notified_console = true;
                // Tell the CONSOLE the install finished. Installs are long and
                // usually kicked off from another room; until now the only
                // completion signal lived in the client UI, so whoever was
                // watching the TV had no idea whether it had finished,
                // stalled, or failed. Detached and fire-and-forget: a toast
                // that fails must never change the install's reported outcome.
                let toast_addr = ps5_addr.clone();
                let title_name = if s.title.trim().is_empty() {
                    content_id.clone()
                } else {
                    s.title.clone()
                };
                let done = matches!(status.phase, InstallPhase::Done);
                let err_code = status.err_code;
                let (summary, body) = if done && !accepted_unverified {
                    (
                        "Install complete",
                        format!("{title_name} is ready to play."),
                    )
                } else if done {
                    (
                        "Install finished (unverified)",
                        format!(
                            "{title_name} was accepted, but completion could not be confirmed."
                        ),
                    )
                } else if stalled {
                    (
                        "Install stalled",
                        format!("{title_name} stopped making progress — the package was kept so you can retry."),
                    )
                } else {
                    (
                        "Install failed",
                        format!("{title_name} did not install (error 0x{err_code:08x})."),
                    )
                };
                tokio::task::spawn_blocking(move || {
                    let req = ToastRequest {
                        title: summary.to_string(),
                        subtitle: body,
                        icon: String::new(),
                        action_url: String::new(),
                    };
                    if let Err(e) = toast_send(&toast_addr, &req) {
                        crate::log_warn!("install console toast failed: {e}");
                    }
                });
            }
            s.progress_consumed_bytes
        } else {
            cached_consumed
        }
    } else {
        // Live in-progress poll: surface the running consumed total for the
        // client's live % (installed_bytes / total).
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .get(&q.session)
            .map(|s| s.progress_consumed_bytes)
            .unwrap_or(cached_consumed)
    };

    // Re-read the transfer counters under the lock: they move on every Range
    // response, and the values captured at the top of the handler are already
    // stale by the time the tracker has run. The DPI verdict is kept from the
    // entry read — the fail-fast block above already acted on it.
    let transfer = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .get(&q.session)
            .map(|s| TransferView {
                bytes: s.transfer_bytes,
                requests: s.requests_served,
                dpi_ok,
                dpi_rc,
            })
            .unwrap_or(TransferView {
                bytes: transfer_bytes,
                requests: requests_served,
                dpi_ok,
                dpi_rc,
            })
    };

    json_ok(&build_status_response(
        q.session,
        status,
        total,
        cancelled,
        // Serve-only sessions have no BGFT task_id; 0 makes via_tier() report
        // "direct-bgft", the honest "no synthetic tier flags" fallback.
        task_id.unwrap_or(0),
        launchable,
        installed_bytes,
        stalled,
        accepted_unverified,
        transfer,
    ))
}

/// The transfer-side view of a session. Needed by both the live status path and
/// the cached-terminal replay so a replayed poll renders identically.
#[derive(Debug, Clone, Copy, Default)]
struct TransferView {
    bytes: u64,
    requests: u64,
    dpi_ok: Option<bool>,
    dpi_rc: Option<i32>,
}

/// Build the wire `StatusResponse` from a payload `PkgInstallStatus`.
/// `fallback_total` is our own known size, used when BGFT reports 0
/// (which it does before the download starts). Shared by the live path
/// and the cached-terminal replay so both render identically.
#[allow(clippy::too_many_arguments)] // flat builder for the wire struct's fields
fn build_status_response(
    session_id: String,
    status: PkgInstallStatus,
    fallback_total: u64,
    cancelled: bool,
    task_id: i32,
    launchable: Option<bool>,
    installed_bytes: u64,
    stalled: bool,
    accepted_unverified: bool,
    transfer: TransferView,
) -> StatusResponse {
    let total = if status.total > 0 {
        status.total
    } else {
        fallback_total
    };
    StatusResponse {
        session_id,
        phase: status.phase,
        downloaded: status.downloaded,
        total,
        err_code: status.err_code,
        err_message: err_code_message(status.err_code).map(|s| s.to_string()),
        detail: status.detail,
        cancelled,
        may_not_launch: ps5upload_core::pkg_install::install_may_not_launch(&status.register_path),
        launchable,
        register_path: status.register_path,
        intdebug_avail: status.intdebug_avail,
        kernel_rw: status.kernel_rw,
        shellui_err: status.shellui_err,
        appinst_err: status.appinst_err,
        via: ps5upload_core::pkg_install::via_tier(task_id).to_string(),
        installed_bytes,
        stalled,
        accepted_unverified,
        transfer_bytes: transfer.bytes,
        served_requests: transfer.requests,
        dpi_ok: transfer.dpi_ok,
        dpi_rc: transfer.dpi_rc,
    }
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

// ─── /api/pkg/dpi-ensure + /api/pkg/payload-restore ──────────────────
//
// Loader-port (:9021) delivery of the two ELF images the install cascade
// swaps between. The desktop client owns an identical pair of commands
// (`dpi_ensure` / `payload_send`) backed by its own embedded copies; these
// routes are what a browser-driven, self-hosted engine calls instead.
//
// Order matters to the caller and is the same on both transports:
// `dpi-ensure` may REPLACE the running ps5upload helper (a single-payload
// loader runs one process), so whoever calls it must call
// `payload-restore` afterwards — including on the failure paths, or the
// console is left with no helper and the web UI cannot reach it again.

/// How long to wait for a TCP connect when asking "is :9040 up?".
const DPI_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
/// Poll budget after streaming the daemon: 16 × 500 ms, matching the
/// desktop client so a slow console behaves the same on both transports.
const DPI_BRINGUP_POLLS: u32 = 16;
const DPI_BRINGUP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, Deserialize)]
pub struct LoaderRequest {
    /// Any PS5 address we hold (`ip:9114`, `ip:9113`) or a bare IP.
    pub ps5_addr: String,
}

#[derive(Debug, Serialize)]
pub struct DpiEnsureResponse {
    pub ok: bool,
    /// True when the daemon is answering on :9040 — either it already was
    /// (a scene daemon like etaHEN, or ours from a previous install) or it
    /// came up after we streamed it.
    pub listening: bool,
    /// True when we actually pushed the daemon to the loader. The caller
    /// uses this to know the helper was displaced: `sent: false` means
    /// nothing was replaced and a restore is a no-op.
    pub sent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Machine-readable cause when `ok` is false, so the client can pick the
    /// right guidance instead of pattern-matching English prose. A user on
    /// 5.17.6 was told to rebuild their engine when the real cause was that
    /// their console's ELF loader had stopped answering on :9021 — three very
    /// different problems were sharing one message.
    ///
    ///   * `no_image`           — this engine build carries no DPI daemon.
    ///   * `loader_unreachable` — nothing accepted a connection on :9021.
    ///   * `loader_send_failed` — the loader accepted, the transfer failed.
    ///   * `no_bringup`         — image delivered, :9040 never came up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

use ps5upload_core::payload_lifecycle::{
    dpi_send_failure_reason, DPI_REASON_NO_BRINGUP, DPI_REASON_NO_IMAGE,
};

#[derive(Debug, Serialize)]
pub struct PayloadRestoreResponse {
    pub ok: bool,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

async fn dpi_ensure_handler(Json(req): Json<LoaderRequest>) -> Response<Body> {
    let ps5_ip = strip_host_port(&req.ps5_addr);
    if ps5_ip.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "ps5_addr is required");
    }
    let res = tokio::task::spawn_blocking(move || dpi_ensure_blocking(&ps5_ip)).await;
    match res {
        Ok(resp) => json_ok(&resp),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("dpi-ensure task failed: {e}"),
        ),
    }
}

/// The blocking half of `dpi-ensure`. Split out so the decision sequence
/// — probe, then stream, then wait — reads in one place.
fn dpi_ensure_blocking(ps5_ip: &str) -> DpiEnsureResponse {
    use ps5upload_core::payload_lifecycle as pl;

    let dpi_addr = pl::join_host_port(ps5_ip, pl::DPI_DAEMON_PORT);
    if pl::port_is_open(&dpi_addr, DPI_PROBE_TIMEOUT) {
        crate::log_info!("dpi-ensure: {} already listening", dpi_addr);
        return DpiEnsureResponse {
            ok: true,
            listening: true,
            sent: false,
            error: None,
            reason: None,
        };
    }

    // An etaHEN / elf-arsenal DPI v2 bridge can do the install for us, which
    // means we do NOT have to send our own daemon to the loader — and so we do
    // not replace the user's running main payload at all. Checked after our own
    // daemon only because if that is already up, it costs nothing to use.
    let v2_addr = pl::join_host_port(ps5_ip, DPI_V2_PORT);
    if pl::port_is_open(&v2_addr, DPI_PROBE_TIMEOUT) {
        crate::log_info!(
            "dpi-ensure: DPI v2 bridge listening on {} — no payload swap needed",
            v2_addr
        );
        return DpiEnsureResponse {
            ok: true,
            listening: true,
            sent: false,
            error: None,
            reason: None,
        };
    }

    let bytes = match crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Dpi) {
        Ok(b) => b,
        Err(e) => {
            crate::log_warn!("dpi-ensure: no daemon image available: {}", e);
            return DpiEnsureResponse {
                ok: false,
                listening: false,
                sent: false,
                error: Some(e),
                reason: Some(DPI_REASON_NO_IMAGE),
            };
        }
    };

    crate::log_info!(
        "dpi-ensure: streaming {} bytes of DPI daemon to {}:{}",
        bytes.len(),
        ps5_ip,
        pl::PS5_LOADER_PORT
    );
    if let Err(e) = pl::send_elf_to_loader(
        ps5_ip,
        pl::PS5_LOADER_PORT,
        &bytes,
        pl::LoaderImage::Companion,
    ) {
        crate::log_warn!("dpi-ensure: send failed: {}", e);
        return DpiEnsureResponse {
            ok: false,
            listening: false,
            sent: false,
            reason: Some(dpi_send_failure_reason(&e)),
            error: Some(format!("send dpi.elf: {e}")),
        };
    }

    for _ in 0..DPI_BRINGUP_POLLS {
        std::thread::sleep(DPI_BRINGUP_INTERVAL);
        if pl::port_is_open(&dpi_addr, DPI_PROBE_TIMEOUT) {
            crate::log_info!("dpi-ensure: daemon up on {}", dpi_addr);
            return DpiEnsureResponse {
                ok: true,
                listening: true,
                sent: true,
                error: None,
                reason: None,
            };
        }
    }
    // Sent but never answered. `sent: true` is the important half of this
    // reply: the helper has been displaced, so the caller must restore it.
    crate::log_warn!("dpi-ensure: daemon never came up on {}", dpi_addr);
    DpiEnsureResponse {
        ok: false,
        listening: false,
        sent: true,
        error: Some("DPI daemon did not come up on :9040".to_string()),
        reason: Some(DPI_REASON_NO_BRINGUP),
    }
}

async fn payload_restore_handler(Json(req): Json<LoaderRequest>) -> Response<Body> {
    let ps5_ip = strip_host_port(&req.ps5_addr);
    if ps5_ip.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "ps5_addr is required");
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
            crate::log_info!("payload-restore: sent {} bytes", bytes);
            json_ok(&PayloadRestoreResponse {
                ok: true,
                bytes,
                error: None,
            })
        }
        Ok(Err(e)) => {
            // Not an HTTP error: the caller runs this in a `finally` and a
            // failed restore is information to log, not a reason to mask
            // the install result that preceded it.
            crate::log_warn!("payload-restore: {}", e);
            json_ok(&PayloadRestoreResponse {
                ok: false,
                bytes: 0,
                error: Some(e),
            })
        }
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("payload-restore task failed: {e}"),
        ),
    }
}

// ─── /api/pkg/dpi-install ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DpiInstallRequest {
    /// Any PS5 address we hold (`ip:9114` etc.) or a bare IP — we use
    /// only the host part and talk to the DPI daemon on `:9040`.
    pub ps5_addr: String,
    /// Absolute PS5-side staged `.pkg` path, or an HTTP(S) URL that the PS5
    /// can fetch directly. The latter has no ps5upload staging copy.
    pub local_ps5_path: String,
    /// Title the package belongs to. Supplied by the client, which parsed the
    /// pkg locally; the engine cannot parse a file that lives on the console.
    /// When present with `package_app_ver`, the install is verified afterwards
    /// (see ps5upload_core::patch_verify). Absent = no verification, which is
    /// what an older client sends.
    #[serde(default)]
    pub title_id: Option<String>,
    /// `APP_VER` the package declares, e.g. "01.09".
    #[serde(default)]
    pub package_app_ver: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DpiInstallResponse {
    /// True when the daemon accepted the install (`ok` reply).
    pub ok: bool,
    /// The daemon's `sceAppInstUtilInstallByPackage` return code, or -1
    /// when the daemon never reached the install call (init/recv/badpath).
    pub rc: i32,
    /// True when the daemon could not initialize AppInstUtil. Distinct
    /// from a Sony-side reject: an init failure means the daemon is in
    /// fallback mode and retrying will likely fail the same way until
    /// the underlying IPMI/kstuff issue resolves.
    pub init_failed: bool,
    /// The installer connection ended without a trustworthy Sony result.
    /// This is deliberately distinct from `ok: false`: callers must restore
    /// the main payload and verify the exact installed artifact before showing
    /// a failure. FW 9.60 can close the DPI socket (or report the daemon's
    /// internal -1 sentinel as 0xffffffff) after the package already landed.
    pub ambiguous: bool,
    pub err_message: Option<String>,
    /// pkg-host evidence for Stream installs. Always zero for staged paths.
    pub requests_served: u64,
    pub bytes_served: u64,
    /// Which installer bridge handled this: `"dpiv2"` for an etaHEN /
    /// elf-arsenal bridge that was already listening (no payload swap
    /// happened), or `"ps5upload"` for our own daemon. Absent for staged
    /// installs, which never route through a bridge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<String>,
    /// Post-install version check: "applied", "did_not_apply", "inconclusive",
    /// or absent when the caller supplied no identity to check against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_verdict: Option<String>,
    /// Installed APP_VER before and after, for the UI and for bug reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_ver_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_ver_after: Option<String>,
}

/// One parsed reply from the DPI daemon. The daemon replies in the
/// reference's ok/error form (elf-arsenal payloads-src/dpi/main.c):
///   "ok"                  — InstallByPackage accepted
///   "error:0x%08X"        — InstallByPackage rejected with rc
///   "error:init:0x%08X"   — sceAppInstUtilInitialize failed with rc
///   "error:init:timeout"  — sceAppInstUtilInitialize timed out
///   "error:badpath"       — path rejected by the daemon's safety check
///   "error:recv"          — daemon saw no valid input on the socket
/// The old decimal-only form ("0", "-2147003130") is still accepted for
/// backward compatibility with older daemons still deployed on a console.
enum DpiReply {
    Ok,
    InstallReject(i32),
    InitFailed(Option<i32>), // None = timeout
    BadPath,
    RecvError,
    Unknown(String),
}

fn parse_dpi_reply(s: &str) -> DpiReply {
    let t = s.trim();
    if t == "ok" || t == "0" {
        return DpiReply::Ok;
    }
    if let Some(rest) = t.strip_prefix("error:init:") {
        if rest == "timeout" {
            return DpiReply::InitFailed(None);
        }
        // Sony error codes have the high bit set (e.g. 0x80B21106) and
        // overflow i32 — parse as u32 then cast so the negative i32
        // representation matches what InstallByPackage actually returns.
        if let Ok(rc) = u32::from_str_radix(rest.trim_start_matches("0x"), 16) {
            return DpiReply::InitFailed(Some(rc as i32));
        }
        return DpiReply::Unknown(t.to_string());
    }
    if let Some(rest) = t.strip_prefix("error:0x") {
        if let Ok(rc) = u32::from_str_radix(rest, 16) {
            return DpiReply::InstallReject(rc as i32);
        }
        return DpiReply::Unknown(t.to_string());
    }
    if t == "error:badpath" {
        return DpiReply::BadPath;
    }
    if t == "error:recv" {
        return DpiReply::RecvError;
    }
    // Backward-compat: old daemon replied with a bare decimal rc.
    if let Ok(rc) = t.parse::<i32>() {
        return if rc == 0 {
            DpiReply::Ok
        } else {
            DpiReply::InstallReject(rc)
        };
    }
    DpiReply::Unknown(t.to_string())
}

/// Connect to the PS5 DPI daemon on `:9040`, send one line (the staged
/// local path or an http(s):// URL), and read back the daemon's reply.
/// The daemon runs `sceAppInstUtilInstallByPackage(uri)` from its own
/// clean loader process, with a timed sceAppInstUtilInitialize + retry
/// so a cold install can never wedge IPMI (issue #152 root cause).
/// The de-facto "DPI v2" port. etaHEN exposes an install bridge here, and
/// elf-arsenal ships a compatible one (`payloads-src/dpiv2/main.c`), so on a
/// console running either, a package URL can be installed over plain HTTP.
const DPI_V2_PORT: u16 = 12800;

/// Hand a package URL to an already-running DPI v2 bridge.
///
/// Why this is tried first: our own DPI daemon has to be sent to the payload
/// loader, and doing that **replaces the main ps5upload payload** for the
/// duration of the install. That swap is the most fragile step in a stream
/// install — it needs the third-party loader alive on :9021, and users who had
/// a working stack of payloads reasonably resent having it disturbed. A
/// console already running etaHEN or elf-arsenal has an installer bridge
/// listening, so we can simply give it the pkg-host URL and leave every
/// running payload untouched.
///
/// Protocol (etaHEN-compatible): `POST /api/install` with `{"url":"…"}`,
/// answered `{"res":"0"}` on acceptance. As with our own daemon, acceptance is
/// NOT proof of a completed install — the caller still verifies through the
/// session tracker.
fn dpi_v2_send(ps5_ip: &str, url: &str) -> std::io::Result<DpiReply> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;

    // The URL goes into a JSON string; a quote or backslash would let a
    // crafted pkg-host path break out of it. Our URLs never contain either,
    // so rejecting is right rather than escaping.
    if url.contains('"') || url.contains('\\') || url.contains('\n') || url.contains('\r') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "url is not safe to embed in a JSON request",
        ));
    }
    let body = format!("{{\"url\":\"{url}\"}}");
    let req = format!(
        "POST /api/install HTTP/1.0\r\n\
         Host: {ps5_ip}:{DPI_V2_PORT}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n{body}",
        body.len()
    );

    let sa = format!("{ps5_ip}:{DPI_V2_PORT}")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("resolve :{DPI_V2_PORT} failed"),
            )
        })?;
    let mut s = std::net::TcpStream::connect_timeout(&sa, std::time::Duration::from_secs(5))?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    // The bridge installs synchronously before replying (elf-arsenal's forwards
    // with `?sync=1` and waits up to 10 minutes), so this needs the same
    // generous deadline as our own daemon.
    s.set_read_timeout(Some(std::time::Duration::from_secs(900)))?;
    s.write_all(req.as_bytes())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    Ok(parse_dpi_v2_reply(&buf))
}

/// Map a DPI v2 HTTP reply onto the same `DpiReply` the native daemon yields,
/// so the hand-off, the session verdict and the UI stay identical either way.
fn parse_dpi_v2_reply(resp: &str) -> DpiReply {
    let body = resp
        .split_once("\r\n\r\n")
        .or_else(|| resp.split_once("\n\n"))
        .map(|(_, b)| b)
        .unwrap_or(resp);
    // `"res":"0"` is the etaHEN success token; elf-arsenal also answers
    // `"ok":true` through the same bridge when it installed synchronously.
    let accepted = body.contains("\"res\":\"0\"")
        || body.contains("\"res\": \"0\"")
        || body.contains("\"ok\":true");
    if accepted {
        return DpiReply::Ok;
    }
    if body.trim().is_empty() {
        return DpiReply::RecvError;
    }
    // A bridge that answers but refuses is a real rejection, not a transport
    // problem. It carries no Sony error code, so use the daemon's ambiguous
    // sentinel and let artifact verification decide.
    DpiReply::InstallReject(-1)
}

/// Install `url` through whichever bridge the console actually has.
///
/// Prefers an already-listening DPI v2 bridge (etaHEN / elf-arsenal) because
/// using it disturbs nothing on the console. Falls back to our own daemon on
/// :9040 when that bridge did not get the install started.
///
/// **A listening bridge is not a working bridge**, which hardware proved on the
/// first real test: elf-arsenal's `dpiv2.elf` forwards to Arsenal's own API on
/// loopback, so when it is loaded standalone it accepts the request, answers
/// `{"res":"-1"}`, and the console never fetches a byte. Preferring it blindly
/// turned an install that our daemon would have completed into a dead end.
///
/// `served` reports how many pkg-host requests the console has made. It is the
/// discriminator that makes a retry safe:
/// - **zero** — Sony's installer never engaged, so nothing was started and
///   nothing can be corrupted by trying again through our own daemon.
/// - **non-zero** — the console really did begin fetching the package. The
///   bridge's refusal is then a genuine installer verdict, and re-running the
///   same install through a second path could act on a half-applied one. We
///   report it and let artifact verification decide.
fn dpi_send_via_best_bridge(
    ps5_ip: &str,
    url: &str,
    served: impl Fn() -> u64,
) -> (std::io::Result<DpiReply>, &'static str) {
    use ps5upload_core::payload_lifecycle as pl;

    let v2_addr = pl::join_host_port(ps5_ip, DPI_V2_PORT);
    if pl::port_is_open(&v2_addr, DPI_PROBE_TIMEOUT) {
        crate::log_info!(
            "dpi: using the DPI v2 bridge already listening on {}",
            v2_addr
        );
        let res = dpi_v2_send(ps5_ip, url);
        let accepted = matches!(res, Ok(DpiReply::Ok));
        let fetched = served();
        if accepted || fetched > 0 {
            if !accepted {
                crate::log_warn!(
                    "dpi: v2 bridge on {} refused after the console fetched {} request(s) —                      reporting its verdict rather than retrying elsewhere",
                    v2_addr,
                    fetched
                );
            }
            return (res, "dpiv2");
        }
        match &res {
            Err(e) => crate::log_warn!(
                "dpi: v2 bridge on {} did not answer ({}); falling back to the ps5upload daemon",
                v2_addr,
                e
            ),
            Ok(_) => crate::log_warn!(
                "dpi: v2 bridge on {} answered but the console fetched nothing — the bridge is \
                 listening without a working installer behind it; falling back to the ps5upload \
                 daemon",
                v2_addr
            ),
        }
    }
    (dpi_send(ps5_ip, url), "ps5upload")
}

fn dpi_send(ps5_ip: &str, line: &str) -> std::io::Result<DpiReply> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;
    let sa = format!("{ps5_ip}:9040")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "resolve :9040 failed"))?;
    let mut s = std::net::TcpStream::connect_timeout(&sa, std::time::Duration::from_secs(5))?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
    // InstallByPackage blocks until the full download completes — a 3.6 GB
    // game over LAN at ~100 MB/s takes ~40 s, but slower links, USB serves,
    // or large multi-part pkgs can take 10+ minutes. Use a generous cap.
    s.set_read_timeout(Some(std::time::Duration::from_secs(900)))?;
    s.write_all(line.as_bytes())?;
    s.write_all(b"\n")?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    Ok(parse_dpi_reply(&buf))
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
fn normalize_mgmt_addr(addr: &str) -> String {
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
fn read_installed_app_ver(mgmt_addr: &str, title_id: &str) -> Option<String> {
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
fn verify_patch_after_install(
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

async fn dpi_install_handler(Json(req): Json<DpiInstallRequest>) -> Response<Body> {
    if !valid_dpi_install_source(&req.local_ps5_path) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "install source must be an absolute PS5 path or an HTTP(S) package URL",
        );
    }
    let ps5_ip = strip_host_port(&req.ps5_addr);
    if ps5_ip.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "ps5_addr is required");
    }
    let path = req.local_ps5_path.clone();
    // Signed download URLs may contain credentials; never put them in logs.
    crate::log_info!(
        "dpi-install: ps5={} source={}",
        ps5_ip,
        if path.starts_with('/') {
            "local"
        } else {
            "remote-url"
        }
    );

    // Snapshot the installed version BEFORE the install so an accepted-but-
    // inert overwrite can be told from a real one afterwards. Sony returns
    // 0x00000000 either way.
    let mgmt = normalize_mgmt_addr(&req.ps5_addr);
    let verify_id = req.title_id.clone().filter(|t| !t.trim().is_empty());
    let verify_pkg_ver = req.package_app_ver.clone().filter(|v| !v.trim().is_empty());
    let app_ver_before = match (&verify_id, &verify_pkg_ver) {
        (Some(tid), Some(_)) => {
            let (m, t) = (mgmt.clone(), tid.clone());
            tokio::task::spawn_blocking(move || read_installed_app_ver(&m, &t))
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };

    let ps5_ip_for_send = ps5_ip.clone();
    let res = tokio::task::spawn_blocking(move || dpi_send(&ps5_ip_for_send, &path)).await;
    match res {
        Ok(Ok(reply)) => {
            let (ok, rc, init_failed, ambiguous, err_message) = match reply {
                DpiReply::Ok => (true, 0, false, false, None),
                DpiReply::InstallReject(rc) => {
                    let ambiguous = rc == -1;
                    if ambiguous {
                        crate::log_warn!(
                            "dpi-install returned daemon sentinel 0xffffffff; verifying artifact"
                        );
                    } else {
                        crate::log_warn!("dpi-install rejected rc=0x{:08x}", rc as u32);
                    }
                    (
                        false,
                        rc,
                        false,
                        ambiguous,
                        if ambiguous {
                            Some("installer acknowledgement was inconclusive".to_string())
                        } else {
                            err_code_message(rc as u32).map(|s| s.to_string())
                        },
                    )
                }
                DpiReply::InitFailed(Some(rc)) => {
                    crate::log_warn!("dpi-install init failed rc=0x{:08x}", rc as u32);
                    (
                        false,
                        -1,
                        true,
                        false,
                        Some(format!(
                            "sceAppInstUtilInitialize failed: 0x{:08X}",
                            rc as u32
                        )),
                    )
                }
                DpiReply::InitFailed(None) => {
                    crate::log_warn!("dpi-install init timed out");
                    (
                        false,
                        -1,
                        true,
                        false,
                        Some(
                            "sceAppInstUtilInitialize timed out (IPMI backend not ready)"
                                .to_string(),
                        ),
                    )
                }
                DpiReply::BadPath => {
                    crate::log_warn!("dpi-install daemon rejected path");
                    (
                        false,
                        -1,
                        false,
                        false,
                        Some("daemon rejected the path (unsafe)".to_string()),
                    )
                }
                DpiReply::RecvError => {
                    crate::log_warn!("dpi-install daemon saw no valid input");
                    (
                        false,
                        -1,
                        false,
                        false,
                        Some("daemon received no valid input".to_string()),
                    )
                }
                DpiReply::Unknown(s) => {
                    crate::log_warn!("dpi-install unknown reply: {:?}", s);
                    (
                        false,
                        -1,
                        false,
                        true,
                        Some(format!("unexpected daemon reply: {s}")),
                    )
                }
            };
            if ok {
                crate::log_info!("dpi-install ok");
            }
            // Only a package that claims a higher version is worth waiting
            // on; a fresh install or a same-version reinstall skips this
            // entirely and costs nothing.
            let (patch_verdict, app_ver_after, ok, err_message) =
                match (ok, &verify_id, &verify_pkg_ver) {
                    (true, Some(tid), Some(pkg_ver)) => {
                        use ps5upload_core::patch_verify::{
                            PatchVerdict, DID_NOT_APPLY_HINT, REGRESSED_HINT,
                        };
                        let (m, t, pv, before) = (
                            mgmt.clone(),
                            tid.clone(),
                            pkg_ver.clone(),
                            app_ver_before.clone(),
                        );
                        let (verdict, after) = tokio::task::spawn_blocking(move || {
                            verify_patch_after_install(&m, &t, before.as_deref(), &pv)
                        })
                        .await
                        .unwrap_or((PatchVerdict::Inconclusive, None));
                        match verdict {
                            PatchVerdict::DidNotApply => {
                                crate::log_error!(
                                "patch did not apply: title={} before={:?} after={:?} package={} \
                                 — Sony accepted the package and copied nothing",
                                tid,
                                app_ver_before,
                                after,
                                pkg_ver
                            );
                                (
                                    Some("did_not_apply".to_string()),
                                    after,
                                    false,
                                    Some(DID_NOT_APPLY_HINT.to_string()),
                                )
                            }
                            PatchVerdict::Applied => {
                                crate::log_info!(
                                    "patch applied: title={} {:?} -> {:?}",
                                    tid,
                                    app_ver_before,
                                    after
                                );
                                (Some("applied".to_string()), after, true, err_message)
                            }
                            PatchVerdict::Regressed => {
                                crate::log_error!(
                                    "update was REMOVED by re-applying it: title={} {:?} -> {:?} \
                                     — the console reverted to the base version",
                                    tid,
                                    app_ver_before,
                                    after
                                );
                                (
                                    Some("regressed".to_string()),
                                    after,
                                    false,
                                    Some(REGRESSED_HINT.to_string()),
                                )
                            }
                            PatchVerdict::Inconclusive => {
                                (Some("inconclusive".to_string()), after, true, err_message)
                            }
                        }
                    }
                    _ => (None, None, ok, err_message),
                };
            json_ok(&DpiInstallResponse {
                bridge: None,
                ok,
                rc,
                init_failed,
                ambiguous,
                err_message,
                requests_served: 0,
                bytes_served: 0,
                patch_verdict,
                app_ver_before,
                app_ver_after,
            })
        }
        Ok(Err(e)) => {
            crate::log_warn!("dpi-install connection ended ambiguously: {e}");
            json_ok(&DpiInstallResponse {
                bridge: None,
                ok: false,
                rc: -1,
                init_failed: false,
                ambiguous: true,
                err_message: Some(format!(
                    "DPI installer connection ended before acknowledgement: {e}"
                )),
                requests_served: 0,
                bytes_served: 0,
                patch_verdict: None,
                app_ver_before,
                app_ver_after: None,
            })
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

// ─── /api/pkg/dpi-direct-install (streaming install beta, #81) ───────

/// Request body for `/api/pkg/dpi-direct-install`. Unlike
/// [`DpiInstallRequest`], this takes a `session_id` (from a prior
/// `/api/pkg/install/start` or the parsed-parts session) instead of a
/// staged PS5 path. The engine serves the pkg at `/pkg-host/{session}/`
/// and hands the DPI daemon that HTTP URL — the daemon pulls the bytes
/// straight off the engine, so no staging copy is uploaded to the PS5's
/// disk first. Saves disk space and one full transfer for the
/// quick-install case.
#[derive(Debug, Deserialize)]
pub struct DpiDirectInstallRequest {
    pub ps5_addr: String,
    pub session_id: String,
}

/// Send the pkg-host URL to the DPI daemon, returning the same
/// [`DpiInstallResponse`] shape as the staged-path route so the client
/// can handle both with identical logic. The daemon pulls the pkg over
/// HTTP; the engine's `/pkg-host/` handler satisfies Range requests so
/// Sony's PlayGo HTTP client works unchanged.
async fn dpi_direct_install_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<DpiDirectInstallRequest>,
) -> Response<Body> {
    let ps5_ip = strip_host_port(&req.ps5_addr);
    if ps5_ip.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "ps5_addr is required");
    }

    // Look up the session to get the content_id (for the canonical
    // pkg-host filename). Hold the lock only long enough to clone what
    // we need — the DPI send below is blocking and must not hold the
    // sessions mutex.
    let content_id = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get(&req.session_id) {
            Some(s) => s.content_id.clone(),
            None => {
                return json_err(
                    StatusCode::NOT_FOUND,
                    &format!("no pkg-host session {}", req.session_id),
                )
            }
        }
    };

    let url = match pkg_host_url_for(&req.ps5_addr, &req.session_id, &content_id) {
        Ok(u) => u,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not build pkg-host URL for PS5 {ps5_ip}: {e}"),
            )
        }
    };

    crate::log_info!(
        "dpi-direct-install: ps5={} session={} url={}",
        ps5_ip,
        req.session_id,
        url
    );
    // Clear any previous attempt's verdict first: the client retries this call
    // on a transient busy, and a stale rejection left on the session would
    // outlive the retry that superseded it.
    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = sessions.get_mut(&req.session_id) {
            s.dpi_ok = None;
            s.dpi_rc = None;
            s.dpi_detail.clear();
        }
    }
    // Live view of the session's request counter, so the bridge choice can tell
    // "the console never started" from "the console started and Sony refused".
    let served_state = state.clone();
    let served_session = req.session_id.clone();
    let served = move || {
        served_state
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&served_session)
            .map(|s| s.requests_served)
            .unwrap_or(0)
    };
    let res = tokio::task::spawn_blocking(move || {
        let (reply, bridge) = dpi_send_via_best_bridge(&ps5_ip, &url, served);
        reply.map(|r| (r, bridge))
    })
    .await;
    let (requests_served, bytes_served) = {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .get(&req.session_id)
            .map(|s| (s.requests_served, s.bytes_served))
            .unwrap_or((0, 0))
    };
    match res {
        Ok(Ok((reply, bridge))) => {
            crate::log_info!("dpi-direct-install: handled by the {} bridge", bridge);
            let (ok, rc, init_failed, ambiguous, err_message) = match reply {
                DpiReply::Ok => (true, 0, false, false, None),
                DpiReply::InstallReject(rc) => {
                    let ambiguous = rc == -1;
                    if ambiguous {
                        crate::log_warn!(
                            "dpi-direct-install returned daemon sentinel 0xffffffff; verifying artifact"
                        );
                    } else {
                        crate::log_warn!("dpi-direct-install rejected rc=0x{:08x}", rc as u32);
                    }
                    (
                        false,
                        rc,
                        false,
                        ambiguous,
                        if ambiguous {
                            Some("installer acknowledgement was inconclusive".to_string())
                        } else {
                            err_code_message(rc as u32).map(|s| s.to_string())
                        },
                    )
                }
                DpiReply::InitFailed(Some(rc)) => {
                    crate::log_warn!("dpi-direct-install init failed rc=0x{:08x}", rc as u32);
                    (
                        false,
                        -1,
                        true,
                        false,
                        Some(format!(
                            "sceAppInstUtilInitialize failed: 0x{:08X}",
                            rc as u32
                        )),
                    )
                }
                DpiReply::InitFailed(None) => {
                    crate::log_warn!("dpi-direct-install init timed out");
                    (
                        false,
                        -1,
                        true,
                        false,
                        Some(
                            "sceAppInstUtilInitialize timed out (IPMI backend not ready)"
                                .to_string(),
                        ),
                    )
                }
                DpiReply::BadPath => {
                    crate::log_warn!("dpi-direct-install daemon rejected URL");
                    (
                        false,
                        -1,
                        false,
                        false,
                        Some("daemon rejected the URL (unsafe)".to_string()),
                    )
                }
                DpiReply::RecvError => {
                    crate::log_warn!("dpi-direct-install daemon saw no valid input");
                    (
                        false,
                        -1,
                        false,
                        false,
                        Some("daemon received no valid input".to_string()),
                    )
                }
                DpiReply::Unknown(s) => {
                    crate::log_warn!("dpi-direct-install unknown reply: {:?}", s);
                    (
                        false,
                        -1,
                        false,
                        true,
                        Some(format!("unexpected daemon reply: {s}")),
                    )
                }
            };
            if ok {
                crate::log_info!("dpi-direct-install ok");
            }
            // Record the daemon's answer ON THE SESSION before replying. The
            // reply is not guaranteed to reach anyone — this call blocks for as
            // long as the console takes to accept the hand-off, and a client
            // timeout, a reverse proxy's own timeout, or a browser navigation
            // all abandon it. Without this the session had no record of what the
            // daemon said, so the status poll reported "installing" until the
            // 600 s startup stall regardless of the daemon having already
            // refused. See the replay in install_status_handler.
            {
                let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = sessions.get_mut(&req.session_id) {
                    s.dpi_ok = Some(ok);
                    s.dpi_rc = Some(rc);
                    s.dpi_detail = err_message.clone().unwrap_or_default();
                }
            }
            json_ok(&DpiInstallResponse {
                ok,
                rc,
                init_failed,
                ambiguous,
                err_message,
                requests_served,
                bytes_served,
                bridge: Some(bridge.to_string()),
                // Stream installs go through the session/status flow, which
                // does its own verification; nothing to report from here.
                patch_verdict: None,
                app_ver_before: None,
                app_ver_after: None,
            })
        }
        Ok(Err(e)) => {
            crate::log_warn!("dpi-direct-install connection ended ambiguously: {e}");
            json_ok(&DpiInstallResponse {
                bridge: None,
                ok: false,
                rc: -1,
                init_failed: false,
                ambiguous: true,
                err_message: Some(format!(
                    "DPI installer connection ended before acknowledgement: {e}"
                )),
                requests_served,
                bytes_served,
                patch_verdict: None,
                app_ver_before: None,
                app_ver_after: None,
            })
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("task: {e}")),
    }
}

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
    // Probe and header-parse are blocking HTTP; keep them off the reactor so
    // concurrent installs for other consoles keep being served.
    let (remote, meta) = tokio::task::spawn_blocking(move || {
        let probe = crate::remote_pkg::RemoteSource::probe(&owned)?;
        let remote = Arc::new(crate::remote_pkg::RemoteSource::new(
            owned,
            probe.total_size,
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
    let metadata = PkgMetadata {
        // No local file exists; the name is for display and logging only.
        path: PathBuf::from(&display_name),
        size: total_size,
        kind: ps5upload_pkg::PkgKind::CntContainer,
        authenticity: head.authenticity,
        content_id: head.content_id,
        title: head.title,
        title_id: head.title_id,
        category: head.category,
        app_ver: head.app_ver,
        fingerprint,
        package_type: req.package_type_override.clone(),
        platform: head.platform,
        icon_png_base64: None,
        warnings: vec![],
    };
    // `parts` stays empty: every range read is proxied, never read off disk.
    Ok((vec![], vec![], total_size, metadata, Some(remote)))
}

#[cfg(target_os = "android")]
async fn resolve_remote_source(
    _url: &str,
    _req: &InstallStartRequest,
) -> Result<ResolvedSource, String> {
    Err("installing from a link is not available in the Android build".into())
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
        crate::remote_pkg::RemoteSource::prefetch_after(remote, end);
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
fn pkg_host_url_for(ps5_addr: &str, session_id: &str, content_id: &str) -> std::io::Result<String> {
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
    let url_filename = pkg_url_filename(content_id);
    Ok(format!(
        "http://{local_ip}:{host_port}/pkg-host/{session_id}/{url_filename}"
    ))
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
fn strip_host_port(host_port: &str) -> String {
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
    use super::*;

    /// The web UI sends the console address the same way it does for every
    /// other pkg route. A rename on either side silently drops the console
    /// and the handler answers "ps5_addr is required".
    #[test]
    fn loader_request_takes_the_shared_ps5_addr_key() {
        let req: LoaderRequest =
            serde_json::from_str(r#"{"ps5_addr":"192.168.1.50:9114"}"#).expect("deserialize");
        assert_eq!(strip_host_port(&req.ps5_addr), "192.168.1.50");
    }

    /// `sent` is the field the caller reads to decide whether the ps5upload
    /// helper was displaced and has to be put back. Serde must always emit
    /// it — including on the failure replies, which is exactly when the
    /// console is at risk of being left with no helper at all.
    #[test]
    fn dpi_ensure_always_reports_whether_the_daemon_was_sent() {
        let failed = DpiEnsureResponse {
            ok: false,
            listening: false,
            sent: true,
            error: Some("DPI daemon did not come up on :9040".into()),
            reason: Some(DPI_REASON_NO_BRINGUP),
        };
        let v: serde_json::Value = serde_json::to_value(&failed).expect("serialize");
        assert_eq!(v["sent"], serde_json::json!(true));
        assert_eq!(v["ok"], serde_json::json!(false));
        assert!(v["error"].is_string());

        // No error key at all on the happy path, so a caller that checks
        // `error` for truthiness doesn't read an empty string as a failure.
        let ok = DpiEnsureResponse {
            ok: true,
            listening: true,
            sent: false,
            error: None,
            reason: None,
        };
        let v: serde_json::Value = serde_json::to_value(&ok).expect("serialize");
        assert!(v.get("error").is_none());
        assert_eq!(v["sent"], serde_json::json!(false));
        assert!(v.get("reason").is_none());
    }

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
mod tests {
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

    /// Build a TrackerObs with sane defaults (1 GB GB expected, idle 0), so
    /// each test sets only the dimension it exercises.
    fn obs(registered: RegisteredObs, consumed: u64, expected: u64, idle_sec: u64) -> TrackerObs {
        TrackerObs {
            registered,
            // Most tracker tests exercise the legacy, no-fingerprint path.
            // Exact-identity tests opt out explicitly below.
            allow_byte_settle: true,
            consumed,
            expected,
            idle_sec,
            startup_sec: 120,
            mid_sec: 240,
            neardone_sec: 600,
            settle_sec: 60,
            synthetic_done_grace_sec: None,
        }
    }

    #[test]
    fn verdict_registered_is_complete_regardless_of_bytes_or_idle() {
        // The authoritative signal wins instantly — even with zero observed
        // bytes (the dir/free-space signals can lag the rename-into-place) and
        // a long idle. This is the ONLY path that deletes a verified pkg.
        assert_eq!(
            install_verdict(&obs(Some(true), 0, 25_000_000_000, 9_999)),
            InstallVerdict::Complete
        );
    }

    #[test]
    fn verdict_large_install_in_progress_is_never_complete() {
        // The exact Bloodborne shape: 25 GB expected, title not yet registered,
        // a few GB written, progress recent (idle 8s). MUST be Installing — the
        // old code returned "done" here and deleted the pkg mid-install.
        let v = install_verdict(&obs(Some(false), 3_000_000_000, 25_000_000_000, 8));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_progress_resets_stall_for_slow_but_advancing_install() {
        // Even a 200 GB install that's only 10% done stays Installing as long
        // as bytes keep landing (idle below the mid deadline). Size-agnostic.
        let v = install_verdict(&obs(Some(false), 20_000_000_000, 200_000_000_000, 200));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_mid_install_flatline_stalls() {
        // Some bytes landed but well short of target, and NO progress past the
        // mid deadline → genuinely stuck → Stalled (terminal, but pkg kept).
        let v = install_verdict(&obs(Some(false), 5_000_000_000, 25_000_000_000, 241));
        assert_eq!(v, InstallVerdict::Stalled);
        // One second earlier it must still be Installing (boundary).
        let v = install_verdict(&obs(Some(false), 5_000_000_000, 25_000_000_000, 239));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_near_done_is_patient_past_the_mid_deadline() {
        // ≥90% consumed → the final commit/register phase writes ~nothing, so a
        // flatline that WOULD trip the mid deadline (241s) must NOT stall yet —
        // we wait out the longer near-done window for the title to register.
        let v = install_verdict(&obs(Some(false), 24_000_000_000, 25_000_000_000, 300));
        assert_eq!(v, InstallVerdict::Installing);
        // …but a flatline past the near-done window is still a stall.
        let v = install_verdict(&obs(Some(false), 24_000_000_000, 25_000_000_000, 601));
        assert_eq!(v, InstallVerdict::Stalled);
    }

    #[test]
    fn verdict_startup_grace_before_any_bytes() {
        // Zero bytes consumed (Sony hasn't begun writing) gets the startup
        // grace, not the stricter mid deadline.
        assert_eq!(
            install_verdict(&obs(Some(false), 0, 25_000_000_000, 119)),
            InstallVerdict::Installing
        );
        assert_eq!(
            install_verdict(&obs(Some(false), 0, 25_000_000_000, 121)),
            InstallVerdict::Stalled
        );
    }

    #[test]
    fn verdict_unsupported_fw_completes_on_byte_settle() {
        // No launch verification possible (None). Byte-accounting is the signal:
        // ~all expected bytes landed AND writing settled ⇒ Complete (delete ok).
        // 24.8/25.0 GB = 99.2% ≥ INSTALL_SETTLE_FRACTION (0.99).
        let v = install_verdict(&obs(None, 24_800_000_000, 25_000_000_000, 60));
        assert_eq!(v, InstallVerdict::Complete);
        // Settled but well short of expected ⇒ NOT complete (don't delete).
        let v = install_verdict(&obs(None, 10_000_000_000, 25_000_000_000, 300));
        assert_eq!(v, InstallVerdict::Stalled);
        // ~all bytes but not yet settled ⇒ still Installing (let it settle).
        let v = install_verdict(&obs(None, 24_900_000_000, 25_000_000_000, 30));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_ext_storage_install_completes_on_byte_settle() {
        // EXTENDED-STORAGE install: the title's app.pkg lands on /mnt/ext*,
        // which the payload's FS_LIST_DIR sees only through a stale mount, so the
        // launch check FALSE-reports Absent (Some(false)). Byte-accounting must
        // still confirm Complete — else a game that installed and PLAYS gets
        // reported as failed (the real bug this fixes). HW-confirmed on a Pro.
        // 24.8/25.0 GB = 99.2% ≥ INSTALL_SETTLE_FRACTION (0.99).
        let v = install_verdict(&obs(Some(false), 24_800_000_000, 25_000_000_000, 60));
        assert_eq!(v, InstallVerdict::Complete);
        // A "dead tile" (registers appmeta but writes ~no content) has Absent +
        // ~zero bytes ⇒ must NOT complete — byte-accounting cleanly rejects it.
        let v = install_verdict(&obs(Some(false), 80_000_000, 25_000_000_000, 300));
        assert_eq!(v, InstallVerdict::Stalled);
        // Absent + still writing (not settled) ⇒ keep installing, don't claim done.
        let v = install_verdict(&obs(Some(false), 24_900_000_000, 25_000_000_000, 20));
        assert_eq!(v, InstallVerdict::Installing);
    }

    #[test]
    fn verdict_unknown_size_never_false_completes() {
        // expected == 0 (size unknown) ⇒ fraction 0, so the settle/near-done
        // shortcuts can't fire. It can only ever be Installing or, on a true
        // flatline, Stalled — never a spurious Complete that deletes the pkg.
        assert_eq!(
            install_verdict(&obs(None, 9_999, 0, 10)),
            InstallVerdict::Installing
        );
        assert_eq!(
            install_verdict(&obs(None, 9_999, 0, 999)),
            InstallVerdict::Stalled
        );
        // …unless the title actually registers.
        assert_eq!(
            install_verdict(&obs(Some(true), 0, 0, 0)),
            InstallVerdict::Complete
        );
    }

    #[test]
    fn verdict_synthetic_done_grace_ends_unverified_when_absent() {
        // Synthetic-DONE tier (shellui-rpc / appinst-local): the payload
        // reported Done immediately, but verify_launchable can't see the
        // title (Some(false) = Absent). Without the grace path, this would
        // spin until the stall deadline (10+ min of "installing" after the
        // PS5 accepted the request). The grace period ends polling, but must
        // not claim success or authorize deletion of the staged package.
        let mut o = obs(Some(false), 0, 25_000_000_000, 180);
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::AcceptedUnverified);
    }

    #[test]
    fn verdict_synthetic_done_grace_ends_unverified_on_unsupported_fw() {
        // Issue #230: on firmware where verify_launchable returns None
        // (Unsupported — e.g. FW 12.20 where app.db is unreadable), the
        // grace path must STILL fire for synthetic-DONE tiers. It reports an
        // explicit unverified terminal state rather than either hanging or
        // inventing a successful installation.
        let mut o = obs(None, 0, 25_000_000_000, 180);
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::AcceptedUnverified);
    }

    #[test]
    fn verdict_synthetic_done_tiny_progress_never_false_completes() {
        // Regression: a synthetic acknowledgement plus a few bytes of
        // progress is not evidence that a large package finished installing.
        let mut o = obs(Some(false), 1_000_000, 25_000_000_000, 180);
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::AcceptedUnverified);
    }

    #[test]
    fn verdict_synthetic_done_near_complete_settle_is_still_confirmed() {
        // The existing byte-settle proof remains stronger than the synthetic
        // acknowledgement grace and may still confirm a completed install.
        let mut o = obs(Some(false), 24_900_000_000, 25_000_000_000, 180);
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::Complete);
    }

    #[test]
    fn verdict_exact_variant_never_completes_from_unrelated_byte_movement() {
        // Two same-version patches can have identical Sony metadata and size.
        // Free-space movement cannot prove which file landed, so an install
        // carrying a sampled package identity must wait for the exact artifact.
        let mut o = obs(Some(false), 24_900_000_000, 25_000_000_000, 180);
        o.allow_byte_settle = false;
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::AcceptedUnverified);
    }

    #[test]
    fn verdict_synthetic_done_grace_not_yet_elapsed_stays_installing() {
        // Before the grace period elapses, a synthetic-DONE install that
        // hasn't registered and hasn't settled should stay Installing —
        // don't declare victory too early, Sony might still error out.
        // (idle=100 is below the startup stall deadline of 120 AND below
        // the grace period of 180.)
        let mut o = obs(Some(false), 0, 25_000_000_000, 100);
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::Installing);
    }

    #[test]
    fn verdict_registered_title_still_completes_with_synthetic_grace() {
        // If verify_launchable returned Some(true), the install is already
        // confirmed Complete via the registered check at the top — the grace
        // path is never reached. This test documents that the grace condition
        // explicitly excludes Some(true) via `registered != Some(true)`.
        let mut o = obs(Some(true), 0, 0, 0);
        o.synthetic_done_grace_sec = Some(180);
        assert_eq!(install_verdict(&o), InstallVerdict::Complete);
    }

    #[test]
    fn verdict_synthetic_done_grace_not_set_for_real_polled_installs() {
        // Real-polled (direct-bgft) installs: synthetic_done_grace_sec is
        // None. verify_launchable == Absent + no byte progress → stalls.
        let o = obs(Some(false), 0, 25_000_000_000, 999);
        assert_eq!(install_verdict(&o), InstallVerdict::Stalled);
    }

    #[test]
    fn is_synthetic_done_tier_classification() {
        // Direct BGFT (no synthetic flags): not synthetic-done.
        assert!(!is_synthetic_done_tier(Some(0x0000_1234)));
        // shellui-rpc flag set: synthetic-done.
        assert!(is_synthetic_done_tier(Some(
            APPINST_VIA_SHELLUI_FLAG | 0x1234
        )));
        // appinst-local flag set: synthetic-done.
        assert!(is_synthetic_done_tier(Some(
            APPINST_VIA_LOCAL_FLAG | 0x1234
        )));
        // tier0-worker flag set: synthetic-done (issue #230 — was missing).
        assert!(is_synthetic_done_tier(Some(
            APPINST_VIA_TIER0_FLAG | 0x1234
        )));
        // Both the base task-id flag + local: still synthetic-done.
        assert!(is_synthetic_done_tier(Some(
            ps5upload_core::pkg_install::APPINST_TASK_ID_FLAG | APPINST_VIA_LOCAL_FLAG | 0x1234
        )));
        // Failure sentinel (-1): not synthetic-done (it's "no tier reached").
        assert!(!is_synthetic_done_tier(Some(-1)));
        // No task_id yet: not synthetic-done.
        assert!(!is_synthetic_done_tier(None));
    }

    // ── staged DLC/patch routing (the FW 9.60 destructive-reject fix) ──

    #[test]
    fn staged_dlc_is_routed_to_dpi_before_main_payload() {
        // DLC routes whether or not DPI is already listening: the in-process
        // cascade can delete an installed add-on before it returns.
        assert!(staged_requires_dpi(true, false, "PS4AC", false));
        assert!(staged_requires_dpi(true, false, "PS5AC", false));
        assert!(staged_requires_dpi(true, false, "PS4AC", true));
    }

    #[test]
    fn staged_patch_skips_the_main_payload_only_when_dpi_is_up() {
        // The in-process attempt is a measured rejection on both consoles
        // (0x80B2150F on FW 5.10, 0x80B2116F on FW 9.60), so skip it — but
        // only while the route that does apply patches is actually up.
        assert!(staged_requires_dpi(true, false, "PS4DP", true));
        assert!(staged_requires_dpi(true, false, "PS5DP", true));
        // No DPI listening: keep the in-process cascade, which does apply
        // patches on some firmware points. Refusing here would turn a
        // working install into a failure on a console whose loader is
        // simply not running.
        assert!(!staged_requires_dpi(true, false, "PS4DP", false));
        assert!(!staged_requires_dpi(true, false, "PS5DP", false));
    }

    #[test]
    fn staged_dpi_route_does_not_capture_stream_or_other_types() {
        // Stream already invokes DPI directly, so its serve-only setup must
        // still create an HTTP session instead of returning the staged handoff.
        assert!(!staged_requires_dpi(true, true, "PS4AC", true));
        assert!(!staged_requires_dpi(false, false, "PS4AC", true));
        assert!(!staged_requires_dpi(true, true, "PS4DP", true));
        assert!(!staged_requires_dpi(false, false, "PS4DP", true));
        assert!(!staged_requires_dpi(true, false, "PS4GD", true));
    }

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
            bytes_served: 0,
            transfer_bytes: 0,
            transfer: TransferCoverage::new(total),
            dpi_ok: None,
            dpi_rc: None,
            dpi_detail: String::new(),
            remote: None,
        }
    }

    /// A bridge can be listening without a working installer behind it —
    /// elf-arsenal's `dpiv2.elf` forwards to Arsenal's own API on loopback, so
    /// loaded standalone it accepts the request, answers `{"res":"-1"}` and the
    /// console never fetches a byte. Measured on hardware (Phat, FW 5.10):
    /// `requests_served: 0`, and an install our own daemon had just completed
    /// twice became a dead end. Zero fetches therefore MUST fall back; a
    /// refusal after real fetches must not, because that is Sony's verdict on
    /// an install that actually started.
    #[test]
    fn a_bridge_that_refuses_without_the_console_fetching_falls_back() {
        // No bridge is listening on this port in the test environment, so the
        // selection function can only reach the fallback — which is itself the
        // guarantee we want when nothing answers.
        let (_res, bridge) =
            dpi_send_via_best_bridge("127.0.0.1", "http://127.0.0.1:1/x.pkg", || 0);
        assert_eq!(
            bridge, "ps5upload",
            "with no reachable v2 bridge the native daemon must be used"
        );
    }

    /// The decision rule itself, isolated from any socket: what the handler
    /// must do for each (bridge answer, bytes fetched) pair.
    #[test]
    fn bridge_fallback_rule_matches_what_hardware_showed() {
        // (accepted by bridge, requests the console made) -> keep the bridge's answer?
        let keep = |accepted: bool, fetched: u64| accepted || fetched > 0;

        // Accepted: always the bridge's result, fetches or not.
        assert!(keep(true, 0));
        assert!(keep(true, 57));
        // Refused having fetched nothing: the elf-arsenal-standalone case.
        // Must NOT be kept — fall back to our daemon.
        assert!(!keep(false, 0));
        // Refused after the console really started: Sony's verdict, keep it
        // rather than re-running the install down a second path.
        assert!(keep(false, 1));
        assert!(keep(false, 57));
    }

    /// etaHEN and elf-arsenal answer the DPI v2 bridge differently on
    /// success, and a bridge that answers-but-refuses must not be confused
    /// with one that never answered: the first is Sony's real verdict, the
    /// second is the only case worth retrying through our own daemon.
    #[test]
    fn dpi_v2_replies_map_onto_the_native_daemon_verdicts() {
        let ok_etahen = "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"res\":\"0\"}";
        assert!(matches!(parse_dpi_v2_reply(ok_etahen), DpiReply::Ok));

        // elf-arsenal's bridge forwards Arsenal's own synchronous verdict.
        let ok_arsenal = "HTTP/1.0 200 OK\r\n\r\n{\"ok\":true,\"via\":\"dpi\"}";
        assert!(matches!(parse_dpi_v2_reply(ok_arsenal), DpiReply::Ok));

        // Whitespace after the colon is still success.
        let ok_spaced = "HTTP/1.0 200 OK\r\n\r\n{\"res\": \"0\"}";
        assert!(matches!(parse_dpi_v2_reply(ok_spaced), DpiReply::Ok));

        // A refusal is a real rejection, reported with the ambiguous sentinel
        // so artifact verification decides rather than a bogus Sony code.
        let refused = "HTTP/1.0 500 Error\r\n\r\n{\"res\":\"-1\",\"error\":\"install failed\"}";
        assert!(matches!(
            parse_dpi_v2_reply(refused),
            DpiReply::InstallReject(-1)
        ));

        // Nothing came back at all — transport failure, worth a fallback.
        assert!(matches!(parse_dpi_v2_reply(""), DpiReply::RecvError));
        assert!(matches!(
            parse_dpi_v2_reply("HTTP/1.0 200 OK\r\n\r\n"),
            DpiReply::RecvError
        ));
    }

    /// The URL is interpolated into a JSON string, so anything that could
    /// close that string has to be refused rather than escaped — our pkg-host
    /// URLs never contain these, so refusing costs nothing and rules out the
    /// injection entirely.
    #[test]
    fn dpi_v2_refuses_a_url_that_could_break_out_of_the_json_body() {
        for bad in [
            "http://host/a\".pkg",
            "http://host/a\\pkg",
            "http://host/a\npkg",
            "http://host/a\rpkg",
        ] {
            let Err(err) = dpi_v2_send("127.0.0.1", bad) else {
                panic!("must refuse {bad:?}");
            };
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "for {bad:?}");
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
        session.remote = Some(remote);

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

    // ── DPI daemon reply parser (the FW-10.40 helper-death fix, #152) ──
    //
    // The daemon now replies in the reference's ok/error form so the
    // engine can tell accept from reject from init-failure. These pin
    // every branch of the parser so a future daemon change can't
    // silently regress to "treat init-failure as install-success".

    #[test]
    fn dpi_parse_ok() {
        assert!(matches!(parse_dpi_reply("ok"), DpiReply::Ok));
        // trailing whitespace / newline tolerated
        assert!(matches!(parse_dpi_reply("ok\n"), DpiReply::Ok));
        assert!(matches!(parse_dpi_reply(" ok\r\n"), DpiReply::Ok));
    }

    #[test]
    fn dpi_parse_install_reject() {
        // 0x80B21106 — the FW-11/12 authid gate (the expected first-attempt
        // rejection that triggers the DPI fallback in the first place).
        assert!(matches!(
            parse_dpi_reply("error:0x80B21106"),
            DpiReply::InstallReject(rc) if rc as u32 == 0x80B21106
        ));
        assert!(matches!(
            parse_dpi_reply("error:0x80b21106\n"),
            DpiReply::InstallReject(rc) if rc as u32 == 0x80B21106
        ));
        // The daemon historically leaked its internal -1 sentinel in this
        // form. Handlers classify it as ambiguous and verify the installed
        // artifact; it must never be presented as a Sony error code.
        assert!(matches!(
            parse_dpi_reply("error:0xffffffff"),
            DpiReply::InstallReject(-1)
        ));
    }

    #[test]
    fn dpi_parse_init_failed_with_rc() {
        // sceAppInstUtilInitialize returned a Sony error — daemon is in
        // fallback mode and retrying will likely fail the same way.
        assert!(matches!(
            parse_dpi_reply("error:init:0x80B21106"),
            DpiReply::InitFailed(Some(rc)) if rc as u32 == 0x80B21106
        ));
    }

    #[test]
    fn dpi_parse_init_timeout() {
        // timed_init returned the -0xDEAD sentinel. Distinct from a Sony
        // error code — IPMI backend never came up.
        assert!(matches!(
            parse_dpi_reply("error:init:timeout"),
            DpiReply::InitFailed(None)
        ));
    }

    #[test]
    fn dpi_parse_badpath_and_recv() {
        assert!(matches!(
            parse_dpi_reply("error:badpath"),
            DpiReply::BadPath
        ));
        assert!(matches!(parse_dpi_reply("error:recv"), DpiReply::RecvError));
    }

    #[test]
    fn dpi_parse_legacy_decimal_ok() {
        // Backward compat: an older daemon still deployed on a console
        // replies with a bare decimal rc. "0" must map to Ok.
        assert!(matches!(parse_dpi_reply("0"), DpiReply::Ok));
        assert!(matches!(parse_dpi_reply("0\n"), DpiReply::Ok));
    }

    #[test]
    fn dpi_parse_legacy_decimal_reject() {
        // Legacy decimal reject: 0x80B21106 reinterpreted as i32 is
        // -2135813882 (i32::MIN + 0x7F8AAFAE + ... — two's complement).
        let expected: i32 = 0x80B21106_u32 as i32;
        assert!(matches!(
            parse_dpi_reply(&expected.to_string()),
            DpiReply::InstallReject(rc) if rc as u32 == 0x80B21106
        ));
    }

    #[test]
    fn dpi_parse_unknown_is_not_ok() {
        // An unrecognised reply must NEVER parse as Ok (would mask a
        // real failure as success) — it falls through to Unknown.
        assert!(matches!(parse_dpi_reply("error:???"), DpiReply::Unknown(_)));
        assert!(matches!(parse_dpi_reply(""), DpiReply::Unknown(_)));
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

    #[test]
    fn sony_log_verdict_reads_playgo_firmware_refusal() {
        // Captured 2026-09-14 streaming Minecraft (debug FPKG) to a FW 9.60 Pro.
        // Transfer finished 0x0; the request then died with 0x80a3000d.
        let log = "\
[PlayGoCore][RequestInstall] begin (#2, UP4433-PPSA17221_00-MINECRAFTPS50000)
[PlayGoCore][Request #2] transfer started (196608/1333460992)
[PlayGoCore][Request #2] transfer ended (0x00000000)
[PlayGoCore][Request #2] request ended (state = 9, error = 0x80a3000d, 15642 [msec])
Task 2000002f : playgo.progress.state=9, progress.error_code=0x80a3000d
[BGFT] [329] changed error code (0x80a3000d -> 0x809900c1)
";
        match sony_log_verdict(log, "UP4433-PPSA17221_00-MINECRAFTPS50000") {
            Some(SonyLogVerdict::Refused { err_code, .. }) => {
                assert_eq!(err_code, 0x80A3_000D);
            }
            other => panic!("expected firmware refusal, got {other:?}"),
        }
    }

    #[test]
    fn sony_log_verdict_reads_clean_request_end() {
        let log = "\
[PlayGoCore][RequestInstall] begin (#7, UP9000-CUSA07842_00-SCUS974290000001)
[PlayGoCore][Request #7] request ended (state = 7, error = 0x0)
";
        assert_eq!(
            sony_log_verdict(log, "UP9000-CUSA07842_00-SCUS974290000001"),
            Some(SonyLogVerdict::Installed)
        );
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

    /// Every verdict must produce its own phase — the bug this mapping exists to
    /// prevent was Complete leaving the phase untouched, which reported a
    /// finished install as still installing for as long as anything polled it.
    #[test]
    fn every_verdict_names_its_own_phase() {
        let total = 1_345_936_761;
        let phase = |v| verdict_phase(v, true, 158, total, total);
        assert_eq!(phase(InstallVerdict::Complete), InstallPhase::Done);
        assert_eq!(
            phase(InstallVerdict::AcceptedUnverified),
            InstallPhase::Done
        );
        assert_eq!(phase(InstallVerdict::Stalled), InstallPhase::Error);
        // Only the live verdict is allowed to look like a live phase.
        assert_eq!(phase(InstallVerdict::Installing), InstallPhase::Install);
        assert_ne!(
            phase(InstallVerdict::Complete),
            phase(InstallVerdict::Installing)
        );
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

    /// The Stream state model, pinned. Measured 2026-09-14 streaming a 1.35 GB
    /// debug FPKG: 152 requests, 1.53 GB served (the sum overshoots), and the
    /// engine reported `install` for the whole 19 s transfer.
    #[test]
    fn stream_phase_tracks_the_transfer() {
        let total = 1_345_936_761;
        // Nothing fetched yet: the console hasn't started, and this must not be
        // dressed up as a transfer in progress.
        assert_eq!(in_flight_phase(true, 0, 0, total), InstallPhase::Queued);
        // Mid-transfer.
        assert_eq!(
            in_flight_phase(true, 30, 177_233_273, total),
            InstallPhase::Download
        );
        // Transfer finished; Sony is writing.
        assert_eq!(
            in_flight_phase(true, 152, total, total),
            InstallPhase::Install
        );
        // A re-fetch can push the *sum* past the total while the furthest byte
        // is still short — the phase must follow `transfer_bytes`.
        assert_eq!(
            in_flight_phase(true, 152, total - 1, total),
            InstallPhase::Download
        );
        // Unknown total (a split set that never reported one) must not sit in
        // `download` forever.
        assert_eq!(in_flight_phase(true, 4, 4096, 0), InstallPhase::Install);
        // A staged install reads from PS5 disk: no transfer to report.
        assert_eq!(in_flight_phase(false, 0, 0, total), InstallPhase::Install);
    }

    #[test]
    fn sony_log_verdict_ignores_other_titles() {
        let log = "\
[PlayGoCore][RequestInstall] begin (#2, UP4433-PPSA17221_00-MINECRAFTPS50000)
[PlayGoCore][Request #2] request ended (state = 9, error = 0x80a3000d, 15642 [msec])
";
        assert_eq!(
            sony_log_verdict(log, "UP9000-CUSA07842_00-SCUS974290000001"),
            None
        );
    }
}
