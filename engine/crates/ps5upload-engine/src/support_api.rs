//! `GET /api/support`: Thanks & Support for PS5Upload Play and Minion (integration design (e)).
//!
//! One source with the desktop app's Thanks page: `client/src/data/supporters.json` (the names,
//! most first, and the count of anonymous tips) and `client/src/data/support.json` (Ko-fi, the
//! website's thanks section, the crypto addresses), built into the engine. Public data, no auth:
//! reachable from the home network like the guest pages, so the PS5 app can refresh the list it
//! carries when this engine's `rev` differs. Names only, no amounts, no tiers.

use axum::{http::header, response::IntoResponse};
use sha2::{Digest, Sha256};

const SUPPORTERS: &str = include_str!("../../../../client/src/data/supporters.json");
const LINKS: &str = include_str!("../../../../client/src/data/support.json");

/// The answer: `{rev, names, anonymous, links}`. `rev` is a short hash of both files, so a client
/// asks for the list again only when it changed.
pub(crate) fn body() -> String {
    let supporters: serde_json::Value =
        serde_json::from_str(SUPPORTERS).unwrap_or_else(|_| serde_json::json!({}));
    let links: serde_json::Value =
        serde_json::from_str(LINKS).unwrap_or_else(|_| serde_json::json!({}));
    let mut h = Sha256::new();
    h.update(SUPPORTERS.as_bytes());
    h.update([0]);
    h.update(LINKS.as_bytes());
    let rev: String = h.finalize()[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    serde_json::json!({
        "rev": rev,
        "names": supporters.get("names").cloned().unwrap_or_else(|| serde_json::json!([])),
        "anonymous": supporters.get("anonymous").cloned().unwrap_or_else(|| serde_json::json!(0)),
        "links": links,
    })
    .to_string()
}

/// GET /api/support: the names and links (`{rev, names, anonymous, links}`).
pub(crate) async fn support() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_links_and_a_stable_rev() {
        let v: serde_json::Value = serde_json::from_str(&body()).unwrap();
        assert!(v["names"].as_array().is_some_and(|n| !n.is_empty()));
        assert!(v["anonymous"].is_number());
        assert!(v["links"]["kofi"]
            .as_str()
            .is_some_and(|k| k.starts_with("https://ko-fi.com/")));
        assert_eq!(v["links"]["crypto"].as_array().map(|c| c.len()), Some(4));
        assert_eq!(v["rev"].as_str().map(str::len), Some(12));
        assert_eq!(body(), body());
    }
}
