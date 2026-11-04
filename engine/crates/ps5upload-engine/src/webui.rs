//! Embedded React SPA served when the `webui` Cargo feature is enabled.
//!
//! The `webui/` directory next to this crate's `Cargo.toml` must be
//! populated with the Vite output from `client/dist` **before** the Rust
//! build runs.  The Docker multi-stage build does this; for local dev copy
//! manually:
//!
//! ```sh
//! npm --prefix client run build:vite
//! cp -r client/dist/* engine/crates/ps5upload-engine/webui/
//! cargo build -p ps5upload-engine --features webui
//! ```
//!
//! In release builds every file is baked into the binary, so the final
//! `FROM scratch` Docker stage only needs the binary itself.  In debug
//! builds `rust-embed` reads from disk so the SPA can be hot-patched.

use axum::{
    extract::Request,
    http::header,
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// All files under `webui/` embedded at compile time.
/// The `#[folder]` path is relative to `CARGO_MANIFEST_DIR`.
#[derive(RustEmbed)]
#[folder = "webui/"]
struct WebAssets;

/// Build a response for a single embedded asset, or fall back to a 404.
/// Does NOT perform the SPA fallback to `index.html` — callers that want
/// SPA fallback should use `spa_fallback` or `spa_response`.
fn asset_response(path: &str) -> Option<Response> {
    let file = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    // `Cow::Borrowed` in a release build: the file goes out with no copy made per request.
    let body = axum::body::Body::from(file.data);
    Some(
        (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, cache_control_for(path)),
            ],
            body,
        )
            .into_response(),
    )
}

/// `assets/` names carry a hash of their content, so a copy never goes stale. Anything else
/// (`index.html` first of all) keeps its name across engine updates: ask each time.
fn cache_control_for(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// A request path as the bundle names files: `my%20font.woff2` -> `my font.woff2`. A path that
/// does not decode to text stays as it came, and is simply not found.
fn decode_path(path: &str) -> std::borrow::Cow<'_, str> {
    if !path.contains('%') {
        return path.into();
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_or(path.into(), Into::into)
}

/// A missing file, not a page of the app: the client's routes have no extension. The SPA shell
/// with a 200 in place of a font or a script is what a browser then fails on.
fn looks_like_a_file(path: &str) -> bool {
    mime_guess::from_path(path).first().is_some()
}

/// Return a response for `path` inside the embedded bundle.  If the path is
/// not found, serve `index.html` instead (SPA client-side-routing fallback).
pub fn spa_response(path: &str) -> Response {
    let found = (path != "index.html")
        .then(|| asset_response(path))
        .flatten();
    found.unwrap_or_else(|| {
        // Serve index.html as the SPA shell for any path the router doesn't
        // recognise.  The React app's client-side router takes over from here.
        match WebAssets::get("index.html") {
            Some(index) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, cache_control_for("index.html")),
                ],
                crate::web_pages::page_shell(&index.data, crate::host_guard::base_path()),
            )
                .into_response(),
            // index.html is embedded at build time when the webui feature is
            // enabled. If we hit this path the binary was misbuilt (webui
            // feature on but no client build output). Return a clear error
            // instead of panicking so the engine stays up for API use.
            None => (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                axum::body::Body::from(
                    "webui index.html not found — rebuild with the client assets bundled.",
                ),
            )
                .into_response(),
        }
    })
}

/// Axum fallback handler: maps the request path to an embedded asset; serves
/// `index.html` for anything not found (SPA deep-link support).
pub async fn spa_fallback(req: Request) -> Response {
    let raw = req.uri().path();
    // Strip the leading '/' — `RustEmbed` uses bare paths like "index.html"
    // and "assets/foo-abc123.js".
    let decoded = decode_path(raw);
    let path = decoded.trim_start_matches('/');

    if path.is_empty() || path == "index.html" {
        return spa_response("index.html");
    }

    // An API path no route serves is not a page: a client (or an older/newer one) asking for
    // a route this engine lacks must see a 404, not the SPA shell with status 200.
    if path.starts_with("api/") {
        return crate::api_errors::no_such_route(raw);
    }

    // Try the exact path first (for assets, favicon, etc.).
    if let Some(resp) = asset_response(path) {
        return resp;
    }

    if looks_like_a_file(path) {
        return (
            axum::http::StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("no such file: {path}"),
        )
            .into_response();
    }

    // Not found → SPA shell.  The React router will render a 404 page if the
    // route is truly invalid, or the correct screen if the user navigated
    // directly to e.g. `/library`.
    spa_response("index.html")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn hashed_assets_are_kept_for_good_and_the_shell_is_asked_for_each_time() {
        assert_eq!(
            cache_control_for("assets/index-abc123.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control_for("index.html"), "no-cache");
        assert_eq!(cache_control_for("favicon.ico"), "no-cache");
    }

    #[test]
    fn a_path_is_decoded_as_the_bundle_names_files() {
        assert_eq!(
            decode_path("/assets/my%20font.woff2"),
            "/assets/my font.woff2"
        );
        assert_eq!(decode_path("/plain.js"), "/plain.js");
        // Not an escape, or not text: as it came, and simply not found.
        assert_eq!(decode_path("/100%.js"), "/100%.js");
        assert_eq!(decode_path("/a%zz.js"), "/a%zz.js");
        assert_eq!(decode_path("/a%ff.js"), "/a%ff.js");
    }

    async fn status_of(path: &str) -> StatusCode {
        let req = Request::builder()
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap();
        spa_fallback(req).await.status()
    }

    /// A missing font or script must not come back as the shell with a 200, and a page of the
    /// app must still get the shell.
    #[tokio::test]
    async fn a_missing_file_is_a_404_and_a_page_is_the_shell() {
        assert_eq!(
            status_of("/assets/gone-abc123.js").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status_of("/assets/my%20gone.woff2").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(status_of("/library").await, StatusCode::OK);
        assert_eq!(status_of("/games/PPSA01234").await, StatusCode::OK);
        assert_eq!(status_of("/api/nope").await, StatusCode::NOT_FOUND);
    }
}
