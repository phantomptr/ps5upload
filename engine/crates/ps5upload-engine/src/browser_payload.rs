//! `POST /api/payload/send`: send a payload to a console's loader, the one way the desktop app
//! does it (`payload_lifecycle::send_payload_image`, Payload Manager fallback included). Every send
//! to a loader the user starts asks here first: the loader serves one connection at a time, an
//! install is using it, and a helper image replaces the running helper.
//!
//! Two forms. Bytes: `?host&port`, the file as the body, for a browser that has the file and
//! no socket. JSON: `{host, port?, path | payload_id, release?}`, for a file on the engine's
//! machine or a payload of the library (its local copy only; nothing is ever downloaded).
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use ps5upload_core::payload_lifecycle::{
    image_is_helper, is_ps5upload_payload, send_payload_file, send_payload_image, SentPayload,
    PS5_LOADER_PORT,
};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

use crate::helper_send::{GateError, SendOpts};
use crate::pkg_install::PkgInstallStateHandle;

/// The name a browser upload is sent under (it has no file name here).
const UPLOAD_LABEL: &str = "upload";

/// How long a send waits out an elfldr swap or an install's daemon send before it says so.
const LOADER_WAIT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct Target {
    #[serde(default, alias = "console", alias = "addr")]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    /// A helper send's options (helper_send::SendOpts), as query parameters.
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    replace: Option<bool>,
    #[serde(default)]
    app_version: Option<String>,
}

impl Target {
    fn opts(&self) -> SendOpts {
        SendOpts {
            mode: self.mode.clone(),
            force: self.force.unwrap_or(false),
            replace: self.replace.unwrap_or(false),
            app_version: self.app_version.clone(),
        }
    }
}

/// The JSON form's body.
#[derive(Deserialize)]
struct SendRequest {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    #[serde(default)]
    port: Option<u16>,
    /// A file on the engine's machine.
    #[serde(default)]
    path: Option<String>,
    /// A payload of the library.
    #[serde(default)]
    payload_id: Option<String>,
    /// A payload stored on the console itself (`/data/...`, a .elf or .bin): read back through the
    /// helper, then sent like `path` (#900).
    #[serde(default)]
    console_path: Option<String>,
    /// With `payload_id`: this version of the local copy. Absent: whatever version is here.
    #[serde(default)]
    release: Option<String>,
    /// The helper this engine carries: alone, it is what is sent; with `path`, it is sent when
    /// that file cannot be read or is not an ELF (a damaged copy of the app's own helper).
    #[serde(default)]
    bundled: bool,
    /// A helper send's options (mode, force, replace, app_version).
    #[serde(flatten)]
    opts: SendOpts,
}

/// What a loader takes (`payload_probe`'s list). The `path` form reads any file the engine can,
/// and a port other than :9021 has no ELF check, so without this it would stream any file on this
/// machine to any address.
const PAYLOAD_EXTENSIONS: &[&str] = &["elf", "bin", "js", "lua", "jar"];

#[derive(Deserialize)]
struct CheckBody {
    host: String,
    /// The file is a ps5upload helper: sending it stops the helper that runs now.
    #[serde(default)]
    helper: bool,
}

pub fn router<S: Clone + Send + Sync + 'static>(installs: PkgInstallStateHandle) -> Router<S> {
    let for_send = installs.clone();
    Router::new()
        .route(
            "/api/payload/send",
            post(
                move |target: Query<Target>, headers: HeaderMap, bytes: Bytes| {
                    send(for_send.clone(), target, headers, bytes)
                },
            )
            .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/api/payload/send-check",
            post(move |Json(body): Json<CheckBody>| check(installs.clone(), body)),
        )
        .route("/api/payload/bundled", axum::routing::get(bundled))
}

/// `GET /api/payload/bundled`: the helper this engine sends, `{available, version, app_version,
/// matches}`. `version` is null for a helper without a version marker (built before 12.6.15) or
/// no helper at all; `matches` is false when it is another version than the engine (a stale build:
/// nothing is sent unasked then), null when not known.
async fn bundled() -> Response {
    let r = tokio::task::spawn_blocking(|| {
        let available =
            crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Payload).is_ok();
        let version = crate::bundled_payload::helper_version();
        let matches = crate::bundled_payload::helper_matches_app();
        if matches == Some(false) {
            crate::log_error!(
                "bundled helper is v{} but this engine is v{}: the helper is never sent by itself",
                version.as_deref().unwrap_or("?"),
                env!("CARGO_PKG_VERSION")
            );
        }
        json!({
            "available": available,
            "version": version,
            "app_version": env!("CARGO_PKG_VERSION"),
            "matches": matches,
        })
    })
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// The reply for a send through the helper gate (helper_send): 200 as `sent_response`, 409 with
/// the refusal's `{error: code, message, forceable, retry_after_ms}`, 502 for a failed send.
fn gated_response(result: Result<Result<SentPayload, GateError>, String>) -> Response {
    match result {
        Ok(Err(GateError::Refused(r))) => {
            crate::log_info!("helper send refused: {} ({})", r.code, r.message);
            (StatusCode::CONFLICT, Json(r.fields())).into_response()
        }
        Ok(Err(GateError::Failed(e))) => sent_response(Ok(Err(e))),
        Ok(Ok(sent)) => sent_response(Ok(Ok(sent))),
        Err(e) => sent_response(Err(e)),
    }
}

/// A helper image to `host`'s loader through the gate (blocking work on its own thread).
async fn send_helper_gated(
    installs: PkgInstallStateHandle,
    host: String,
    label: String,
    image: Vec<u8>,
    opts: SendOpts,
) -> Response {
    let result = tokio::task::spawn_blocking(move || {
        if let Some(m) = away_refusal(&host) {
            return Err(GateError::Failed(m));
        }
        crate::helper_send::send_helper(&installs, &host, &label, &image, &opts, &mut stamp_helper)
    })
    .await
    .map_err(|e| e.to_string());
    gated_response(result)
}

/// Why a send to this console's loader must not go now, or None.
fn refusal(installs: &PkgInstallStateHandle, host: &str, helper: bool) -> Option<&'static str> {
    // A payload sent into an install can take the loader (and the daemon's bring-up) from under it.
    if installs.jobs.active_for(host) {
        return Some("pkg_install_busy");
    }
    // A transfer or file operation stops with the helper.
    if helper && crate::helper_stop::is_busy(host) {
        return Some("helper_busy");
    }
    None
}

/// Waits until nobody in this engine is using the console's loader; false when it stayed in use.
fn loader_settled(host: &str, wait: Duration) -> bool {
    let lock = crate::elfldr_guard::loader_lock(host);
    let until = Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(_) | Err(std::sync::TryLockError::Poisoned(_)) => return true,
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A console reached through Tailscale now: a loader port is not on its tailnet allow-list, so a
/// send would only time out. Blocking (the router may probe home once).
fn away_refusal(host: &str) -> Option<String> {
    ps5upload_core::route::via(host)
        .is_tailnet()
        .then(|| ps5upload_core::route::home_only("Sending a payload"))
}

fn refused(code: &str) -> Response {
    (StatusCode::CONFLICT, Json(json!({ "error": code }))).into_response()
}

/// `POST /api/payload/send-check {host, helper}`: for a send this engine does not make itself (the
/// desktop app's own socket): 200 once it can go, 409 `{error}` (`pkg_install_busy`, `helper_busy`,
/// `loader_busy`) when it should not.
async fn check(installs: PkgInstallStateHandle, body: CheckBody) -> Response {
    let host = crate::install::console_id(body.host.trim());
    if host.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Choose a console."})),
        )
            .into_response();
    }
    if let Some(code) = refusal(&installs, &host, body.helper) {
        return refused(code);
    }
    let away_host = host.clone();
    if let Ok(Some(message)) = tokio::task::spawn_blocking(move || away_refusal(&away_host)).await {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "tailnet_away", "message": message })),
        )
            .into_response();
    }
    let h = host.clone();
    match tokio::task::spawn_blocking(move || loader_settled(&h, LOADER_WAIT)).await {
        Ok(true) => match refusal(&installs, &host, body.helper) {
            // It may have been an install that held the loader.
            Some(code) => refused(code),
            None => Json(json!({ "ok": true })).into_response(),
        },
        Ok(false) => refused("loader_busy"),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}

/// A payload file, read whole (the 128 MiB cap is the send's own).
fn read_payload_file(path: &str) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("open {path}: {e}"))?
        .len();
    if size > 128 * 1024 * 1024 {
        return Err(format!("payload is too large ({size} bytes)"));
    }
    std::fs::read(path).map_err(|e| format!("read {path}: {e}"))
}

/// The helper to send for `{bundled: true}`: the file at `given` when it reads as an ELF, else
/// this engine's own helper (the desktop app's extracted copy may be damaged or gone).
fn bundled_or_given(given: Option<&str>) -> Result<(String, Vec<u8>), String> {
    // Only an .elf, as the path form takes it: never any file on this machine.
    let given = given.map(str::trim).filter(|p| {
        std::path::Path::new(p)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("elf"))
    });
    if let Some(p) = given {
        match read_payload_file(p) {
            Ok(b) if b.starts_with(b"\x7fELF") => return Ok((p.to_string(), b)),
            Ok(_) => {
                crate::log_warn!("helper send: {p} is not an ELF; sending the built-in helper")
            }
            Err(e) => crate::log_warn!("helper send: {e}; sending the built-in helper"),
        }
    }
    let image = crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Payload)?;
    Ok(("ps5upload.elf".to_string(), image.into_owned()))
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn stamp_helper(bytes: &mut Vec<u8>) {
    *bytes = crate::ava1_api::stamped_helper(bytes);
}

/// Wait out an elfldr swap (or an install's daemon send) rather than send into its gap. Another
/// port is another program's: none of this is about it.
fn with_loader_lock<T>(host: &str, port: u16, f: impl FnOnce() -> T) -> T {
    let lock = crate::elfldr_guard::loader_lock(&crate::install::console_id(host));
    let _loader = (port == PS5_LOADER_PORT).then(|| lock.lock().unwrap_or_else(|e| e.into_inner()));
    f()
}

/// The refusal for a send to `port` on `host`, or None.
fn send_refused(
    installs: &PkgInstallStateHandle,
    host: &str,
    port: u16,
    helper: bool,
) -> Option<Response> {
    if port != PS5_LOADER_PORT {
        return None;
    }
    refusal(installs, &crate::install::console_id(host), helper).map(refused)
}

fn sent_response(result: Result<Result<SentPayload, String>, String>) -> Response {
    match result {
        Ok(Ok(sent)) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "bytes_sent": sent.bytes,
                "bytes": sent.bytes,
                "via_payload_manager": sent.via_payload_manager,
                "status": sent.status,
            })),
        )
            .into_response(),
        Ok(Err(error)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": error, "status": error })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": error })),
        )
            .into_response(),
    }
}

async fn send(
    installs: PkgInstallStateHandle,
    Query(target): Query<Target>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("json"));
    if is_json {
        return send_json(installs, &bytes).await;
    }
    let opts = target.opts();
    let host = target.host.unwrap_or_default().trim().to_string();
    let port = target.port.unwrap_or(PS5_LOADER_PORT);
    if host.is_empty() || port == 0 || !bytes.starts_with(b"\x7fELF") {
        return bad_request("Choose an ELF payload and a valid loader address and port.");
    }
    // As the send below decides it (it then stops the running helper): the AVA1 marker alone
    // missed a helper the send recognised by its name string, and the refusal that keeps a
    // running transfer from being cut was skipped.
    let helper = image_is_helper(UPLOAD_LABEL, &bytes) || crate::console_payload::is_helper(&bytes);
    if helper && port == PS5_LOADER_PORT {
        return send_helper_gated(installs, host, UPLOAD_LABEL.into(), bytes.to_vec(), opts).await;
    }
    if let Some(refused) = send_refused(&installs, &host, port, helper) {
        return refused;
    }
    let result = tokio::task::spawn_blocking(move || {
        if let Some(m) = away_refusal(&host) {
            return Err(m);
        }
        with_loader_lock(&host, port, || {
            send_payload_image(&host, port, UPLOAD_LABEL, &bytes, &mut stamp_helper)
        })
    })
    .await
    .map_err(|e| e.to_string());
    sent_response(result)
}

async fn send_json(installs: PkgInstallStateHandle, body: &[u8]) -> Response {
    let req: SendRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return bad_request(&format!("bad request: {e}")),
    };
    let host = req.host.trim().to_string();
    let port = req.port.unwrap_or(PS5_LOADER_PORT);
    if host.is_empty() || port == 0 {
        return bad_request("host and a valid port are required");
    }
    let opts = req.opts.clone();
    // This engine's own helper: alone, or in place of a copy that cannot be read.
    if req.bundled && port == PS5_LOADER_PORT {
        let given = req.path.clone();
        let image = tokio::task::spawn_blocking(move || bundled_or_given(given.as_deref())).await;
        return match image {
            Ok(Ok((label, image))) => send_helper_gated(installs, host, label, image, opts).await,
            Ok(Err(e)) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": e, "status": e })),
            )
                .into_response(),
            Err(e) => sent_response(Err(e.to_string())),
        };
    }
    if let Some(on_console) = req.console_path.as_deref().map(str::trim) {
        if req.path.is_some() || req.payload_id.is_some() {
            return bad_request("give one of path, payload_id or console_path");
        }
        return send_console_file(installs, host, port, on_console.to_string()).await;
    }
    let path = match (req.path, req.payload_id) {
        (Some(path), None) => {
            let ext_ok = std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .is_some_and(|e| PAYLOAD_EXTENSIONS.contains(&e.as_str()));
            if !ext_ok {
                return bad_request("path must be a .elf, .bin, .js, .lua or .jar file");
            }
            path
        }
        (None, Some(id)) => {
            let Some(lib) = crate::payloads_api::library() else {
                return crate::payloads_api::no_library();
            };
            let release = req.release;
            let cached =
                tokio::task::spawn_blocking(move || lib.local_copy(&id, release.as_deref())).await;
            match cached {
                Ok(Ok(p)) => p.to_string_lossy().into_owned(),
                Ok(Err(e)) => return crate::payloads_api::library_error(&e),
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": e.to_string() })),
                    )
                        .into_response()
                }
            }
        }
        _ => return bad_request("give either path or payload_id"),
    };
    // A file that cannot be read says so when the send opens it.
    let probed = path.clone();
    let helper = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut head = Vec::new();
        std::fs::File::open(&probed)
            .and_then(|f| f.take(512 * 1024).read_to_end(&mut head))
            .is_ok()
            && is_ps5upload_payload(&probed, &head)
    })
    .await
    .unwrap_or(false);
    if helper && port == PS5_LOADER_PORT {
        // Every helper send goes through the gate (helper_send), the desktop app's included.
        let read = path.clone();
        let image = tokio::task::spawn_blocking(move || read_payload_file(&read)).await;
        return match image {
            Ok(Ok(image)) => send_helper_gated(installs, host, path, image, opts).await,
            Ok(Err(e)) => sent_response(Ok(Err(e))),
            Err(e) => sent_response(Err(e.to_string())),
        };
    }
    if let Some(refused) = send_refused(&installs, &host, port, helper) {
        return refused;
    }
    let result = tokio::task::spawn_blocking(move || {
        if let Some(m) = away_refusal(&host) {
            return Err(m);
        }
        with_loader_lock(&host, port, || {
            send_payload_file(&host, &path, port, &mut stamp_helper)
        })
    })
    .await
    .map_err(|e| e.to_string());
    sent_response(result)
}

/// Why a payload read from the console cannot be sent, in words for the person who chose it.
fn console_file_problem(path: &str, code: &str) -> String {
    match code {
        "payload_not_elf" => format!("{path} on the PS5 is not an ELF payload."),
        "payload_is_helper" => format!(
            "{path} on the PS5 is ps5upload's helper. Send the helper from Connection instead."
        ),
        "payload_too_large" => format!(
            "{path} on the PS5 is larger than a loader takes ({} MiB).",
            crate::console_payload::PAYLOAD_MAX / (1024 * 1024)
        ),
        other => format!("Couldn't read {path} on the PS5: {other}"),
    }
}

/// The JSON form's `console_path`: a payload on the console, read back through the helper and sent
/// like a file on this machine, under the same refusals, loader lock and ELF check.
async fn send_console_file(
    installs: PkgInstallStateHandle,
    host: String,
    port: u16,
    path: String,
) -> Response {
    if !path.starts_with('/') || !crate::console_payload::is_payload_name(&path) {
        return bad_request("console_path must be a .elf or .bin file on the PS5");
    }
    if let Some(refused) = send_refused(&installs, &host, port, false) {
        return refused;
    }
    let result = tokio::task::spawn_blocking(move || {
        if let Some(m) = away_refusal(&host) {
            return Err(m);
        }
        let mgmt =
            crate::pkg_install::normalize_mgmt_addr(&crate::install::console_id(host.trim()));
        let bytes = crate::console_payload::read_console_file(&mgmt, &path).map_err(|e| {
            let code = e.strip_prefix(&format!("read {path}: ")).unwrap_or(&e);
            console_file_problem(&path, code)
        })?;
        if let Some(code) = crate::console_payload::refusal(&bytes) {
            return Err(console_file_problem(&path, code));
        }
        with_loader_lock(&host, port, || {
            send_payload_image(&host, port, &path, &bytes, &mut stamp_helper)
        })
    })
    .await
    .map_err(|e| e.to_string());
    sent_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn octet() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            "application/octet-stream".parse().unwrap(),
        );
        h
    }

    fn json_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        h
    }

    fn listen_once() -> (u16, std::thread::JoinHandle<Vec<u8>>) {
        let loader = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = loader.local_addr().unwrap().port();
        let received = std::thread::spawn(move || {
            let (mut socket, _) = loader.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            socket.read_to_end(&mut data).unwrap();
            data
        });
        (port, received)
    }

    fn query(port: Option<u16>) -> Query<Target> {
        Query(Target {
            host: Some("127.0.0.1".into()),
            port,
            mode: None,
            force: None,
            replace: None,
            app_version: None,
        })
    }

    async fn body_json(r: Response) -> serde_json::Value {
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    #[tokio::test]
    async fn sends_the_exact_image_and_closes_the_loader_write_side() {
        let (port, received) = listen_once();
        let image = Bytes::from_static(b"\x7fELFtest payload");
        let response = send(
            Default::default(),
            query(Some(port)),
            octet(),
            image.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let v = body_json(response).await;
        assert_eq!(v["bytes_sent"], image.len());
        assert_eq!(v["via_payload_manager"], false);
        assert_eq!(received.join().unwrap(), image);
    }

    #[tokio::test]
    async fn rejects_a_non_elf_before_connecting() {
        let response = send(
            Default::default(),
            query(Some(1)),
            octet(),
            Bytes::from_static(b"not an ELF"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_json_form_sends_a_file_from_a_path() {
        let dir = std::env::temp_dir().join(format!("ps5upload-send-api-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("thing.elf");
        std::fs::write(&file, b"\x7fELFfrom a path").unwrap();
        let (port, received) = listen_once();
        let body = json!({"host": "127.0.0.1", "port": port, "path": file}).to_string();
        let response = send(
            Default::default(),
            query(None),
            json_headers(),
            Bytes::from(body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(received.join().unwrap(), b"\x7fELFfrom a path");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_json_form_accepts_the_other_names_for_the_console() {
        let (port, received) = listen_once();
        let dir = std::env::temp_dir().join(format!("ps5upload-send-api2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        let body = json!({"console": "127.0.0.1", "port": port, "path": file}).to_string();
        let response = send(
            Default::default(),
            query(None),
            json_headers(),
            Bytes::from(body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(received.join().unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_path_form_will_not_stream_any_file_on_this_machine() {
        for path in ["/etc/passwd", "/home/me/.ssh/id_rsa", "noext"] {
            let body = json!({"host": "127.0.0.1", "port": 1, "path": path}).to_string();
            let r = send(
                Default::default(),
                query(None),
                json_headers(),
                Bytes::from(body),
            )
            .await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{path}");
        }
    }

    #[tokio::test]
    async fn the_json_form_wants_exactly_one_source() {
        for body in [
            json!({"host": "127.0.0.1"}),
            json!({"host": "127.0.0.1", "path": "/a.elf", "payload_id": "x"}),
            json!({"host": " ", "path": "/a.elf"}),
        ] {
            let r = send(
                Default::default(),
                query(None),
                json_headers(),
                Bytes::from(body.to_string()),
            )
            .await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{body}");
        }
    }

    #[tokio::test]
    async fn a_console_path_is_alone_and_a_payload_file() {
        for body in [
            json!({"host": "127.0.0.1", "console_path": "/data/k.elf", "path": "/a.elf"}),
            json!({"host": "127.0.0.1", "console_path": "/data/k.elf", "payload_id": "x"}),
            json!({"host": "127.0.0.1", "console_path": "/data/k.pkg"}),
            json!({"host": "127.0.0.1", "console_path": "data/k.elf"}),
        ] {
            let r = send(
                Default::default(),
                query(None),
                json_headers(),
                Bytes::from(body.to_string()),
            )
            .await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{body}");
        }
    }

    /// A console whose helper does not answer: the step fails with the file's name and why.
    #[tokio::test]
    async fn an_unreadable_console_path_says_so() {
        let r = send(
            Default::default(),
            query(None),
            json_headers(),
            Bytes::from(json!({"host": "127.0.0.1", "console_path": "/data/k.elf"}).to_string()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        let v = body_json(r).await;
        let e = v["error"].as_str().unwrap();
        assert!(e.starts_with("Couldn't read /data/k.elf on the PS5"), "{v}");
    }

    #[test]
    fn console_file_problems_name_the_file() {
        assert_eq!(
            console_file_problem("/data/x.elf", "payload_not_elf"),
            "/data/x.elf on the PS5 is not an ELF payload."
        );
        assert!(console_file_problem("/data/x.elf", "payload_too_large").contains("64 MiB"));
        assert!(console_file_problem("/data/p.elf", "payload_is_helper").contains("helper"));
    }

    #[tokio::test]
    async fn a_dead_custom_port_is_a_bad_gateway_with_the_reason() {
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let r = send(
            Default::default(),
            query(Some(closed)),
            octet(),
            Bytes::from_static(b"\x7fELFabc"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        let v = body_json(r).await;
        assert!(v["error"].as_str().unwrap().starts_with("connect "), "{v}");
    }

    /// A helper image while this engine runs a job there, and anything during an install, wait.
    #[tokio::test]
    async fn a_send_into_a_busy_console_is_refused_and_says_why() {
        let installs: PkgInstallStateHandle = Default::default();
        let host = "192.0.2.77";
        assert_eq!(refusal(&installs, host, true), None);
        let busy = crate::helper_stop::enter(host);
        assert_eq!(refusal(&installs, host, true), Some("helper_busy"));
        assert_eq!(
            refusal(&installs, host, false),
            None,
            "a companion runs beside it"
        );
        drop(busy);
        let response = check(
            installs.clone(),
            CheckBody {
                host: host.into(),
                helper: false,
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// A helper sent as bytes from the browser is known as the send knows it (its name string),
    /// not only by the AVA1 marker: a running transfer then refuses it.
    #[tokio::test]
    async fn a_browser_sent_helper_waits_for_a_running_transfer() {
        let busy = crate::helper_stop::enter("127.0.0.1");
        let image = Bytes::from_static(b"\x7fELF\x02\x01\x01 ps5upload helper build");
        let r = send(
            Default::default(),
            query(Some(PS5_LOADER_PORT)),
            octet(),
            image,
        )
        .await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(r).await["error"], "helper_busy");
        drop(busy);
    }

    #[test]
    fn a_loader_in_use_is_waited_for_then_reported() {
        let host = "192.0.2.78";
        let lock = crate::elfldr_guard::loader_lock(host);
        let held = lock.lock().unwrap();
        assert!(!loader_settled(host, Duration::from_millis(50)));
        drop(held);
        assert!(loader_settled(host, Duration::from_millis(50)));
    }

    /// `{bundled: true, path}`: the app's extracted helper when it reads as an ELF, this engine's
    /// own one in place of a damaged copy, and never a file that is not named .elf.
    #[test]
    fn the_bundled_form_takes_a_good_copy_and_replaces_a_damaged_one() {
        let dir = std::env::temp_dir().join(format!("ps5upload-bundled-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("ps5upload.elf");
        std::fs::write(&good, b"\x7fELF the app's helper").unwrap();
        let (label, bytes) = bundled_or_given(good.to_str()).unwrap();
        assert_eq!(label, good.to_string_lossy());
        assert_eq!(bytes, b"\x7fELF the app's helper");
        for (name, body) in [
            ("damaged.elf", &b"not an elf"[..]),
            ("helper.txt", b"\x7fELF but named .txt"),
        ] {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            // The built-in helper when this build has one, else the reason it has none: never
            // the file given.
            match bundled_or_given(p.to_str()) {
                Ok((label, _)) => assert_eq!(label, "ps5upload.elf", "{name}"),
                Err(e) => assert!(e.contains("no ps5upload payload"), "{name}: {e}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A helper sent as bytes goes through the gate: a person's send right after another one is
    /// refused with the code, and may go anyway.
    #[tokio::test]
    async fn a_second_helper_right_after_one_is_refused_until_forced() {
        let host = "192.0.2.93";
        crate::helper_send::tests_note_sent(host);
        let image = Bytes::from_static(b"\x7fELF..PS5UPLOAD_HELPER_VERSION=12.6.14;..");
        let mut q = query(Some(PS5_LOADER_PORT));
        q.0.host = Some(host.into());
        let r = send(Default::default(), q, octet(), image).await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        let v = body_json(r).await;
        assert_eq!(v["error"], "recent_send");
        assert_eq!(v["forceable"], true);
    }
}
