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

use crate::pkg_install::in_container;

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

/// The engine never replaces itself: the desktop app installs its own updates, and a container is
/// pulled again. An update or restart there ends Remote Play and friends' play, so every answer
/// also says who plays through Play Together now (`friends_playing`: `[{console, friends}]`, empty
/// when nobody is in), read fresh on each call, for the app to ask "Friends are playing on this
/// PS5. Update anyway?" first.
pub(crate) async fn check() -> Json<serde_json::Value> {
    let mut answer = cached_check().await;
    answer["friends_playing"] = friends_now();
    Json(answer)
}

/// Who plays through Play Together now, on every console.
fn friends_now() -> serde_json::Value {
    crate::join::friends_playing_any()
        .into_iter()
        .map(|(console, friends)| serde_json::json!({ "console": console, "friends": friends }))
        .collect()
}

async fn cached_check() -> serde_json::Value {
    if let Some((at, v)) = CACHE.lock().ok().and_then(|g| g.clone()) {
        if at.elapsed() < CACHE_FOR {
            return v;
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
            return serde_json::json!({
                "current": current, "available": false, "docker": in_container(), "error": e,
            })
        }
    };
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), body.clone()));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn the_check_names_friends_playing() {
        let console = "10.0.94.1";
        crate::join::test_party_with_guest(console, "Lee");
        let v = super::friends_now();
        assert!(v
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["console"] == console && e["friends"] == serde_json::json!(["Lee"])));
        crate::join::test_close_party(console);
        assert!(!super::friends_now()
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["console"] == console));
    }

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
