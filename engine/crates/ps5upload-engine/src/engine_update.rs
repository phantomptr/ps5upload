//! "Is there a newer release?" for the web UI.
//!
//! The desktop app checks GitHub itself and installs updates; a browser using a self-hosted
//! engine (Docker, a NAS) has nothing that can, so the page asks the engine. The engine reads the
//! same `latest.json` the desktop updater reads (the newest release, not pre-releases; Docker's
//! `:latest` tag moves at the same time) at most once per `CACHE_FOR`, and says whether it runs
//! in a container so the page can show the right way to update.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;

const MANIFEST_URL: &str =
    "https://github.com/phantomptr/ps5upload/releases/latest/download/latest.json";
const CACHE_FOR: Duration = Duration::from_secs(6 * 60 * 60);

/// The last answer and when it was fetched.
static CACHE: Mutex<Option<(Instant, serde_json::Value)>> = Mutex::new(None);

/// True when `latest` is a higher `major.minor.patch` than `current` (anything after a `-` or
/// `+` is ignored; an unreadable version is never newer).
pub(crate) fn is_newer(current: &str, latest: &str) -> bool {
    fn parts(v: &str) -> Option<[u64; 3]> {
        let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
        let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
        Some([it.next()??, it.next()??, it.next().flatten().unwrap_or(0)])
    }
    match (parts(current), parts(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Whether this engine runs in a container: the images set `PS5UPLOAD_DOCKER=1`, and Docker
/// leaves `/.dockerenv` for any other image built from them.
fn in_container() -> bool {
    std::env::var("PS5UPLOAD_DOCKER").is_ok_and(|v| v == "1")
        || std::path::Path::new("/.dockerenv").exists()
}

fn fetch_latest() -> Result<(String, String), String> {
    let text = ureq::get(MANIFEST_URL)
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .call()
        .map_err(|e| format!("fetching the release list: {e}"))?
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(|e| format!("reading the release list: {e}"))?;
    let m: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("reading the release list: {e}"))?;
    let version = m["version"].as_str().unwrap_or_default().to_string();
    if version.is_empty() {
        return Err("the release list has no version".into());
    }
    Ok((version, m["notes"].as_str().unwrap_or_default().to_string()))
}

pub(crate) async fn check() -> Json<serde_json::Value> {
    if let Some((at, v)) = CACHE.lock().ok().and_then(|g| g.clone()) {
        if at.elapsed() < CACHE_FOR {
            return Json(v);
        }
    }
    let current = env!("CARGO_PKG_VERSION");
    let fetched = tokio::task::spawn_blocking(fetch_latest)
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    let body = match fetched {
        Ok((latest, notes)) => serde_json::json!({
            "current": current,
            "latest": latest,
            "available": is_newer(current, &latest),
            "notes": notes,
            "docker": in_container(),
        }),
        // Not cached: the next page load tries again.
        Err(e) => {
            return Json(serde_json::json!({
                "current": current, "available": false, "docker": in_container(), "error": e,
            }))
        }
    };
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), body.clone()));
    }
    Json(body)
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_release_versions() {
        assert!(is_newer("10.0.3", "10.1.0"));
        assert!(is_newer("9.9.9", "10.0.0"));
        assert!(!is_newer("10.1.0", "10.1.0"));
        assert!(!is_newer("10.1.0", "10.0.9"));
        assert!(is_newer("10.1", "10.1.1"));
        assert!(!is_newer("10.1.0", "garbage"));
        assert!(!is_newer("10.1.0-beta", "10.1.0"));
    }
}
