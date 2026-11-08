//! `POST /api/payload/send`: send a payload to a console's loader, the one way the desktop app
//! does it (`payload_lifecycle::send_payload_image`, Payload Manager fallback included). Every send
//! to a loader the user starts asks here first: the loader serves one connection at a time, an
//! install is using it, and a helper image replaces the running helper.
//!
//! Two forms. Bytes: `?host&port`, the file as the body, for a browser that has the file and
//! no socket. JSON: `{host, port?, path | payload_id, release?}`, for a file on the engine's
//! machine or a payload of the library (downloaded first when it isn't cached).
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use ps5upload_core::payload_lifecycle::{
    is_ps5upload_payload, send_payload_file, send_payload_image, SentPayload, PS5_LOADER_PORT,
};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

use crate::pkg_install::PkgInstallStateHandle;

/// How long a send waits out an elfldr swap or an install's daemon send before it says so.
const LOADER_WAIT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct Target {
    #[serde(default, alias = "console", alias = "addr")]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
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
    /// With `payload_id`: this release tag. Absent: what is cached, else the latest.
    #[serde(default)]
    release: Option<String>,
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
    let host = target.host.unwrap_or_default().trim().to_string();
    let port = target.port.unwrap_or(PS5_LOADER_PORT);
    if host.is_empty() || port == 0 || !bytes.starts_with(b"\x7fELF") {
        return bad_request("Choose an ELF payload and a valid loader address and port.");
    }
    let helper = crate::console_payload::is_helper(&bytes);
    if let Some(refused) = send_refused(&installs, &host, port, helper) {
        return refused;
    }
    let result = tokio::task::spawn_blocking(move || {
        if let Some(m) = away_refusal(&host) {
            return Err(m);
        }
        with_loader_lock(&host, port, || {
            send_payload_image(&host, port, "upload", &bytes, &mut stamp_helper)
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
                tokio::task::spawn_blocking(move || lib.ensure_cached(&id, release.as_deref()))
                    .await;
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

    #[test]
    fn a_loader_in_use_is_waited_for_then_reported() {
        let host = "192.0.2.78";
        let lock = crate::elfldr_guard::loader_lock(host);
        let held = lock.lock().unwrap();
        assert!(!loader_settled(host, Duration::from_millis(50)));
        drop(held);
        assert!(loader_settled(host, Duration::from_millis(50)));
    }
}
