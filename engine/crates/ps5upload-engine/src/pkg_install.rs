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
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, Response, StatusCode},
    routing::{get, post},
    Json, Router,
};
use ps5upload_core::pkg_install::{
    err_code_message, pkg_install, pkg_install_status, InstallPhase, PkgInstallRequest,
    PkgInstallResponse, PkgInstallStatus,
};
use ps5upload_pkg::{parse_pkg, parse_split_pkg, PkgMetadata, SplitPkgMetadata};
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
}

#[derive(Default)]
pub struct PkgInstallState {
    pub sessions: Mutex<HashMap<String, InstallSession>>,
}

pub type PkgInstallStateHandle = Arc<PkgInstallState>;

pub fn router(state: PkgInstallStateHandle) -> Router {
    Router::new()
        .route("/api/pkg/parse", post(parse_handler))
        .route("/api/pkg/parse-split", post(parse_split_handler))
        .route("/api/pkg/install/start", post(install_start_handler))
        .route("/api/pkg/install/status", get(install_status_handler))
        .route("/api/pkg/install/cancel", post(install_cancel_handler))
        .route("/pkg-host/{session}/file.pkg", get(serve_handler))
        .with_state(state)
}

// ─── /api/pkg/parse ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ParseRequest {
    pub path: String,
}

async fn parse_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    match parse_pkg(std::path::Path::new(&req.path)) {
        Ok(meta) => json_ok(&meta),
        Err(e) => json_err(StatusCode::BAD_REQUEST, &format!("parse failed: {e}")),
    }
}

async fn parse_split_handler(Json(req): Json<ParseRequest>) -> Response<Body> {
    match parse_split_pkg(std::path::Path::new(&req.path)) {
        Ok(meta) => json_ok(&meta),
        Err(e) => json_err(StatusCode::BAD_REQUEST, &format!("split parse failed: {e}")),
    }
}

// ─── /api/pkg/install/start ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InstallStartRequest {
    /// PS5 mgmt-port address, e.g. "192.168.1.42:9114".
    pub ps5_addr: String,
    /// Either `path` (single .pkg) or `split_root` (lead `.pkg` of a
    /// split set) must be set. `split_root` triggers split-pkg
    /// detection — we look for `<root>.0`, `<root>.1`, ... siblings.
    pub path: Option<String>,
    pub split_root: Option<String>,
    /// Optional override for the package_type passed to BGFT. When
    /// unset we use whatever `derive_package_type(category)` returns
    /// or fall back to "PS4GD". Useful for unknown-magic PKGs where
    /// the user picks the type manually in the UI.
    pub package_type_override: Option<String>,
    /// Optional PS5-side absolute path to a pre-uploaded `.pkg`. When
    /// set, the install URL becomes `file:///<path>` and the
    /// HTTP-host setup is skipped — Sony's installer reads the
    /// bytes from the PS5's local filesystem. This is the path
    /// etaHEN / GoldHEN-style installers use; it's substantially
    /// more reliable than HTTP-pull because:
    ///
    /// - No desktop-IP / firewall / process-context dependency.
    /// - Sony's installer accepts file:// URIs from any caller
    ///   context that can call sceAppInstUtilInstallByPackage.
    /// - The PS5 needs disk space for the .pkg first, but the
    ///   existing FTX2 single-file upload (Upload tab) puts it
    ///   wherever the user wants.
    ///
    /// When unset, falls back to the legacy HTTP-host flow.
    pub local_ps5_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InstallStartResponse {
    pub session_id: String,
    pub url: String,
    pub task_id: i32,
    pub err_code: u32,
    pub err_message: Option<String>,
    pub detail: String,
}

async fn install_start_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<InstallStartRequest>,
) -> Response<Body> {
    let (parts, part_sizes, total_size, head_meta) = match resolve_parts_and_meta(&req).await {
        Ok(t) => t,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &e),
    };

    let package_type = req
        .package_type_override
        .clone()
        .or_else(|| head_meta.package_type.clone())
        .unwrap_or_else(|| "PS4GD".to_string());

    // Two URL strategies:
    //   1. file:// — preferred when the caller has uploaded the
    //      .pkg to the PS5's local disk first (via Upload tab or
    //      similar) and passes its absolute path. Sony's installer
    //      reads from disk, no network round-trip, no desktop-IP /
    //      firewall / process-context dependency. Matches what
    //      etaHEN's DirectPKGInstaller and GoldHEN's RPI use.
    //   2. http:// — legacy fallback. The desktop hosts the .pkg
    //      bytes on PS5UPLOAD_ENGINE_PORT and the PS5 pulls them.
    //      Works when LAN routing is straightforward and the
    //      desktop's firewall lets the PS5 connect inbound, but
    //      community installers gave this up because of repeated
    //      cross-firmware reliability issues.
    let session_id = Uuid::new_v4().to_string();
    let url = if let Some(local_path) = req.local_ps5_path.as_deref().filter(|s| !s.is_empty()) {
        // Reject paths that aren't absolute on the PS5 — file:// URIs
        // need an absolute path or Sony's installer rejects with a
        // parse error.
        if !local_path.starts_with('/') {
            return json_err(
                StatusCode::BAD_REQUEST,
                &format!("local_ps5_path must be absolute (start with /), got: {local_path}"),
            );
        }
        format!("file://{local_path}")
    } else {
        // Pick the LAN IP this host presents to the PS5. Multi-NIC
        // safe: bind a UDP socket "connected" to the PS5's mgmt
        // addr and read the local addr — that's the IP the OS
        // picked for outbound.
        let ps5_host_only = req.ps5_addr.split(':').next().unwrap_or("").to_string();
        let local_ip = match lan_ip_for_ps5(&ps5_host_only) {
            Ok(ip) => ip,
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("could not determine local LAN IP for PS5 {ps5_host_only}: {e}"),
                )
            }
        };
        let host_port = std::env::var("PS5UPLOAD_ENGINE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(19113);
        format!("http://{local_ip}:{host_port}/pkg-host/{session_id}/file.pkg")
    };

    let session = InstallSession {
        id: session_id.clone(),
        parts,
        part_sizes,
        total_size,
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
        ps5_mgmt_addr: req.ps5_addr.clone(),
        task_id: None,
        err_code: 0,
        detail: String::new(),
        cancelled: false,
        created_at_unix: now_unix(),
    };

    // Insert *before* sending the install frame so the HTTP listener
    // is ready to serve when BGFT starts pulling immediately on its end.
    state
        .sessions
        .lock()
        .unwrap()
        .insert(session_id.clone(), session.clone());

    let install_req = PkgInstallRequest {
        url: url.clone(),
        content_id: session.content_id.clone(),
        size: session.total_size,
        title: session.title.clone(),
        package_type,
    };

    let resp: PkgInstallResponse = match pkg_install(&req.ps5_addr, &install_req) {
        Ok(r) => r,
        Err(e) => {
            // Roll back the session — the install never started.
            state.sessions.lock().unwrap().remove(&session_id);
            return json_err(
                StatusCode::BAD_GATEWAY,
                &format!("payload PKG_INSTALL failed: {e}"),
            );
        }
    };

    {
        let mut sessions = state.sessions.lock().unwrap();
        if let Some(sess) = sessions.get_mut(&session_id) {
            sess.task_id = Some(resp.task_id);
            sess.err_code = resp.err_code;
            sess.detail = resp.detail.clone();
        }
    }

    let err_message = err_code_message(resp.err_code).map(|s| s.to_string());

    json_ok(&InstallStartResponse {
        session_id,
        url,
        task_id: resp.task_id,
        err_code: resp.err_code,
        err_message,
        detail: resp.detail,
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

/// Drop sessions older than the configured GC threshold. Called as a
/// best-effort sweep at the start of every status handler invocation.
/// Cheap (linear in active session count, which is bounded by the
/// queue UI to <100 in practice).
fn gc_old_sessions(state: &PkgInstallStateHandle) {
    let now = now_unix();
    let max_age = pkg_session_max_age_sec();
    let mut sessions = state.sessions.lock().unwrap();
    sessions.retain(|_, s| {
        // Always keep sessions younger than the GC threshold; drop
        // older ones regardless of state. A session that's still
        // actively polling but past the threshold is treated as
        // orphaned (queue UI's worker loop terminates at done/error/
        // cancelled or after pollErrors >= 5, so it shouldn't be
        // legitimately polling a 2h-old session anyway).
        now.saturating_sub(s.created_at_unix) < max_age
    });
}

async fn install_status_handler(
    State(state): State<PkgInstallStateHandle>,
    Query(q): Query<StatusQuery>,
) -> Response<Body> {
    gc_old_sessions(&state);
    let (ps5_addr, task_id, total, cancelled) = {
        let sessions = state.sessions.lock().unwrap();
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
            ),
        }
    };
    let task_id = match task_id {
        Some(t) => t,
        None => return json_err(StatusCode::CONFLICT, "session has no BGFT task_id yet"),
    };
    let status: PkgInstallStatus = match pkg_install_status(&ps5_addr, task_id) {
        Ok(s) => s,
        Err(e) => {
            return json_err(
                StatusCode::BAD_GATEWAY,
                &format!("payload PKG_INSTALL_STATUS failed: {e}"),
            )
        }
    };

    // Surface the BGFT-reported total bytes only when it's non-zero;
    // BGFT sometimes reports 0 before download starts. Otherwise fall
    // back to our own known total.
    let total = if status.total > 0 {
        status.total
    } else {
        total
    };

    json_ok(&StatusResponse {
        session_id: q.session,
        phase: status.phase,
        downloaded: status.downloaded,
        total,
        err_code: status.err_code,
        err_message: err_code_message(status.err_code).map(|s| s.to_string()),
        detail: status.detail,
        cancelled,
    })
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

async fn install_cancel_handler(
    State(state): State<PkgInstallStateHandle>,
    Json(req): Json<CancelRequest>,
) -> Response<Body> {
    let mut sessions = state.sessions.lock().unwrap();
    match sessions.get_mut(&req.session) {
        Some(s) => {
            s.cancelled = true;
            json_ok(&CancelResponse {
                session_id: req.session,
                host_stopped: true,
            })
        }
        None => json_err(
            StatusCode::NOT_FOUND,
            &format!("no install session {}", req.session),
        ),
    }
}

// ─── /pkg-host/:session/file.pkg ─────────────────────────────────────

async fn serve_handler(
    State(state): State<PkgInstallStateHandle>,
    AxumPath(session): AxumPath<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let session = match state.sessions.lock().unwrap().get(&session) {
        Some(s) if !s.cancelled => s.clone(),
        Some(_) => return plain_response(StatusCode::GONE, "install session was cancelled"),
        None => return plain_response(StatusCode::NOT_FOUND, "no such install session"),
    };

    let total = session.total_size;
    let (start, end) = match parse_range_header(&headers, total) {
        Ok(r) => r,
        Err(_) => return plain_response(StatusCode::RANGE_NOT_SATISFIABLE, "invalid Range"),
    };

    let chunk = match read_split_range(&session, start, end) {
        Ok(b) => b,
        Err(e) => {
            return plain_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("read failed: {e}"),
            )
        }
    };

    let len = chunk.len() as u64;
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len.to_string());

    let has_range = headers.contains_key(header::RANGE);
    if has_range {
        builder = builder.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    } else {
        builder = builder.status(StatusCode::OK);
    }

    builder.body(Body::from(chunk)).unwrap()
}

// ─── helpers ─────────────────────────────────────────────────────────

async fn resolve_parts_and_meta(
    req: &InstallStartRequest,
) -> Result<(Vec<PathBuf>, Vec<u64>, u64, PkgMetadata), String> {
    if let Some(p) = &req.split_root {
        let m: SplitPkgMetadata =
            parse_split_pkg(std::path::Path::new(p)).map_err(|e| format!("{e}"))?;
        Ok((m.parts, m.part_sizes, m.total_size, m.head))
    } else if let Some(p) = &req.path {
        let meta = parse_pkg(std::path::Path::new(p)).map_err(|e| format!("{e}"))?;
        let size = meta.size;
        Ok((vec![meta.path.clone()], vec![size], size, meta))
    } else {
        Err("either `path` or `split_root` is required".into())
    }
}

/// Map a Range request to (start, end) inclusive over the total size.
/// We support `bytes=N-M` and `bytes=N-` only — Sony BGFT only sends
/// those forms in practice.
fn parse_range_header(headers: &HeaderMap, total: u64) -> Result<(u64, u64), ()> {
    let h = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => return Ok((0, total.saturating_sub(1))),
    };
    let after = h.strip_prefix("bytes=").ok_or(())?;
    let (s, e) = after.split_once('-').ok_or(())?;
    let start: u64 = s.parse().map_err(|_| ())?;
    let end: u64 = if e.is_empty() {
        total.saturating_sub(1)
    } else {
        e.parse().map_err(|_| ())?
    };
    if start > end || end >= total {
        return Err(());
    }
    Ok((start, end))
}

/// Read a byte range `[start, end]` (inclusive) from the split-pkg
/// part list, crossing part boundaries as needed.
fn read_split_range(s: &InstallSession, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity((end - start + 1) as usize);
    let mut cursor = start;

    // Find the part containing `cursor` and stream until we've covered
    // the requested range. Read in capped chunks to avoid huge buffers
    // on a single call.
    let mut prefix = 0u64;
    for (i, part_size) in s.part_sizes.iter().enumerate() {
        let part_end = prefix + part_size;
        if cursor < part_end {
            let local_start = cursor - prefix;
            let want_end_global = end.min(part_end - 1);
            let local_end = want_end_global - prefix;
            let take = local_end - local_start + 1;

            let mut f = std::fs::File::open(&s.parts[i])?;
            f.seek(SeekFrom::Start(local_start))?;
            let mut chunk = vec![0u8; take as usize];
            f.read_exact(&mut chunk)?;
            out.extend_from_slice(&chunk);

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
pub fn lan_ip_for_ps5(ps5_host: &str) -> std::io::Result<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    // UDP "connect" doesn't actually send anything — it just sets the
    // peer for routing-table lookup, so local_addr() returns the IP
    // the OS would use. Port number is arbitrary.
    sock.connect(format!("{ps5_host}:1"))?;
    Ok(sock.local_addr()?.ip())
}

fn json_ok<T: Serialize>(v: &T) -> Response<Body> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn json_err(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": msg }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn plain_response(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(msg.to_string()))
        .unwrap()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ps5_mgmt_addr: "127.0.0.1:0".into(),
            task_id: None,
            err_code: 0,
            detail: String::new(),
            cancelled: false,
            created_at_unix: 0,
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
}
