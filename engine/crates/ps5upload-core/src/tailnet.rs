//! "Put this PS5 on Tailscale": the helper's tailnet node (`payload/src/tailnet_node.c`,
//! management method 198 `tailnet`; the C core is `tailnet/` in the repo root).
//!
//! The console then serves exactly TCP 9295 + 9120 and UDP 9296 + 9302 to the user's tailnet
//! (an allow-list in the core AND the tailnet's own ACL). Every call answers `Ok(None)` when
//! the helper predates this feature (`ERR_UNKNOWN_METHOD`); a refusal is an `Err` whose cause
//! carries the helper's `{"ok":false,"err":..}`.

use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use crate::console_files::is_unsupported;
use crate::mgmt::{self, m};

/// Tailscale's own control server, used when the user did not pick another one (Headscale).
pub const DEFAULT_CONTROL_URL: &str = "https://controlplane.tailscale.com";

/// What the app asks of the console's node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Op {
    #[default]
    Status,
    /// Join (or stay joined). Empty fields keep what the console has saved.
    Enable {
        control_url: String,
        hostname: String,
        authkey: String,
    },
    /// Leave the tailnet, keep the keys (turning it back on needs no new sign-in).
    Disable,
    /// Leave and forget the keys: the next enable signs in again.
    Logout,
}

impl Op {
    pub fn parse(op: &str, control_url: &str, hostname: &str, authkey: &str) -> Result<Op> {
        Ok(match op {
            "" | "status" => Op::Status,
            "enable" => Op::Enable {
                control_url: control_url.trim().to_string(),
                hostname: hostname.trim().to_string(),
                authkey: authkey.trim().to_string(),
            },
            "disable" => Op::Disable,
            "logout" => Op::Logout,
            other => bail!("unknown op {other:?}"),
        })
    }

    pub fn mutates(&self) -> bool {
        !matches!(self, Op::Status)
    }
}

/// The host part of an http(s) URL, or None.
fn url_host(url: &str) -> Option<(String, u16)> {
    let (rest, port) = match url.strip_prefix("https://") {
        Some(r) => (r, 443),
        None => (url.strip_prefix("http://")?, 80),
    };
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    if hostport.is_empty() || hostport.starts_with('[') {
        return None;
    }
    match hostport.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((hostport.to_string(), port)),
    }
}

/// The console has no DNS resolver of its own: the engine resolves the control host and hands
/// the console an IPv4 address to dial when its own lookup fails (TLS still checks the name).
pub fn resolve_control_v4(url: &str) -> Option<String> {
    let (host, port) = url_host(url)?;
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Some(host);
    }
    let addrs = (host.as_str(), port).to_socket_addrs().ok()?;
    addrs.into_iter().find_map(|a| match a {
        SocketAddr::V4(v4) => Some(v4.ip().to_string()),
        SocketAddr::V6(_) => None,
    })
}

/// The request body the helper reads.
pub fn request_body(op: &Op) -> Value {
    match op {
        Op::Status => json!({ "op": "status" }),
        Op::Disable => json!({ "op": "disable" }),
        Op::Logout => json!({ "op": "logout" }),
        Op::Enable {
            control_url,
            hostname,
            authkey,
        } => {
            let mut b = Map::new();
            b.insert("op".into(), json!("enable"));
            if !control_url.is_empty() {
                b.insert("control_url".into(), json!(control_url));
            }
            if !hostname.is_empty() {
                b.insert("hostname".into(), json!(hostname));
            }
            if !authkey.is_empty() {
                b.insert("authkey".into(), json!(authkey));
            }
            let url = if control_url.is_empty() {
                DEFAULT_CONTROL_URL
            } else {
                control_url.as_str()
            };
            if let Some(ip) = resolve_control_v4(url) {
                b.insert("control_addr".into(), json!(ip));
            }
            Value::Object(b)
        }
    }
}

/// One call to the console's node. `Ok(None)`: the helper is older than this feature.
pub fn console(addr: &str, op: &Op) -> Result<Option<Value>> {
    if let Op::Enable { control_url, .. } = op {
        if !control_url.is_empty() && url_host(control_url).is_none() {
            bail!("control URL must be http:// or https://");
        }
    }
    let body = serde_json::to_vec(&request_body(op))?;
    match mgmt::call(addr, m::TAILNET, &body) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
        Err(e) if is_unsupported(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The console's tailnet address when its node runs, for "use it when away" (100.64.0.0/10).
pub fn tailnet_ip(status: &Value) -> Option<String> {
    let ip = status.get("ip")?.as_str()?;
    let v4: std::net::Ipv4Addr = ip.parse().ok()?;
    let o = v4.octets();
    (o[0] == 100 && (o[1] & 0xc0) == 64).then(|| ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_parse() {
        assert_eq!(Op::parse("", "", "", "").unwrap(), Op::Status);
        assert_eq!(Op::parse("logout", "", "", "").unwrap(), Op::Logout);
        assert!(Op::parse("reboot", "", "", "").is_err());
        assert!(!Op::Status.mutates());
        assert!(Op::Disable.mutates());
    }

    #[test]
    fn url_hosts() {
        assert_eq!(
            url_host("https://controlplane.tailscale.com"),
            Some(("controlplane.tailscale.com".into(), 443))
        );
        assert_eq!(
            url_host("http://hs.example:8080/x"),
            Some(("hs.example".into(), 8080))
        );
        assert_eq!(url_host("ftp://x"), None);
        assert_eq!(url_host("https://"), None);
        assert_eq!(
            resolve_control_v4("http://192.0.2.7:8080").as_deref(),
            Some("192.0.2.7")
        );
    }

    #[test]
    fn enable_body_keeps_empty_fields_out_and_never_logs_key() {
        let b = request_body(&Op::Enable {
            control_url: "http://192.0.2.7:8080".into(),
            hostname: String::new(),
            authkey: "tskey-x".into(),
        });
        assert_eq!(b["op"], "enable");
        assert_eq!(b["control_addr"], "192.0.2.7");
        assert!(b.get("hostname").is_none());
        assert_eq!(b["authkey"], "tskey-x");
    }

    #[test]
    fn tailnet_ip_only_cgnat_range() {
        assert_eq!(
            tailnet_ip(&json!({"ip":"100.64.0.5"})).as_deref(),
            Some("100.64.0.5")
        );
        assert_eq!(
            tailnet_ip(&json!({"ip":"100.127.255.1"})).as_deref(),
            Some("100.127.255.1")
        );
        assert_eq!(tailnet_ip(&json!({"ip":"100.128.0.1"})), None);
        assert_eq!(tailnet_ip(&json!({"ip":"192.168.1.2"})), None);
        assert_eq!(tailnet_ip(&json!({"ip":""})), None);
    }
}
