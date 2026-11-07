//! Diagnostics: the old "Detailed logs" switch and the feature summary a bug report carries.
//!
//! * `GET  /api/diag/log-level` -> `{detailed, until_ms, max_hours}`
//! * `POST /api/diag/log-level` `{detailed, until_ms?}` -> the same. The switch from before log
//!   levels (`/api/logs/level`, log_levels.rs), kept for older apps: on raises the global level to
//!   `debug` until `until_ms` (Unix ms), at most [`MAX_DETAILED`] from now and a day when it is
//!   left out; off drops the raise.
//! * `GET  /api/diag/features` -> what each feature is doing now, as counts and states: Play
//!   Together, phone controllers and the watch party, Go live, Remote Play (the running session
//!   and the last one's start and end), saved remote connections, the mesh. Built to be posted in
//!   public: never a stream, pairing, invite or resume key, a passcode, a password, a user name,
//!   or the full address of a stream destination.

use std::time::Duration;

use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// The longest detailed logs stay on at a time.
pub(crate) const MAX_DETAILED: Duration = Duration::from_secs(24 * 3600);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn level_json() -> Value {
    let until = ps5upload_core::log::verbose_until();
    json!({
        "detailed": until.is_some(),
        "until_ms": until,
        "max_hours": MAX_DETAILED.as_secs() / 3600,
    })
}

/// When detailed logs asked to run until `asked` (Unix ms) turn off, given the time now: never
/// later than [`MAX_DETAILED`] from now; the full day when no time was given.
pub(crate) fn clamp_until(asked: Option<u64>, now: u64) -> u64 {
    let max = now + MAX_DETAILED.as_millis() as u64;
    asked.unwrap_or(max).min(max)
}

pub async fn log_level_get() -> Json<Value> {
    Json(level_json())
}

#[derive(Deserialize)]
pub struct LogLevelReq {
    detailed: bool,
    #[serde(default)]
    until_ms: Option<u64>,
}

pub async fn log_level_set(Json(req): Json<LogLevelReq>) -> Response {
    let before = ps5upload_core::log::levels();
    let was = ps5upload_core::log::verbose();
    if req.detailed {
        let now = now_ms();
        let until = clamp_until(req.until_ms, now);
        ps5upload_core::log::set_verbose_until(until);
        if until > now && !was {
            crate::log_info!(
                "detailed logs on for {} min (the global level at debug)",
                (until - now) / 60_000
            );
        }
    } else {
        ps5upload_core::log::set_verbose_until(0);
        if was {
            crate::log_info!("detailed logs off");
        }
    }
    // Saved and announced like a PUT /api/logs/level, so a restart keeps it and the app hears.
    crate::log_levels::changed(&before, &ps5upload_core::log::levels());
    Json(level_json()).into_response()
}

fn started() -> &'static std::time::Instant {
    static T: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T.get_or_init(std::time::Instant::now)
}

/// Note the engine's start (its uptime counts from here). Called once at startup.
pub(crate) fn mark_started() {
    let _ = started();
}

/// How long the engine has run, in seconds.
pub(crate) fn uptime_s() -> u64 {
    started().elapsed().as_secs()
}

/// This computer for a bug report: OS and its version, CPU, memory, uptime of the machine and the
/// engine, network interfaces by kind and MTU (never an address or a MAC). Each value is null when
/// it can't be read.
pub(crate) fn host_json() -> Value {
    let mut v = serde_json::to_value(ps5upload_core::host_info::host_info()).unwrap_or_default();
    v["engine_uptime_s"] = json!(uptime_s());
    v["in_container"] = json!(crate::pkg_install::in_container());
    v["network_interfaces"] = json!(net_interfaces());
    v
}

/// The network interfaces by kind, each once, with its MTU where it can be read and whether it
/// has an IPv4 / IPv6 address. Never the name's number, an address or a MAC.
pub(crate) fn net_interfaces() -> Vec<Value> {
    use std::collections::BTreeMap;
    let mut by_name: BTreeMap<String, (bool, bool, bool)> = BTreeMap::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        let e = by_name.entry(i.name.clone()).or_default();
        e.0 |= i.is_loopback();
        e.1 |= i.ip().is_ipv4();
        e.2 |= i.ip().is_ipv6();
    }
    let mtus = interface_mtus();
    by_name
        .into_iter()
        .map(|(name, (lo, v4, v6))| {
            json!({
                "kind": if lo { "loopback" } else { interface_kind(&name) },
                "mtu": mtus.get(&name),
                "ipv4": v4,
                "ipv6": v6,
            })
        })
        .collect()
}

/// What an interface is, from its name: `en0` is "ethernet_or_wifi", `utun3` is "vpn".
pub(crate) fn interface_kind(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let starts = |p: &[&str]| p.iter().any(|x| n.starts_with(x));
    if starts(&["lo"]) {
        "loopback"
    } else if starts(&[
        "utun",
        "tun",
        "tap",
        "wg",
        "tailscale",
        "zt",
        "ppp",
        "ipsec",
    ]) {
        "vpn"
    } else if starts(&[
        "docker", "veth", "br-", "bridge", "virbr", "vmnet", "vboxnet", "awdl", "llw", "anpi", "ap",
    ]) {
        "virtual"
    } else if starts(&["wl", "wifi", "wi-fi", "wireless"]) {
        "wifi"
    } else if starts(&["eth", "enp", "eno", "ens", "enx", "ethernet"]) {
        "ethernet"
    } else if starts(&["en"]) {
        // macOS: en0 is Wi-Fi on a laptop and Ethernet on a desktop.
        "ethernet_or_wifi"
    } else if starts(&["rmnet", "ccmni", "pdp", "wwan"]) {
        "cellular"
    } else {
        "other"
    }
}

/// MTU per interface name: sysfs on Linux and Android, `ifconfig` on the BSDs and macOS.
fn interface_mtus() -> std::collections::HashMap<String, u32> {
    let mut out = std::collections::HashMap::new();
    if cfg!(any(target_os = "linux", target_os = "android")) {
        if let Ok(dir) = std::fs::read_dir("/sys/class/net") {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(m) = std::fs::read_to_string(e.path().join("mtu"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                {
                    out.insert(name, m);
                }
            }
        }
    } else if cfg!(target_os = "macos") {
        if let Ok(o) = std::process::Command::new("/sbin/ifconfig")
            .arg("-a")
            .output()
        {
            out = parse_ifconfig_mtus(&String::from_utf8_lossy(&o.stdout));
        }
    }
    out
}

/// `en0: flags=8863<UP,...> mtu 1500` lines out of `ifconfig -a`.
pub(crate) fn parse_ifconfig_mtus(text: &str) -> std::collections::HashMap<String, u32> {
    text.lines()
        .filter(|l| !l.starts_with(char::is_whitespace))
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let mtu = rest.split_whitespace().skip_while(|w| *w != "mtu").nth(1)?;
            Some((name.to_string(), mtu.parse().ok()?))
        })
        .collect()
}

/// The feature summary (see the module docs).
pub(crate) fn features_json() -> Value {
    json!({
        "engine": {
            "version": env!("CARGO_PKG_VERSION"),
            "kind": crate::mesh_api::kind(),
            "uptime_s": uptime_s(),
        },
        "detailed_logs": level_json(),
        "log_levels": crate::log_levels::levels_json(),
        "host": host_json(),
        "remote_play": crate::rp_stream_api::diag_summary(),
        "play_together": crate::join::diag_summary(),
        "pad_link": crate::pad_link::diag_summary(),
        "go_live": crate::rp_golive::diag_summary(),
        "remote_connections": crate::remote::api::diag_summary(),
        "mesh": crate::mesh_api::diag_summary(),
    })
}

pub async fn features() -> Response {
    // The Tailscale check and the stores read the disk and the network stack: off the reactor.
    match tokio::task::spawn_blocking(features_json).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => crate::json_err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("feature summary failed: {e}"),
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detailed_logs_never_run_past_a_day() {
        let now = 1_800_000_000_000;
        let day = MAX_DETAILED.as_millis() as u64;
        assert_eq!(clamp_until(None, now), now + day);
        assert_eq!(clamp_until(Some(now + 3_600_000), now), now + 3_600_000);
        assert_eq!(clamp_until(Some(now + 30 * day), now), now + day);
    }

    /// Detail lines reach the log (at debug) only while the switch is on.
    #[test]
    fn detail_lines_follow_the_switch() {
        let id = std::process::id();
        let off = format!("detail-off-{id}");
        let on = format!("detail-on-{id}");
        ps5upload_core::log::set_verbose_until(0);
        crate::log_detail!("{off}");
        ps5upload_core::log::set_verbose_until(now_ms() + 60_000);
        // A test flooding the debug ring in parallel could push one line out: a few tries.
        let seen = (0..3).any(|_| {
            crate::log_detail!("{on}");
            crate::engine_log::tail_since(0)
                .iter()
                .any(|e| e.msg == on && e.level == "debug")
        });
        ps5upload_core::log::set_verbose_until(0);
        assert!(seen);
        assert!(!crate::engine_log::tail_since(0)
            .iter()
            .any(|e| e.msg == off));
    }

    #[test]
    fn a_custom_server_shows_only_its_host() {
        use crate::rp_golive::server_host;
        assert_eq!(
            server_host("rtmp://live.example.com:1935/app/sk_live_SECRETKEY?x=1").as_deref(),
            Some("live.example.com:1935")
        );
        assert_eq!(
            server_host("srt://user:hunter2@ingest.example.net:9000?streamid=SECRET").as_deref(),
            Some("ingest.example.net:9000")
        );
        assert_eq!(server_host("").as_deref(), None);
    }

    /// The summary as a whole carries no secret: a go-live destination with a key, an open
    /// phone link, Play Together with an invite and a guest are all reduced to counts.
    #[test]
    fn the_feature_summary_carries_no_keys() {
        let text = features_json().to_string();
        for k in [
            "\"key\"",
            "\"token\"",
            "\"pad_token\"",
            "\"invite\"",
            "\"code\"",
            "\"password\"",
            "\"url\"",
        ] {
            assert!(!text.contains(k), "{k} in {text}");
        }
        // Every section is there.
        let v: Value = serde_json::from_str(&text).unwrap();
        for s in [
            "engine",
            "detailed_logs",
            "remote_play",
            "play_together",
            "pad_link",
            "go_live",
            "remote_connections",
            "mesh",
            "log_levels",
            "host",
        ] {
            assert!(v.get(s).is_some(), "{s} missing");
        }
        // Interfaces by kind, never by address.
        for i in v["host"]["network_interfaces"].as_array().unwrap() {
            assert!(i.get("kind").is_some() && i.get("name").is_none(), "{i}");
        }
    }

    #[test]
    fn interfaces_by_kind_and_mtu() {
        assert_eq!(interface_kind("en0"), "ethernet_or_wifi");
        assert_eq!(interface_kind("utun4"), "vpn");
        assert_eq!(interface_kind("wlan0"), "wifi");
        assert_eq!(interface_kind("enp3s0"), "ethernet");
        assert_eq!(interface_kind("docker0"), "virtual");
        assert_eq!(interface_kind("lo0"), "loopback");
        let m = parse_ifconfig_mtus(
            "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\toptions=1203<RXCSUM>\nen0: flags=8863<UP,BROADCAST> mtu 1500\n\tether aa:bb:cc:dd:ee:ff\n",
        );
        assert_eq!(m.get("lo0"), Some(&16384));
        assert_eq!(m.get("en0"), Some(&1500));
        assert_eq!(m.len(), 2);
    }
}
