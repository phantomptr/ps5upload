//! ShadowMount+'s HTTP API (1.7beta8 and later): `POST http://<console>:10101/api/v1/<route>`
//! with one JSON object, answered with JSON carrying a numeric `status` (0 = ok, else an errno)
//! and `error`. Written from SMP's documented behaviour (docs/api.md), not its code.
//!
//! Since beta8 the API listens on the LAN by default. A user who set `api_bind_address=127.0.0.1`,
//! disabled it, or runs an older SMP gets no answer: callers then fall back to the older ways
//! (`config.ini` rules, the scan root), so nothing here is required.
//!
//! Measured on a PS5 Pro (FW 13.60, SMP 1.7beta11): with SMP's default
//! `persistent_image_mounts=0` no image is mounted while idle; `games/mount` with `mode: "rw"`
//! mounts one game's image read-write for as long as it is held, and a scan requested meanwhile
//! is deferred (`runtime_prepared`) rather than releasing it; `games/unmount` releases it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

pub const API_PORT: u16 = 10101;
/// Short: a console without the API should not hold up the fallback.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Mounting an image can take a while on a busy console.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SmpVersion {
    #[serde(default)]
    pub shadowmount_version: String,
    #[serde(default)]
    pub api_version: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl SmpVersion {
    pub fn can(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// One image as SMP lists it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SmpImage {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub mount_point: String,
    #[serde(default)]
    pub mounted: bool,
}

pub struct SmpApi {
    base: String,
    agent: ureq::Agent,
}

fn agent(timeout: Duration) -> ureq::Agent {
    // A LAN call: never through a proxy from the environment, and an error status is read,
    // not raised, so SMP's own `error` text reaches the user.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .proxy(None)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// What an error answer means, in words a user can act on.
pub fn explain(route: &str, http: u16, status: i64, error: &str) -> String {
    let what = match (http, status) {
        (404, _) => "ShadowMount+ does not track this game (no mount link for it)".to_string(),
        (409, _) => "ShadowMount+ is busy with this game (it is running, launching, or being scanned or moved); close the game and try again".to_string(),
        (501, _) => "this game's image type can only be mounted read-only".to_string(),
        _ => format!("ShadowMount+ answered HTTP {http}"),
    };
    if error.is_empty() {
        format!("{route}: {what} (status {status})")
    } else {
        format!("{route}: {what} ({error})")
    }
}

impl SmpApi {
    /// The API on the console at `addr` (any `host[:port]` form), when it answers its version
    /// call; `None` for no API (older SMP, bound to the console only, switched off, or SMP not
    /// running).
    pub fn probe(addr: &str) -> Option<(SmpApi, SmpVersion)> {
        let host = crate::events::console_host(addr);
        if host.is_empty() {
            return None;
        }
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let mut api = SmpApi {
            base: format!("http://{host}:{API_PORT}/api/v1/"),
            agent: agent(PROBE_TIMEOUT),
        };
        let v: SmpVersion = serde_json::from_value(api.call("version", json!({})).ok()?).ok()?;
        if v.api_version == 0 {
            return None;
        }
        api.agent = agent(CALL_TIMEOUT);
        Some((api, v))
    }

    fn call(&self, route: &str, body: Value) -> Result<Value> {
        let mut resp = self
            .agent
            .post(format!("{}{route}", self.base))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .with_context(|| format!("ShadowMount+ API {route}"))?;
        let http = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .with_context(|| format!("read ShadowMount+ API {route}"))?;
        let v: Value = serde_json::from_str(&text)
            .with_context(|| format!("ShadowMount+ API {route} did not answer JSON"))?;
        let status = v.get("status").and_then(Value::as_i64).unwrap_or(-1);
        if http != 200 || status != 0 {
            let error = v.get("error").and_then(Value::as_str).unwrap_or("");
            bail!(explain(route, http, status, error));
        }
        Ok(v)
    }

    /// Mount one game, read-write when `rw`, else in its configured mode.
    pub fn mount(&self, title_id: &str, rw: bool) -> Result<()> {
        let body = if rw {
            json!({ "title_id": title_id, "mode": "rw" })
        } else {
            json!({ "title_id": title_id })
        };
        let v = self.call("games/mount", body)?;
        if rw && v.get("mode").and_then(Value::as_str) != Some("rw") {
            bail!("ShadowMount+ did not mount {title_id} read-write");
        }
        Ok(())
    }

    /// Queues a full rescan, clearing the retry counters: a title whose earlier images failed to
    /// mount is otherwise skipped, and a new image of it is never tried.
    pub fn rescan(&self) -> Result<()> {
        self.call("scan", json!({ "reset_attempts": true }))?;
        Ok(())
    }

    pub fn unmount(&self, title_id: &str) -> Result<()> {
        self.call("games/unmount", json!({ "title_id": title_id }))?;
        Ok(())
    }

    pub fn images(&self) -> Result<Vec<SmpImage>> {
        let v = self.call("images", json!({}))?;
        Ok(v.get("images")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .context("ShadowMount+ image list")?
            .unwrap_or_default())
    }

    /// The image at `image_path`, as SMP lists it.
    pub fn image(&self, image_path: &str) -> Result<Option<SmpImage>> {
        Ok(self.images()?.into_iter().find(|i| i.path == image_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_answer_reads_capabilities() {
        // As the Pro (SMP 1.7beta11) answered, shortened.
        let v: SmpVersion = serde_json::from_value(json!({
            "status": 0, "api_version": 1, "shadowmount_version": "1.7beta11",
            "capabilities": ["web_ui", "mount_game", "unmount_game", "list_images"]
        }))
        .unwrap();
        assert_eq!(v.shadowmount_version, "1.7beta11");
        assert!(v.can("mount_game") && v.can("unmount_game"));
        assert!(!v.can("pkg_install"));
    }

    #[test]
    fn image_list_tolerates_extra_and_missing_fields() {
        let i: SmpImage = serde_json::from_value(json!({
            "path": "/data/homebrew/Planet Zoo - PPSA18026.exfat",
            "mount_point": "/mnt/shadowmnt/Planet Zoo_44080ef3",
            "mounted": true, "backend": "LVD", "size": 1
        }))
        .unwrap();
        assert!(i.mounted);
        let bare: SmpImage = serde_json::from_value(json!({})).unwrap();
        assert!(!bare.mounted && bare.path.is_empty());
    }

    #[test]
    fn errors_say_what_to_do() {
        assert!(explain("games/mount", 409, 16, "EBUSY").contains("close the game"));
        assert!(explain("games/mount", 404, 2, "").contains("does not track"));
        assert!(explain("games/mount", 501, 45, "ENOTSUP").contains("read-only"));
        assert!(explain("games/mount", 500, 5, "").contains("HTTP 500"));
    }

    #[test]
    fn no_api_is_none_quickly() {
        // Nothing listens on this port of the loopback address.
        let t = std::time::Instant::now();
        assert!(SmpApi::probe("127.0.0.1:1").is_none());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
