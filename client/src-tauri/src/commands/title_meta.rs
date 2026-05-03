//! Title metadata fetch (HTML scrape of prosperopatches.com).
//!
//! The Library → Game Details modal renders a cover-art thumbnail
//! plus a few fields from a public PS5 title-info site. The fetch
//! lives here in Rust (rather than in the renderer) for two reasons:
//!
//!   1. The renderer's CSP `connect-src` whitelist intentionally
//!      doesn't include arbitrary external hosts; routing through a
//!      command keeps the renderer's network surface narrow.
//!   2. We enforce a hostname allowlist here so a compromised
//!      renderer can't pivot this command into an SSRF primitive
//!      (e.g. probing intranet hosts from inside the desktop app's
//!      network namespace).
//!
//! The command returns the response body as a UTF-8 string. The
//! renderer parses the HTML with `DOMParser` to pull out the
//! `<title>` text and `<meta name="twitter:image">` cover URL —
//! prosperopatches.com fills both for known titles, returns 404 for
//! unknown ones, and serves images off `cdn.prosperopatches.com`
//! (whitelisted in the renderer's `img-src`).
//!
//! Body size is capped to keep a hostile origin (or a misbehaving
//! redirect chain landing somewhere unexpected) from filling memory.

use std::time::Duration;

/// Public title-info hosts we will fetch on behalf of the renderer.
/// PROSPEROPatches covers PS5 (PPSA#####); ORBISPatches covers PS4
/// (CUSA#####, runnable on PS5 via backwards compatibility). Image
/// URLs from `cdn.prosperopatches.com` / `cdn.orbispatches.com` are
/// loaded directly via `<img src>` (whitelisted in the renderer
/// CSP), so the CDN hosts don't need to come through this command.
const ALLOWED_HOSTS: &[&str] = &["prosperopatches.com", "orbispatches.com"];

/// 1 MiB ceiling. The title pages are ~30 KB; anything close to
/// this is anomalous.
const MAX_BODY_BYTES: usize = 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Polite identifier so the upstream operator can distinguish our
/// traffic and contact us if there's an issue. Bumped along with
/// the desktop app's user-facing version.
const USER_AGENT: &str = concat!(
    "ps5upload/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/phantomptr/ps5upload)"
);

#[tauri::command]
pub async fn title_meta_fetch(url: String) -> Result<String, String> {
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("invalid url: {e}"))?;

    if parsed.scheme() != "https" {
        return Err(format!("refusing non-https url: {url}"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("url has no host: {url}"))?;
    if !ALLOWED_HOSTS.contains(&host) {
        return Err(format!("host not in allowlist: {host}"));
    }

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| format!("title-meta client init: {e}"))?;

    let resp = client
        .get(parsed)
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await
        .map_err(|e| format!("title-meta fetch: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("title-meta http {}", status.as_u16()));
    }

    if let Some(len) = resp.content_length() {
        if len as usize > MAX_BODY_BYTES {
            return Err(format!(
                "title-meta body too large ({len} > {MAX_BODY_BYTES} cap)"
            ));
        }
    }

    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("title-meta read body: {e}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "title-meta body too large after read ({} > {MAX_BODY_BYTES} cap)",
            body.len()
        ));
    }

    String::from_utf8(body.to_vec()).map_err(|e| format!("title-meta body not utf-8: {e}"))
}
