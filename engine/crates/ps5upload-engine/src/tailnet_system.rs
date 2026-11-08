//! The system's own Tailscale app on this device, and the names the app gives tailnet devices.
//!
//! * Detection is one interface scan: an address in 100.64.0.0/10 or fd7a:115c:a1e0::/48 is
//!   Tailscale's (the engine's own node is userspace and has no interface, so it never counts).
//!   Cached for a few seconds; nothing runs in the background.
//! * The peer list, where it can be read, comes from the Tailscale CLI (`tailscale status --json`:
//!   the macOS app's CLI, the Windows service's, tailscaled's on Linux). Only on demand, cached,
//!   and never on a phone (there, the interface address is all there is).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

const DETECT_TTL: Duration = Duration::from_secs(10);
const PEERS_TTL: Duration = Duration::from_secs(30);
const CLI_TIMEOUT: Duration = Duration::from_secs(2);

/// Is this a Tailscale address (CGNAT 100.64.0.0/10, or Tailscale's ULA fd7a:115c:a1e0::/48)?
pub fn is_tailscale_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// The system's Tailscale as seen from this device's interfaces.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SystemTs {
    pub detected: bool,
    /// The interface that carries it ("utun4", "tailscale0", "Tailscale").
    pub iface: String,
    /// This device's tailnet IPv4 there, when it has one.
    pub ipv4: Option<Ipv4Addr>,
}

/// Detection from a list of (interface, address): the pure part, tested with fake interfaces.
pub fn detect_from(ifaces: &[(String, IpAddr)]) -> SystemTs {
    let mut out = SystemTs::default();
    for (name, ip) in ifaces {
        if !is_tailscale_ip(ip) || ip.is_loopback() {
            continue;
        }
        if !out.detected {
            out.detected = true;
            out.iface = name.clone();
        }
        if let IpAddr::V4(v4) = ip {
            if out.ipv4.is_none() {
                out.ipv4 = Some(*v4);
                out.iface = name.clone();
            }
        }
    }
    out
}

static DETECTED: Mutex<Option<(Instant, SystemTs)>> = Mutex::new(None);

/// The system's Tailscale now (cached for a few seconds).
pub fn detect() -> SystemTs {
    if let Ok(t) = std::env::var("PS5UPLOAD_FAKE_SYSTEM_TAILSCALE") {
        // Tests and demos: "100.101.102.103" pretends the system app is on with that address.
        return detect_from(&[(
            "fake0".into(),
            t.parse()
                .unwrap_or(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))),
        )]);
    }
    let mut g = DETECTED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, v)) = g.as_ref() {
        if at.elapsed() < DETECT_TTL {
            return v.clone();
        }
    }
    let list: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| {
            let ip = i.ip();
            (i.name, ip)
        })
        .collect();
    let v = detect_from(&list);
    *g = Some((Instant::now(), v.clone()));
    v
}

/// One device in the system Tailscale's peer list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SysPeer {
    /// The MagicDNS name's first label ("living-room-ps5u").
    pub name: String,
    pub ipv4: Option<Ipv4Addr>,
    pub online: bool,
}

/// What `tailscale status --json` says: the backend state and the peers.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SysStatus {
    pub state: String,
    pub self_ipv4: Option<Ipv4Addr>,
    pub peers: Vec<SysPeer>,
}

fn first_v4(v: &Value) -> Option<Ipv4Addr> {
    v.get("TailscaleIPs")?
        .as_array()?
        .iter()
        .filter_map(|x| x.as_str()?.parse::<Ipv4Addr>().ok())
        .next()
}

fn label(v: &Value) -> String {
    let dns = v.get("DNSName").and_then(Value::as_str).unwrap_or("");
    let first = dns.split('.').next().unwrap_or("");
    if !first.is_empty() {
        return first.to_string();
    }
    v.get("HostName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The pure part of reading `tailscale status --json`.
pub fn parse_status(text: &str) -> Option<SysStatus> {
    let v: Value = serde_json::from_str(text).ok()?;
    let mut out = SysStatus {
        state: v
            .get("BackendState")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        self_ipv4: v.get("Self").and_then(first_v4),
        peers: Vec::new(),
    };
    if let Some(map) = v.get("Peer").and_then(Value::as_object) {
        for p in map.values() {
            out.peers.push(SysPeer {
                name: label(p),
                ipv4: first_v4(p),
                online: p.get("Online").and_then(Value::as_bool).unwrap_or(false),
            });
        }
    }
    out.peers.sort_by(|a, b| a.name.cmp(&b.name));
    Some(out)
}

fn cli_candidates() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec![
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
        ]
    } else if cfg!(windows) {
        vec![r"C:\Program Files\Tailscale\tailscale.exe", "tailscale.exe"]
    } else {
        vec![
            "/usr/bin/tailscale",
            "/usr/local/bin/tailscale",
            "tailscale",
        ]
    }
}

fn run_cli() -> Option<String> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return None;
    }
    for exe in cli_candidates() {
        if (exe.starts_with('/') || exe.contains('\\')) && !std::path::Path::new(exe).exists() {
            continue;
        }
        let Ok(mut child) = std::process::Command::new(exe)
            .args(["status", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            continue;
        };
        let t0 = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if t0.elapsed() < CLI_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let out = child.wait_with_output().ok()?;
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    None
}

static PEERS: Mutex<Option<(Instant, Option<SysStatus>)>> = Mutex::new(None);

/// The system Tailscale's own status, when its CLI can be read (cached; None on phones).
pub fn status() -> Option<SysStatus> {
    let mut g = PEERS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, v)) = g.as_ref() {
        if at.elapsed() < PEERS_TTL {
            return v.clone();
        }
    }
    let v = run_cli().and_then(|t| parse_status(&t));
    *g = Some((Instant::now(), v.clone()));
    v
}

// ---- names ----

/// A tailnet device name as a DNS label: lowercase a-z, 0-9 and hyphens, at most 63, no hyphen
/// at either end. Anything else becomes a hyphen (runs collapse). Empty when nothing is left.
pub fn sanitize_label(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    let mut t: String = t.chars().take(63).collect();
    while t.ends_with('-') {
        t.pop();
    }
    t
}

/// The app's suffix on the names it gives.
pub const SUFFIX: &str = "-ps5u";

/// `<base>-ps5u` as a DNS label, keeping the suffix whole when the base is long. `fallback`
/// when the base has nothing usable.
pub fn default_name(base: &str, fallback: &str) -> String {
    let mut b = sanitize_label(base);
    if b.is_empty() {
        b = fallback.to_string();
    }
    let room = 63 - SUFFIX.len();
    if b.len() > room {
        b.truncate(room);
        while b.ends_with('-') {
            b.pop();
        }
    }
    if b.ends_with(SUFFIX) {
        return b;
    }
    format!("{b}{SUFFIX}")
}

/// This device's default name: its own name (the OS host name, or the name set for it in the
/// app) + "-ps5u".
pub fn default_device_name() -> String {
    default_name(&crate::mesh_api::device_name(), "device")
}

/// A console's default name: its own name (DDP `host-name`) + "-ps5u".
pub fn default_console_name(ddp_host_name: &str) -> String {
    default_name(ddp_host_name, "ps5")
}

/// Tailscale's control server makes names unique itself: a second device asking for a name that
/// is taken gets it with "-1", "-2"… added to its MagicDNS name (Headscale adds its own suffix).
/// The app relies on that and shows the name actually given. This says whether control changed
/// the name we asked for (`assigned` is the MagicDNS name, maybe with the tailnet's domain).
pub fn renamed_by_control(asked: &str, assigned: &str) -> Option<String> {
    let first = assigned
        .trim_end_matches('.')
        .split('.')
        .next()
        .unwrap_or("");
    let want = sanitize_label(asked);
    if first.is_empty() || want.is_empty() || first == want {
        return None;
    }
    Some(first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_dns_safe() {
        assert_eq!(sanitize_label("Living Room PS5"), "living-room-ps5");
        assert_eq!(
            sanitize_label("  --Yun's MacBook Pro!!  "),
            "yun-s-macbook-pro"
        );
        assert_eq!(sanitize_label("PS5-Ä_Ö"), "ps5");
        assert_eq!(sanitize_label("日本"), "");
        assert_eq!(sanitize_label(&"a".repeat(80)).len(), 63);
        let long = format!("{}-b", "a".repeat(62));
        assert_eq!(sanitize_label(&long), "a".repeat(62)); // no hyphen left at the end
    }

    #[test]
    fn default_names_carry_the_suffix() {
        assert_eq!(
            default_console_name("Living Room PS5"),
            "living-room-ps5-ps5u"
        );
        assert_eq!(default_console_name(""), "ps5-ps5u");
        assert_eq!(default_name("my-ps5u", "x"), "my-ps5u");
        assert_eq!(default_name("Den", "x"), "den-ps5u");
        let long = default_name(&"b".repeat(100), "x");
        assert_eq!(long.len(), 63);
        assert!(long.ends_with("-ps5u"));
    }

    #[test]
    fn control_dedup_is_reported_not_redone() {
        assert_eq!(
            renamed_by_control("den-ps5u", "den-ps5u.tail1234.ts.net."),
            None
        );
        assert_eq!(
            renamed_by_control("den-ps5u", "den-ps5u-1.tail1234.ts.net."),
            Some("den-ps5u-1".into())
        );
        assert_eq!(renamed_by_control("Den PS5u", "den-ps5u"), None);
        assert_eq!(renamed_by_control("x", ""), None);
    }

    #[test]
    fn detects_the_system_app_from_interfaces() {
        let none = detect_from(&[
            ("en0".into(), "192.168.1.20".parse().unwrap()),
            ("lo0".into(), "127.0.0.1".parse().unwrap()),
            // 100.128/9 is not Tailscale's
            ("en1".into(), "100.128.0.1".parse().unwrap()),
        ]);
        assert!(!none.detected);
        let v4 = detect_from(&[
            ("en0".into(), "192.168.1.20".parse().unwrap()),
            ("utun4".into(), "100.101.102.103".parse().unwrap()),
        ]);
        assert!(v4.detected);
        assert_eq!(v4.iface, "utun4");
        assert_eq!(v4.ipv4, Some(Ipv4Addr::new(100, 101, 102, 103)));
        let v6 = detect_from(&[("tailscale0".into(), "fd7a:115c:a1e0::1234".parse().unwrap())]);
        assert!(v6.detected && v6.ipv4.is_none());
        assert!(!detect_from(&[("x".into(), "fd7a:115c:a1e1::1".parse().unwrap())]).detected);
    }

    #[test]
    fn reads_the_cli_status() {
        let text = r#"{"BackendState":"Running","Self":{"TailscaleIPs":["100.64.0.2","fd7a:115c:a1e0::2"]},
            "Peer":{"nodekey:a":{"HostName":"Living Room","DNSName":"living-room-ps5u.tail1.ts.net.",
            "TailscaleIPs":["100.64.0.9"],"Online":true},
            "nodekey:b":{"HostName":"phone","DNSName":"","TailscaleIPs":["fd7a:115c:a1e0::3"],"Online":false}}}"#;
        let st = parse_status(text).unwrap();
        assert_eq!(st.state, "Running");
        assert_eq!(st.self_ipv4, Some(Ipv4Addr::new(100, 64, 0, 2)));
        assert_eq!(st.peers.len(), 2);
        assert_eq!(st.peers[0].name, "living-room-ps5u");
        assert_eq!(st.peers[0].ipv4, Some(Ipv4Addr::new(100, 64, 0, 9)));
        assert!(st.peers[0].online);
        assert_eq!(st.peers[1].name, "phone");
        assert_eq!(st.peers[1].ipv4, None);
        assert!(parse_status("not json").is_none());
    }
}
