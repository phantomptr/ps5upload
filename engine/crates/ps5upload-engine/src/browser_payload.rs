//! Browser ELF forwarding uses the same loader sender as the desktop shell.
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Target {
    host: String,
    port: u16,
}

pub fn router() -> Router {
    Router::new().route(
        "/api/payload/send",
        post(send).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
    )
}

async fn send(Query(target): Query<Target>, bytes: Bytes) -> impl IntoResponse {
    if target.host.trim().is_empty() || target.port == 0 || !bytes.starts_with(b"\x7fELF") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Choose an ELF payload and a valid loader address and port."})),
        );
    }
    let result = tokio::task::spawn_blocking(move || {
        ps5upload_core::payload_lifecycle::send_elf_to_loader(
            target.host.trim(),
            target.port,
            &bytes,
            ps5upload_core::payload_lifecycle::LoaderImage::Companion,
        )
    })
    .await;
    match result {
        Ok(Ok(bytes_sent)) => (StatusCode::OK, Json(json!({"bytes_sent":bytes_sent}))),
        Ok(Err(error)) => (StatusCode::BAD_GATEWAY, Json(json!({"error":error}))),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        ),
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
            Query(Target {
                host: "127.0.0.1".into(),
                port,
            }),
            image.clone(),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(received.join().unwrap(), image);
    }

    #[tokio::test]
    async fn rejects_a_non_elf_before_connecting() {
        let response = send(
            Query(Target {
                host: "127.0.0.1".into(),
                port: 1,
            }),
            Bytes::from_static(b"not an ELF"),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
