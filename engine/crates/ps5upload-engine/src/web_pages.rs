//! What every page this engine serves gets on its way out: the web UI's files, `/join` and `/pad`.
//!
//! They used to go out raw and were sent again in full on each load. A friend on mobile data waited
//! for 800 KB of `/join` after every reload, and the web UI cost 2-3 MB per load, over the Tailscale
//! path the FAQ recommends too. Text compresses to about a quarter, so it goes gzipped when the
//! client takes that; each file is compressed once and kept. An `ETag` lets a browser keep its copy
//! of what has no hash in its name (`Cache-Control` is the route's own when it set one).
//!
//! Also here because it is the same place: `nosniff`, and no framing by another site. Plain http
//! has no Fetch Metadata for the origin guard to read, so a page elsewhere could otherwise frame
//! the UI. And `X-Accel-Buffering: no` on event streams, so that a buffering proxy cannot hold them
//! back until the client's idle watchdog gives up.
//!
//! Never the API or `/pkg-host/`: those stream, and a console will not read gzip.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

/// Nothing this engine embeds is near this; a larger body is not one of its pages.
const MAX_PAGE: u64 = 32 * 1024 * 1024;
/// Below this, gzip's own overhead eats the gain.
const MIN_GZIP: usize = 1024;
/// Bodies kept compressed. The set of files is fixed in a release build; this bounds a debug build
/// reading from disk.
const MAX_CACHED: usize = 2048;

const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");
const X_CONTENT_TYPE_OPTIONS: HeaderName = HeaderName::from_static("x-content-type-options");
const X_FRAME_OPTIONS: HeaderName = HeaderName::from_static("x-frame-options");

fn is_page_path(method: &Method, path: &str) -> bool {
    let path = crate::host_guard::strip_base(path);
    method == Method::GET && !path.starts_with("/api/") && !path.starts_with("/pkg-host/")
}

/// Text that compresses well. Images and fonts already are compressed.
fn compresses(content_type: &str) -> bool {
    let t = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    t.starts_with("text/")
        || matches!(
            t.as_str(),
            "application/javascript"
                | "application/json"
                | "application/manifest+json"
                | "application/wasm"
                | "application/xml"
                | "image/svg+xml"
        )
}

/// `Accept-Encoding` names gzip and does not refuse it (`gzip;q=0`).
fn takes_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|coding| {
            let mut parts = coding.split(';');
            let name = parts.next().unwrap_or("").trim();
            let refused = parts.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q <= 0.0)
            });
            name.eq_ignore_ascii_case("gzip") && !refused
        })
}

fn fingerprint(body: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    h.finish()
}

fn etag_of(print: u64, len: usize) -> String {
    format!("\"{print:016x}-{len:x}\"")
}

/// `If-None-Match` lists this tag, or says anything will do. A proxy that gzips by itself weakens
/// the tag (`W/`); it still names this body.
fn matches_etag(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

fn gzip(body: &[u8]) -> Option<Bytes> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(
        Vec::with_capacity(body.len() / 3),
        flate2::Compression::default(),
    );
    enc.write_all(body).ok()?;
    enc.finish().ok().map(Bytes::from)
}

/// This body gzipped: from memory after the first time. None when gzip does not make it smaller.
async fn gzipped(print: u64, body: &Bytes) -> Option<Bytes> {
    static CACHE: Mutex<Option<HashMap<u64, Option<Bytes>>>> = Mutex::new(None);
    let read = || {
        let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().and_then(|m| m.get(&print).cloned())
    };
    if let Some(known) = read() {
        return known;
    }
    let input = body.clone();
    let packed = tokio::task::spawn_blocking(move || gzip(&input))
        .await
        .ok()
        .flatten()
        .filter(|gz| gz.len() < body.len());
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= MAX_CACHED {
        map.clear();
    }
    map.insert(print, packed.clone());
    packed
}

/// The app's `index.html` as this engine serves it. The bundle is built with relative paths so that
/// one image can run at any prefix, but a relative path on a deep link (`/ps5/games/PPSA01234`)
/// would resolve under the route: they become absolute here, and the page is told its prefix for
/// its API calls and its router (client `lib/basePath.ts`). `prefix` is `""` or a checked path
/// (`host_guard::normalize_base`), so it needs no escaping. A build with a prefix baked in has no
/// relative paths, and only gets the tag.
#[cfg(any(feature = "webui", test))]
pub(crate) fn page_shell(html: &[u8], prefix: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(html);
    let text = text.replace("=\"./", &format!("=\"{prefix}/")).replacen(
        "<head>",
        &format!("<head><meta name=\"ps5upload-base\" content=\"{prefix}\">"),
        1,
    );
    text.into_bytes()
}

/// The layer. See the module comment.
pub(crate) async fn finish_pages(req: Request, next: Next) -> Response {
    let page = is_page_path(req.method(), req.uri().path());
    let asked = page.then(|| req.headers().clone());
    let mut res = next.run(req).await;
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if content_type.starts_with("text/event-stream") {
        res.headers_mut()
            .insert(X_ACCEL_BUFFERING, HeaderValue::from_static("no"));
    }
    let Some(asked) = asked else {
        return res;
    };
    harden(res.headers_mut(), &content_type);
    if res.status() != StatusCode::OK
        || !compresses(&content_type)
        || res.headers().contains_key(header::CONTENT_ENCODING)
        || !res
            .body()
            .size_hint()
            .exact()
            .is_some_and(|n| n <= MAX_PAGE)
    {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(body) = axum::body::to_bytes(body, MAX_PAGE as usize).await else {
        parts.status = StatusCode::INTERNAL_SERVER_ERROR;
        return Response::from_parts(parts, Body::empty());
    };
    let print = fingerprint(&body);
    let etag = etag_of(print, body.len());
    if let Ok(value) = HeaderValue::from_str(&etag) {
        parts.headers.insert(header::ETAG, value);
    }
    parts
        .headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-cache"));
    parts
        .headers
        .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    if matches_etag(&asked, &etag) {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.remove(header::CONTENT_TYPE);
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    }
    if body.len() >= MIN_GZIP && takes_gzip(&asked) {
        if let Some(gz) = gzipped(print, &body).await {
            parts.headers.remove(header::CONTENT_LENGTH);
            parts
                .headers
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            return Response::from_parts(parts, Body::from(gz));
        }
    }
    Response::from_parts(parts, Body::from(body))
}

fn harden(headers: &mut HeaderMap, content_type: &str) {
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if content_type.starts_with("text/html") {
        headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
        headers.append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'self'"),
        );
    }
}

/// An API answer can hold a key (the console list, a settings backup), and a browser's disk cache
/// keeps what it is not told otherwise: words go uncached unless their route says differently.
/// Pictures and files are left alone, as they have their own rules.
pub(crate) async fn api_no_store(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    if api && !res.headers().contains_key(header::CACHE_CONTROL) {
        let words = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|t| t.to_str().ok())
            .is_none_or(|t| t.starts_with("text/") || t.contains("json"));
        if words {
            res.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use tower::ServiceExt;

    fn big_page() -> String {
        format!("<!doctype html>{}", "<p>hello</p>".repeat(1000))
    }

    fn app() -> axum::Router {
        axum::Router::new()
            .route("/join", get(|| async { axum::response::Html(big_page()) }))
            .route("/tiny", get(|| async { axum::response::Html("<p>x</p>") }))
            .route(
                "/assets/a.js",
                get(|| async {
                    (
                        [
                            (header::CONTENT_TYPE, "application/javascript"),
                            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                        ],
                        "x".repeat(4096),
                    )
                }),
            )
            .route(
                "/logo.png",
                get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![7u8; 4096]) }),
            )
            .route(
                "/api/list",
                get(|| async { axum::Json(serde_json::json!({ "pad": "x".repeat(4096) })) }),
            )
            .route(
                "/pkg-host/a.txt",
                get(|| async { ([(header::CONTENT_TYPE, "text/plain")], "x".repeat(4096)) }),
            )
            .route(
                "/api/events",
                get(|| async {
                    let none = futures_util::stream::empty::<
                        Result<axum::response::sse::Event, std::convert::Infallible>,
                    >();
                    axum::response::sse::Sse::new(none)
                }),
            )
            .layer(axum::middleware::from_fn(finish_pages))
    }

    async fn fetch(path: &str, headers: &[(&'static str, &str)]) -> Response {
        let mut req = Request::builder().uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        app()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn body_of(res: Response) -> Bytes {
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
    }

    #[test]
    fn the_shell_names_its_files_from_the_prefix_so_a_deep_link_finds_them() {
        let built = br#"<html><head><link rel="icon" href="./logo.png"><script src="./boot-guard.js"></script><script type="module" src="./assets/app-1.js"></script></head></html>"#;
        let at_root = String::from_utf8(page_shell(built, "")).unwrap();
        assert!(at_root.contains(r#"<head><meta name="ps5upload-base" content="">"#));
        assert!(at_root.contains(r#"src="/assets/app-1.js""#));
        assert!(at_root.contains(r#"src="/boot-guard.js""#));
        assert!(!at_root.contains("\"./"));
        let below = String::from_utf8(page_shell(built, "/ps5")).unwrap();
        assert!(below.contains(r#"content="/ps5""#));
        assert!(below.contains(r#"src="/ps5/assets/app-1.js""#));
        assert!(below.contains(r#"href="/ps5/logo.png""#));
        // Baked into the build: left alone.
        let baked = page_shell(
            br#"<head><script src="/old/assets/a.js"></script></head>"#,
            "",
        );
        assert!(String::from_utf8(baked)
            .unwrap()
            .contains(r#"src="/old/assets/a.js""#));
    }

    #[tokio::test]
    async fn a_page_goes_gzipped_to_a_browser_and_plain_to_anything_else() {
        let res = fetch("/join", &[("accept-encoding", "gzip, deflate, br")]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(res.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
        let packed = body_of(res).await;
        assert!(packed.len() < big_page().len() / 4);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&packed[..]), &mut text)
            .unwrap();
        assert_eq!(text, big_page());

        let plain = fetch("/join", &[]).await;
        assert!(!plain.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(body_of(plain).await, big_page().as_bytes());

        let refused = fetch("/join", &[("accept-encoding", "gzip;q=0")]).await;
        assert!(!refused.headers().contains_key(header::CONTENT_ENCODING));
    }

    #[tokio::test]
    async fn a_browser_with_a_current_copy_gets_no_body() {
        let first = fetch("/join", &[]).await;
        let etag = first.headers()[header::ETAG].to_str().unwrap().to_string();
        let again = fetch("/join", &[("if-none-match", &etag)]).await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(again.headers()[header::ETAG], etag.as_str());
        assert!(body_of(again).await.is_empty());
        let weak = format!("W/{etag}");
        let proxied = fetch("/join", &[("if-none-match", &weak)]).await;
        assert_eq!(proxied.status(), StatusCode::NOT_MODIFIED);
        let other = fetch("/join", &[("if-none-match", "\"old\"")]).await;
        assert_eq!(other.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_route_keeps_its_own_cache_rule() {
        let res = fetch("/assets/a.js", &[("accept-encoding", "gzip")]).await;
        assert_eq!(
            res.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(res.headers()[header::CONTENT_ENCODING], "gzip");
    }

    #[tokio::test]
    async fn pages_are_hardened_and_only_html_needs_the_framing_rule() {
        let html = fetch("/tiny", &[]).await;
        assert_eq!(html.headers()["x-content-type-options"], "nosniff");
        assert_eq!(html.headers()["x-frame-options"], "SAMEORIGIN");
        assert_eq!(
            html.headers()[header::CONTENT_SECURITY_POLICY],
            "frame-ancestors 'self'"
        );
        let js = fetch("/assets/a.js", &[]).await;
        assert_eq!(js.headers()["x-content-type-options"], "nosniff");
        assert!(!js.headers().contains_key("x-frame-options"));
    }

    #[tokio::test]
    async fn too_small_and_already_compressed_files_go_as_they_are() {
        let tiny = fetch("/tiny", &[("accept-encoding", "gzip")]).await;
        assert!(!tiny.headers().contains_key(header::CONTENT_ENCODING));
        let png = fetch("/logo.png", &[("accept-encoding", "gzip")]).await;
        assert!(!png.headers().contains_key(header::CONTENT_ENCODING));
        assert!(!png.headers().contains_key(header::ETAG));
    }

    #[tokio::test]
    async fn the_api_and_what_a_console_fetches_are_left_alone() {
        for path in ["/api/list", "/pkg-host/a.txt"] {
            let res = fetch(path, &[("accept-encoding", "gzip")]).await;
            assert!(
                !res.headers().contains_key(header::CONTENT_ENCODING),
                "{path}"
            );
            assert!(!res.headers().contains_key(header::ETAG), "{path}");
            assert!(!res.headers().contains_key("x-frame-options"), "{path}");
        }
    }

    #[tokio::test]
    async fn event_streams_tell_a_proxy_not_to_buffer_them() {
        let res = fetch("/api/events", &[("accept-encoding", "gzip")]).await;
        assert_eq!(res.headers()["x-accel-buffering"], "no");
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
    }
}
