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
        // Disable redirects so the hostname allowlist actually
        // gates every byte the renderer can see. Default reqwest
        // policy follows up to 10 redirects automatically — a 3xx
        // from prosperopatches.com to attacker.example would
        // silently be followed and the body returned, defeating
        // the SSRF defense the allowlist is here to provide.
        // 3xx responses are surfaced as an error below.
        .redirect(reqwest::redirect::Policy::none())
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
        // Compare in u64 space — `len as usize` truncates on 32-bit
        // targets (Windows x86 builds), letting a 5 GiB
        // Content-Length sneak past the cap and OOM the desktop on
        // the subsequent .bytes().await.
        if len > MAX_BODY_BYTES as u64 {
            return Err(format!(
                "title-meta body too large ({len} > {MAX_BODY_BYTES} cap)"
            ));
        }
    }

    // The Content-Length check above is only an early-out: it is None for
    // chunked transfer-encoding, and `resp.bytes()` would then buffer the
    // whole body before the post-read cap fires. Stream with a running
    // total so a no-Content-Length response can't defeat the cap.
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("title-meta read body: {e}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(format!(
                "title-meta body too large (> {MAX_BODY_BYTES} cap)"
            ));
        }
        body.extend_from_slice(&chunk);
    }

    String::from_utf8(body).map_err(|e| format!("title-meta body not utf-8: {e}"))
}
