//! A phone's "Add to Home Screen" for the pages this engine serves (the web UI at `/`, Play
//! Together's `/join`, the controller's `/pad`): the PS5Upload icon and the name "PS5Upload".
//!
//! GET /app-icon/{name}: a few small fixed files, public like the pages that link them (the
//! loopback guard lets `/app-icon/` through, lib.rs). iOS ignores a `data:` touch icon in some
//! versions, so the single-file pages link these real files instead, relatively
//! (`../app-icon/…`), which reaches the same files on the website (site/app-icon/, the same bytes:
//! `site_copy_is_the_same` below). Made by `python3 scripts/release/brand-assets.py --home-icons`;
//! the manifests are written by hand. `page.webmanifest` has no `start_url`, so a saved invite
//! opens the address it was saved from.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

const PNG: &str = "image/png";
const MANIFEST: &str = "application/manifest+json";

/// (name, type, bytes).
const FILES: &[(&str, &str, &[u8])] = &[
    (
        "apple-touch-icon.png",
        PNG,
        include_bytes!("app_icon/apple-touch-icon.png"),
    ),
    ("icon-192.png", PNG, include_bytes!("app_icon/icon-192.png")),
    ("icon-512.png", PNG, include_bytes!("app_icon/icon-512.png")),
    (
        "icon-maskable-512.png",
        PNG,
        include_bytes!("app_icon/icon-maskable-512.png"),
    ),
    (
        "app.webmanifest",
        MANIFEST,
        include_bytes!("app_icon/app.webmanifest"),
    ),
    (
        "page.webmanifest",
        MANIFEST,
        include_bytes!("app_icon/page.webmanifest"),
    ),
];

pub(crate) async fn file(Path(name): Path<String>) -> Response {
    match FILES.iter().find(|(n, _, _)| *n == name) {
        Some((_, ctype, bytes)) => (
            [
                (header::CONTENT_TYPE, *ctype),
                // Small and rarely changed; a day, then the browser checks again.
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            *bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no such file").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    async fn fetch(path: &str) -> (StatusCode, Option<String>, Vec<u8>) {
        let app = Router::new().route("/app-icon/{name}", get(file));
        let res = app
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let ctype = res
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, ctype, body.to_vec())
    }

    #[tokio::test]
    async fn icons_and_manifests_answer_with_their_types() {
        for name in [
            "apple-touch-icon.png",
            "icon-192.png",
            "icon-512.png",
            "icon-maskable-512.png",
        ] {
            let (s, t, b) = fetch(&format!("/app-icon/{name}")).await;
            assert_eq!(s, StatusCode::OK, "{name}");
            assert_eq!(t.as_deref(), Some(PNG), "{name}");
            assert!(b.starts_with(b"\x89PNG"), "{name}");
        }
        for name in ["app.webmanifest", "page.webmanifest"] {
            let (s, t, b) = fetch(&format!("/app-icon/{name}")).await;
            assert_eq!(s, StatusCode::OK, "{name}");
            assert_eq!(t.as_deref(), Some(MANIFEST), "{name}");
            let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
            assert_eq!(v["name"], "PS5Upload", "{name}");
            assert_eq!(v["short_name"], "PS5Upload", "{name}");
            // Every icon it names is one served here.
            for icon in v["icons"].as_array().unwrap() {
                let src = icon["src"].as_str().unwrap();
                assert!(FILES.iter().any(|(n, _, _)| *n == src), "{name}: {src}");
            }
        }
        let (s, _, _) = fetch("/app-icon/nope.png").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = fetch("/app-icon/..%2Flib.rs").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    /// iOS wants an opaque touch icon (a transparent one gets a black background): RGB, 180 px.
    #[test]
    fn touch_icon_is_opaque_180() {
        let b = include_bytes!("app_icon/apple-touch-icon.png");
        // IHDR: width, height (big-endian u32) at 16..24, colour type at 25 (2 = RGB).
        assert_eq!(&b[16..24], &[0, 0, 0, 180, 0, 0, 0, 180]);
        assert_eq!(b[25], 2);
    }

    /// The website's copy (site/app-icon/, linked by site/join and site/pad) is these files.
    #[test]
    fn site_copy_is_the_same() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../site/app-icon");
        for (name, _, bytes) in FILES {
            let site = std::fs::read(dir.join(name)).unwrap_or_default();
            assert!(
                site == *bytes,
                "site/app-icon/{name} differs: rerun brand-assets.py --home-icons"
            );
        }
    }
}
