//! A title and cover for a game that cannot name itself (only its DLC, or nothing but an
//! archive's Game ID): PROSPEROPatches for PS5, ORBISPatches for PS4, as PS Game Library does.
//! Best effort; a network failure is never taken to mean "no such game".

#[cfg(not(target_os = "android"))]
use std::time::Duration;

/// The Android engine is built without an HTTP client (pure Rust, like ps5upload-core's
/// lookups there): the lookup is unavailable, which the caller treats as a failed fetch, never
/// as "no such game".
#[cfg(target_os = "android")]
pub fn fetch(_game_id: &str) -> Result<(Option<String>, Option<String>), String> {
    Err("online lookups are not available on this device".into())
}

/// `(title, cover_url)` from a title page, or `Err` when the page could not be fetched.
#[cfg(not(target_os = "android"))]
pub fn fetch(game_id: &str) -> Result<(Option<String>, Option<String>), String> {
    let (host, cdn) = if game_id.starts_with("CU") {
        ("orbispatches.com", "orbispatches")
    } else {
        ("prosperopatches.com", "prosperopatches")
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&format!("https://{host}/{game_id}"))
        .header("User-Agent", "Mozilla/5.0 ps5upload-collection/1.0")
        .call()
        .map_err(|e| e.to_string())?;
    if resp.status() == 404 {
        return Ok((None, None));
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let html = resp
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((title_from(&html, game_id), cover_from(&html, game_id, cdn)))
}

/// The page title, without the Game ID prefix and the site suffix. The site's own generic
/// title (what an unknown ID gets) is not a game's name.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn title_from(html: &str, game_id: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = start + lower[start..].find("</title>")?;
    let mut t = html[start..end]
        .trim()
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"");
    if t.to_ascii_uppercase()
        .starts_with(&game_id.to_ascii_uppercase())
    {
        t = t[game_id.len()..]
            .trim_start_matches([':', ' '])
            .trim()
            .to_string();
    }
    for sep in [" | ", " - ", " – ", " — "] {
        if let Some(i) = t.rfind(sep) {
            let tail = t[i + sep.len()..].to_ascii_lowercase();
            if tail.starts_with("orbispatches") || tail.starts_with("prosperopatches") {
                t.truncate(i);
            }
        }
    }
    let t = t.trim().to_string();
    let lt = t.to_ascii_lowercase();
    if t.is_empty()
        || t.eq_ignore_ascii_case(game_id)
        || lt.starts_with("prosperopatches")
        || lt.starts_with("orbispatches")
        || lt.contains("game tracker")
    {
        return None;
    }
    Some(t)
}

#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn cover_from(html: &str, game_id: &str, cdn: &str) -> Option<String> {
    let needle = format!("https://cdn.{cdn}.com/titles/{game_id}_");
    let at = html.find(&needle)?;
    let rest = &html[at..];
    let end = rest.find(['"', '\'', ' ', ')']).unwrap_or(rest.len());
    let url = &rest[..end];
    url.contains("/icon0.").then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_the_id_and_the_site_name() {
        assert_eq!(
            title_from(
                "<title>PPSA01467: Marvel's Spider-Man Remastered | PROSPEROPatches.com</title>",
                "PPSA01467"
            ),
            Some("Marvel's Spider-Man Remastered".into())
        );
        assert_eq!(
            title_from(
                "<TITLE> CUSA00900: Bloodborne&amp;Co - ORBISPatches.com </TITLE>",
                "CUSA00900"
            ),
            Some("Bloodborne&Co".into())
        );
    }

    #[test]
    fn the_sites_own_page_title_is_not_a_game() {
        assert_eq!(
            title_from(
                "<title>PROSPEROPatches.com - PlayStation 5 Game Tracker</title>",
                "NPXS39041"
            ),
            None
        );
        assert_eq!(title_from("<title>PPSA01234</title>", "PPSA01234"), None);
    }

    #[test]
    fn covers_are_the_titles_icon_on_the_cdn() {
        let html =
            r#"<meta content="https://cdn.prosperopatches.com/titles/PPSA01467_ab12/icon0.webp">"#;
        assert_eq!(
            cover_from(html, "PPSA01467", "prosperopatches").as_deref(),
            Some("https://cdn.prosperopatches.com/titles/PPSA01467_ab12/icon0.webp")
        );
        assert_eq!(cover_from(html, "PPSA00001", "prosperopatches"), None);
    }
}
