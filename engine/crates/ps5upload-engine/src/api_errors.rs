//! One error shape for `/api/*`: every failure a client can get is JSON with an `error` text.
//!
//! Handlers already answer `{"error": ...}` through `json_err`; what they cannot reach are axum's
//! own refusals (a malformed body, a missing query parameter, a wrong method, an unknown route)
//! and the plain-text guards. The outermost layer turns those into
//! `{"error": "<text>", "code": "<snake_case from the status>"}` and keeps the status. A response
//! that is already JSON passes untouched, so the per-route extras (`reason`, `field`, ...) stay.
//!
//! The same layer works out the path without the `PS5UPLOAD_BASE_URL` prefix once, for the guards
//! and the request log inside it.

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// An error body longer than this is not a message.
const MAX_ERROR_TEXT: usize = 64 * 1024;

/// The request's path with the `PS5UPLOAD_BASE_URL` prefix removed.
#[derive(Clone, Debug)]
pub(crate) struct ApiPath(pub String);

/// `/ps5/api/jobs` under the prefix `/ps5` is `/api/jobs`. A path outside the prefix stays as it
/// is (`/pkg-host/...` is served at the root whatever the prefix).
pub(crate) fn strip_base<'a>(path: &'a str, base: &str) -> &'a str {
    if base.is_empty() {
        return path;
    }
    match path.strip_prefix(base) {
        Some("") => "/",
        Some(rest) if rest.starts_with('/') => rest,
        _ => path,
    }
}

pub(crate) fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// The request's path for matching: without the prefix, when the outer layer has run.
pub(crate) fn request_path(req: &Request) -> String {
    match req.extensions().get::<ApiPath>() {
        Some(p) => p.0.clone(),
        None => req.uri().path().to_string(),
    }
}

/// `404 Not Found` → `not_found`.
pub(crate) fn code_for_status(status: StatusCode) -> String {
    match status.canonical_reason() {
        Some(reason) => reason
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect(),
        None => format!("http_{}", status.as_u16()),
    }
}

/// The JSON error body for a status and a text (the status's own words when the text is empty).
pub(crate) fn error_body(status: StatusCode, text: &str) -> serde_json::Value {
    let text = text.trim();
    let error = if text.is_empty() {
        status.canonical_reason().unwrap_or("request failed")
    } else {
        text
    };
    serde_json::json!({ "error": error, "code": code_for_status(status) })
}

/// The `/api/*` answer for a route that does not exist.
pub(crate) fn no_such_route(path: &str) -> Response {
    let status = StatusCode::NOT_FOUND;
    (
        status,
        axum::Json(error_body(status, &format!("no such route: {path}"))),
    )
        .into_response()
}

/// The router's fallback when the web UI is not compiled in (with it, `webui::spa_fallback`).
#[cfg(not(feature = "webui"))]
pub(crate) async fn fallback(req: Request) -> Response {
    let path = req.uri().path();
    if is_api_path(path) {
        no_such_route(path)
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

fn is_json(content_type: Option<&HeaderValue>) -> bool {
    content_type
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("json"))
}

fn is_text(content_type: Option<&HeaderValue>) -> bool {
    match content_type.and_then(|v| v.to_str().ok()) {
        None => true,
        Some(v) => v.to_ascii_lowercase().starts_with("text/"),
    }
}

/// A 4xx/5xx that is not JSON becomes `{error, code}` with the same status and other headers.
/// Everything else (2xx, the 101 of a socket, a 304, a redirect) is not an error body.
pub(crate) async fn normalize(resp: Response) -> Response {
    let status = resp.status();
    if !(status.is_client_error() || status.is_server_error())
        || is_json(resp.headers().get(header::CONTENT_TYPE))
    {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let text = if is_text(parts.headers.get(header::CONTENT_TYPE)) {
        match to_bytes(body, MAX_ERROR_TEXT).await {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_ENCODING);
    parts.headers.remove(header::TRANSFER_ENCODING);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(error_body(status, &text).to_string()))
}

/// The outermost layer: see the module comment.
pub(crate) async fn api_surface(
    State(base): State<std::sync::Arc<str>>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = strip_base(req.uri().path(), &base).to_string();
    let api = is_api_path(&path);
    req.extensions_mut().insert(ApiPath(path));
    let resp = next.run(req).await;
    if api {
        normalize(resp).await
    } else {
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_comes_off_only_at_a_path_boundary() {
        assert_eq!(strip_base("/ps5/api/jobs", "/ps5"), "/api/jobs");
        assert_eq!(strip_base("/ps5", "/ps5"), "/");
        assert_eq!(strip_base("/ps5/", "/ps5"), "/");
        assert_eq!(strip_base("/ps55/api", "/ps5"), "/ps55/api");
        assert_eq!(strip_base("/pkg-host/a/b", "/ps5"), "/pkg-host/a/b");
        assert_eq!(strip_base("/api/jobs", ""), "/api/jobs");
    }

    #[test]
    fn codes_come_from_the_status() {
        for (status, code) in [
            (StatusCode::BAD_REQUEST, "bad_request"),
            (StatusCode::FORBIDDEN, "forbidden"),
            (StatusCode::NOT_FOUND, "not_found"),
            (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"),
            (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable_entity"),
            (StatusCode::BAD_GATEWAY, "bad_gateway"),
        ] {
            assert_eq!(code_for_status(status), code);
        }
        assert_eq!(
            code_for_status(StatusCode::from_u16(599).unwrap()),
            "http_599"
        );
    }

    async fn body_json(r: Response) -> serde_json::Value {
        let b = to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    #[tokio::test]
    async fn plain_text_errors_become_json_and_keep_status_and_headers() {
        let r = (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "5")],
            "not now",
        )
            .into_response();
        let r = normalize(r).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers()[header::RETRY_AFTER], "5");
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
        let v = body_json(r).await;
        assert_eq!(v["error"], "not now");
        assert_eq!(v["code"], "service_unavailable");
    }

    #[tokio::test]
    async fn an_empty_error_says_what_its_status_means() {
        let v = body_json(normalize(StatusCode::METHOD_NOT_ALLOWED.into_response()).await).await;
        assert_eq!(v["error"], "Method Not Allowed");
        assert_eq!(v["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn json_errors_and_non_errors_pass_untouched() {
        let r = normalize(
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({"error": "x", "reason": "y"})),
            )
                .into_response(),
        )
        .await;
        let v = body_json(r).await;
        assert_eq!(v, serde_json::json!({"error": "x", "reason": "y"}));

        for status in [
            StatusCode::OK,
            StatusCode::NOT_MODIFIED,
            StatusCode::SWITCHING_PROTOCOLS,
            StatusCode::FOUND,
        ] {
            let r = normalize((status, "raw").into_response()).await;
            assert_eq!(r.status(), status);
            assert!(!is_json(r.headers().get(header::CONTENT_TYPE)));
        }
    }

    #[tokio::test]
    async fn a_binary_or_oversized_error_body_is_replaced_by_the_status() {
        let v = body_json(
            normalize(
                (
                    StatusCode::NOT_FOUND,
                    [(header::CONTENT_TYPE, "image/png")],
                    vec![0x89u8, 0x50],
                )
                    .into_response(),
            )
            .await,
        )
        .await;
        assert_eq!(v["error"], "Not Found");

        let big = "x".repeat(MAX_ERROR_TEXT + 1);
        let v = body_json(normalize((StatusCode::BAD_GATEWAY, big).into_response()).await).await;
        assert_eq!(v["error"], "Bad Gateway");
    }
}
