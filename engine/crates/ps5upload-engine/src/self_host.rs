//! Play Together on a self-hosted engine (#881): the signalling hub and the guest pages' address
//! an engine uses when nothing is saved in Settings › Play Together, from environment variables. A
//! headless engine (Docker) has no app to push its settings, so these are its defaults:
//!
//! * `PS5UPLOAD_HUB_URL`: the hub (`wss://…`, as the hub setting takes it);
//! * `PS5UPLOAD_PAGE_BASE`: where the guest pages are (`https://play.example.com/`, the pages
//!   being `<base>join/`, `<base>pad/` and `<base>share/`);
//! * the built-in hub's `PS5UPLOAD_HUB_PUBLIC_URL` (crate::builtin_hub), which gives both when
//!   these two are not set, and its `PS5UPLOAD_HUB_STUN`, which puts its STUN server first.
//!
//! A value that does not pass the setting's own check is ignored with a warning at start-up. A
//! saved setting wins over the environment (join.rs keeps a setting only while it differs from
//! these defaults), and the API says which one is in force (`sources`).

use crate::builtin_hub::Plan;
use crate::join_net::{DEFAULT_HUB, DEFAULT_PAGE_BASE, DEFAULT_STUN};

pub(crate) const HUB_URL_ENV: &str = "PS5UPLOAD_HUB_URL";
pub(crate) const PAGE_BASE_ENV: &str = "PS5UPLOAD_PAGE_BASE";

/// The Play Together defaults of this engine run, and whether each came from the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Defaults {
    pub hub_url: String,
    pub page_base: String,
    pub stun: Vec<String>,
    pub hub_url_env: bool,
    pub page_base_env: bool,
    pub stun_env: bool,
    /// The built-in hub the environment asks for (None: off).
    pub built_in: Option<Plan>,
}

impl Defaults {
    /// The built-in defaults, overridden by what `get` finds. `warn` gets one line for each value
    /// that was ignored.
    pub(crate) fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        warn: &mut Vec<String>,
    ) -> Self {
        let built_in = Plan::from_lookup(&get, warn);
        let derived = |f: fn(&Plan) -> Option<String>| built_in.as_ref().and_then(f);
        let hub = derived(Plan::hub_url);
        let pages = derived(Plan::page_base);
        let own_stun = derived(Plan::stun_server);
        let mut d = Defaults {
            hub_url_env: hub.is_some(),
            hub_url: hub.unwrap_or_else(|| DEFAULT_HUB.to_string()),
            page_base_env: pages.is_some(),
            page_base: pages.unwrap_or_else(|| DEFAULT_PAGE_BASE.to_string()),
            stun_env: own_stun.is_some(),
            stun: own_stun
                .into_iter()
                .chain(DEFAULT_STUN.iter().map(|s| s.to_string()))
                .collect(),
            built_in,
        };
        let set = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        if let Some(v) = set(HUB_URL_ENV) {
            let v = v.trim();
            if crate::join_net::hub_url_ok(v) {
                d.hub_url = v.to_string();
                d.hub_url_env = true;
            } else {
                warn.push(format!(
                    "ignoring {HUB_URL_ENV}={v}: use a wss:// address (ws:// only to this \
                     computer). Using {} instead.",
                    d.hub_url
                ));
            }
        }
        if let Some(v) = set(PAGE_BASE_ENV) {
            match crate::join_net::page_base(&v) {
                Some(b) => {
                    d.page_base = b;
                    d.page_base_env = true;
                }
                None => warn.push(format!(
                    "ignoring {PAGE_BASE_ENV}={}: use an https:// address with no query or \
                     fragment, such as https://play.example.com/. Using {} instead.",
                    v.trim(),
                    d.page_base
                )),
            }
        }
        d
    }

    /// "environment" for a default the environment set, else "default".
    fn word(env: bool) -> &'static str {
        if env {
            "environment"
        } else {
            "default"
        }
    }

    /// Where each of the settings' `hub_url`, `page_base` and `stun` comes from: "custom" (saved
    /// in Settings, which only keeps what differs from these defaults), "environment" or
    /// "default".
    pub(crate) fn sources(
        &self,
        hub_url: &str,
        page_base: &str,
        stun: &[String],
    ) -> serde_json::Value {
        let pick = |same: bool, env: bool| if same { Self::word(env) } else { "custom" };
        serde_json::json!({
            "hub_url": pick(hub_url == self.hub_url, self.hub_url_env),
            "page_base": pick(page_base == self.page_base, self.page_base_env),
            "stun": pick(stun == self.stun.as_slice(), self.stun_env),
        })
    }
}

/// This run's defaults (read once from the environment).
pub(crate) fn defaults() -> &'static Defaults {
    static D: std::sync::OnceLock<(Defaults, Vec<String>)> = std::sync::OnceLock::new();
    &D.get_or_init(|| {
        let mut warn = Vec::new();
        let d = Defaults::from_lookup(|k| std::env::var(k).ok(), &mut warn);
        (d, warn)
    })
    .0
}

/// What a self-hoster should read at start-up: each ignored value, and the defaults in force
/// when the environment set them.
pub(crate) fn log_settings() {
    let mut warn = Vec::new();
    let d = Defaults::from_lookup(|k| std::env::var(k).ok(), &mut warn);
    for w in warn {
        crate::stderr_quiet(&format!("[ps5upload-engine] WARNING: {w}"));
        crate::log_at!(Warn, "together", "{w}");
    }
    if d.hub_url_env || d.page_base_env {
        crate::log_at!(
            Info,
            "together",
            "Play Together defaults from the environment: hub {}, pages {}",
            d.hub_url,
            d.page_base
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn nothing_set_is_the_built_in_defaults() {
        let mut w = Vec::new();
        let d = Defaults::from_lookup(env(&[]), &mut w);
        assert!(w.is_empty());
        assert_eq!(d.hub_url, "wss://hub.phantomptr.com/ws");
        assert_eq!(d.page_base, "https://ps5upload.phantomptr.com/");
        assert_eq!(
            d.stun,
            ["stun.cloudflare.com:3478", "stun.l.google.com:19302"]
        );
        assert!(!d.hub_url_env && !d.page_base_env && !d.stun_env);
        assert_eq!(
            d.sources(&d.hub_url, &d.page_base, &d.stun),
            serde_json::json!({"hub_url": "default", "page_base": "default", "stun": "default"})
        );
    }

    #[test]
    fn the_environment_sets_the_defaults_and_bad_values_are_ignored() {
        let mut w = Vec::new();
        let d = Defaults::from_lookup(
            env(&[
                (HUB_URL_ENV, " wss://hub.example.com/ws "),
                (PAGE_BASE_ENV, "https://Play.Example.com:443"),
            ]),
            &mut w,
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(d.hub_url, "wss://hub.example.com/ws");
        assert_eq!(d.page_base, "https://play.example.com/");
        assert_eq!(
            d.sources(&d.hub_url, &d.page_base, &d.stun),
            serde_json::json!({"hub_url": "environment", "page_base": "environment", "stun": "default"})
        );
        // A saved value is "custom", whatever the environment says.
        assert_eq!(
            d.sources("wss://mine.example/ws", "https://mine.example/", &[])["hub_url"],
            "custom"
        );

        let mut w = Vec::new();
        let d = Defaults::from_lookup(
            env(&[
                (HUB_URL_ENV, "https://hub.example.com/ws"),
                (PAGE_BASE_ENV, "http://play.example.com/"),
            ]),
            &mut w,
        );
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains(HUB_URL_ENV) && w[1].contains(PAGE_BASE_ENV));
        assert_eq!(d.hub_url, DEFAULT_HUB);
        assert_eq!(d.page_base, DEFAULT_PAGE_BASE);
        assert!(!d.hub_url_env && !d.page_base_env);
        // Empty is unset, not a mistake.
        let mut w = Vec::new();
        Defaults::from_lookup(env(&[(HUB_URL_ENV, " "), (PAGE_BASE_ENV, "")]), &mut w);
        assert!(w.is_empty());
    }
}
