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
    let body: Vec<u8> = file.data.into_owned();
    Some(([(header::CONTENT_TYPE, mime)], body).into_response())
}

/// Return a response for `path` inside the embedded bundle.  If the path is
/// not found, serve `index.html` instead (SPA client-side-routing fallback).
pub fn spa_response(path: &str) -> Response {
    asset_response(path).unwrap_or_else(|| {
        // Serve index.html as the SPA shell for any path the router doesn't
        // recognise.  The React app's client-side router takes over from here.
        match WebAssets::get("index.html") {
            Some(index) => (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                index.data.into_owned(),
            )
                .into_response(),
            // index.html is embedded at build time when the webui feature is
            // enabled. If we hit this path the binary was misbuilt (webui
            // feature on but no client build output). Return a clear error
            // instead of panicking so the engine stays up for API use.
            None => (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "webui index.html not found — rebuild with the client assets bundled."
                    .to_string()
                    .into_bytes(),
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
    let path = raw.trim_start_matches('/');

    if path.is_empty() || path == "index.html" {
        return spa_response("index.html");
    }

    // Try the exact path first (for assets, favicon, etc.).
    if let Some(resp) = asset_response(path) {
        return resp;
    }

    // Not found → SPA shell.  The React router will render a 404 page if the
    // route is truly invalid, or the correct screen if the user navigated
    // directly to e.g. `/library`.
    spa_response("index.html")
}
