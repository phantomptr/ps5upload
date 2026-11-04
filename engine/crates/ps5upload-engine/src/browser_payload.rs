//! `POST /api/payload/send`: send a payload to a console's loader, the one way the desktop app
//! does it (`payload_lifecycle::send_payload_image`, Payload Manager fallback included).
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
    send_payload_file, send_payload_image, SentPayload, PS5_LOADER_PORT,
};
use serde::Deserialize;
use serde_json::json;

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

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route(
        "/api/payload/send",
        post(send).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
    )
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn stamp_helper(bytes: &mut Vec<u8>) {
    *bytes = crate::ava1_api::stamped_helper(bytes);
}

/// Wait out an elfldr swap (or an install's daemon send) rather than send into its gap.
fn with_loader_lock<T>(host: &str, f: impl FnOnce() -> T) -> T {
    let lock = crate::elfldr_guard::loader_lock(host);
    let _loader = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
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

async fn send(Query(target): Query<Target>, headers: HeaderMap, bytes: Bytes) -> Response {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("json"));
    if is_json {
        return send_json(&bytes).await;
    }
    let host = target.host.unwrap_or_default().trim().to_string();
    let port = target.port.unwrap_or(PS5_LOADER_PORT);
    if host.is_empty() || port == 0 || !bytes.starts_with(b"\x7fELF") {
        return bad_request("Choose an ELF payload and a valid loader address and port.");
    }
    let result = tokio::task::spawn_blocking(move || {
        with_loader_lock(&host, || {
            send_payload_image(&host, port, "upload", &bytes, &mut stamp_helper)
        })
    })
    .await
    .map_err(|e| e.to_string());
    sent_response(result)
}

async fn send_json(body: &[u8]) -> Response {
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
    let result = tokio::task::spawn_blocking(move || {
        with_loader_lock(&host, || {
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
        let response = send(query(Some(port)), octet(), image.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let v = body_json(response).await;
        assert_eq!(v["bytes_sent"], image.len());
        assert_eq!(v["via_payload_manager"], false);
        assert_eq!(received.join().unwrap(), image);
    }

    #[tokio::test]
    async fn rejects_a_non_elf_before_connecting() {
        let response = send(query(Some(1)), octet(), Bytes::from_static(b"not an ELF")).await;
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
        let response = send(query(None), json_headers(), Bytes::from(body)).await;
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
        let response = send(query(None), json_headers(), Bytes::from(body)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(received.join().unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_path_form_will_not_stream_any_file_on_this_machine() {
        for path in ["/etc/passwd", "/home/me/.ssh/id_rsa", "noext"] {
            let body = json!({"host": "127.0.0.1", "port": 1, "path": path}).to_string();
            let r = send(query(None), json_headers(), Bytes::from(body)).await;
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
            let r = send(query(None), json_headers(), Bytes::from(body.to_string())).await;
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
            query(Some(closed)),
            octet(),
            Bytes::from_static(b"\x7fELFabc"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        let v = body_json(r).await;
        assert!(v["error"].as_str().unwrap().starts_with("connect "), "{v}");
    }
}
