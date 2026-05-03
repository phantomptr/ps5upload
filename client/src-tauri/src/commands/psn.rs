//! PSN Store metadata fetch, server-side (Rust).
//!
//! The renderer's `fetch()` to PSN endpoints is blocked by the
//! webview's CSP `connect-src` (and would face CORS even if the CSP
//! were widened). Routing through this command sidesteps both:
//!
//!   1. The request is issued from the desktop process, not the
//!      renderer — no CSP, no CORS preflight.
//!   2. We enforce a hostname allowlist here so a compromised
//!      renderer can't pivot this command into an SSRF primitive
//!      (e.g. probing intranet hosts from inside the desktop app's
//!      network namespace).
//!
//! The command returns the response body as a UTF-8 string. The
//! renderer parses JSON itself — keeps this file tiny and free of
//! PSN schema knowledge that changes more often than this code does.
//!
//! Body size is capped to keep a hostile origin (or a misbehaving
//! redirect chain landing somewhere unexpected) from filling memory.

use std::time::Duration;

/// PSN endpoints we will fetch on behalf of the renderer.
///
/// `store.playstation.com` covers both the modern valkyrie-api and
/// the legacy chihiro titlecontainer endpoints. Cover art previews
/// served through `image.api.playstation.com` go straight to `<img
/// src>` after CSP whitelist (see tauri.conf.json `img-src`), so
/// they don't need a fetch hop.
const ALLOWED_HOSTS: &[&str] = &["store.playstation.com"];

/// 1 MiB ceiling. PSN's resolve + titlecontainer payloads are tens
/// of kilobytes; anything close to this is anomalous.
const MAX_BODY_BYTES: usize = 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

#[tauri::command]
pub async fn psn_fetch(url: String) -> Result<String, String> {
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
        .build()
        .map_err(|e| format!("psn client init: {e}"))?;

    let resp = client
        .get(parsed)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("psn fetch: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("psn http {}", status.as_u16()));
    }

    if let Some(len) = resp.content_length() {
        if len as usize > MAX_BODY_BYTES {
            return Err(format!(
                "psn body too large ({len} > {MAX_BODY_BYTES} cap)"
            ));
        }
    }

    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("psn read body: {e}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "psn body too large after read ({} > {MAX_BODY_BYTES} cap)",
            body.len()
        ));
    }

    String::from_utf8(body.to_vec()).map_err(|e| format!("psn body not utf-8: {e}"))
}
