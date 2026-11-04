//! Browser ELF forwarding uses the same loader sender as the desktop shell, and every send to a
//! console's loader the user starts asks here first: the loader serves one connection at a time,
//! an install is using it, and a helper image replaces the running helper.
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};

use crate::pkg_install::PkgInstallStateHandle;
use ps5upload_core::payload_lifecycle::PS5_LOADER_PORT;

/// How long a send waits out an elfldr swap or an install's daemon send before it says so.
const LOADER_WAIT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct Target {
    host: String,
    port: u16,
}

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
            post(move |Query(target): Query<Target>, bytes: Bytes| {
                send(for_send.clone(), target, bytes)
            })
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

async fn send(installs: PkgInstallStateHandle, target: Target, bytes: Bytes) -> Response {
    if target.host.trim().is_empty() || target.port == 0 || !bytes.starts_with(b"\x7fELF") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Choose an ELF payload and a valid loader address and port."})),
        )
            .into_response();
    }
    // Another port is another program's: none of this is about it.
    let to_loader = target.port == PS5_LOADER_PORT;
    let host = crate::install::console_id(target.host.trim());
    if to_loader {
        if let Some(code) = refusal(&installs, &host, crate::console_payload::is_helper(&bytes)) {
            return refused(code);
        }
    }
    let result = tokio::task::spawn_blocking(move || {
        // Waits out an elfldr swap or an install's daemon send rather than send into its gap.
        let lock = crate::elfldr_guard::loader_lock(&host);
        let _loader = to_loader.then(|| lock.lock().unwrap_or_else(|e| e.into_inner()));
        ps5upload_core::payload_lifecycle::send_elf_to_loader(
            target.host.trim(),
            target.port,
            &bytes,
            ps5upload_core::payload_lifecycle::LoaderImage::Companion,
        )
    })
    .await;
    match result {
        Ok(Ok(bytes_sent)) => {
            (StatusCode::OK, Json(json!({"bytes_sent":bytes_sent}))).into_response()
        }
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error":error}))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[tokio::test]
    async fn sends_the_exact_image_and_closes_the_loader_write_side() {
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
        let image = Bytes::from_static(b"\x7fELFtest payload");
        let response = send(
            Default::default(),
            Target {
                host: "127.0.0.1".into(),
                port,
            },
            image.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(received.join().unwrap(), image);
    }

    #[tokio::test]
    async fn rejects_a_non_elf_before_connecting() {
        let response = send(
            Default::default(),
            Target {
                host: "127.0.0.1".into(),
                port: 1,
            },
            Bytes::from_static(b"not an ELF"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
