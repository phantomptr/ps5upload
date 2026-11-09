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
    /// The device name, any time (applied now while on; the address stays).
    Rename { hostname: String },
    /// Renew the sign-in keeping the node (its address and name): a login URL follows.
    Renew,
    /// Which tailnet devices may reach the PS5: `mode` "all" or "listed" (empty keeps it), and
    /// the list as "100.a.b.c,100.d.e.f".
    Allow { mode: String, list: String },
    /// Add one device (its tailnet IPv4) to the list.
    AllowAdd { ip: String },
    /// Stop showing a device the list turned away (`blocked_recent`), until the helper restarts.
    BlockedIgnore { ip: String },
}

/// The fields of an op request beyond its name.
#[derive(Clone, Debug, Default)]
pub struct OpFields<'a> {
    pub control_url: &'a str,
    pub hostname: &'a str,
    pub authkey: &'a str,
    pub allow_mode: &'a str,
    pub allow: &'a str,
    pub ip: &'a str,
}

impl Op {
    pub fn parse(op: &str, control_url: &str, hostname: &str, authkey: &str) -> Result<Op> {
        Self::parse_fields(
            op,
            &OpFields {
                control_url,
                hostname,
                authkey,
                ..Default::default()
            },
        )
    }

    pub fn parse_fields(op: &str, f: &OpFields) -> Result<Op> {
        let (control_url, hostname, authkey) = (f.control_url, f.hostname, f.authkey);
        Ok(match op {
            "" | "status" => Op::Status,
            "enable" => Op::Enable {
                control_url: control_url.trim().to_string(),
                hostname: hostname.trim().to_string(),
                authkey: authkey.trim().to_string(),
            },
            "disable" => Op::Disable,
            "logout" => Op::Logout,
            "rename" => {
                let h = hostname.trim();
                if h.is_empty() || h.len() > 63 {
                    bail!("a device name is 1 to 63 characters");
                }
                Op::Rename {
                    hostname: h.to_string(),
                }
            }
            "renew" => Op::Renew,
            "allow" => {
                let mode = f.allow_mode.trim();
                if !mode.is_empty() && mode != "all" && mode != "listed" {
                    bail!("allow_mode must be \"all\" or \"listed\"");
                }
                for ip in f.allow.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if tailnet_ip(&json!({ "ip": ip })).is_none() {
                        bail!("{ip:?} is not a Tailscale address (100.64.0.0/10)");
                    }
                }
                Op::Allow {
                    mode: mode.to_string(),
                    list: f.allow.trim().to_string(),
                }
            }
            "allow_add" | "blocked_ignore" => {
                if tailnet_ip(&json!({ "ip": f.ip.trim() })).is_none() {
                    bail!("ip must be a Tailscale address (100.64.0.0/10)");
                }
                let ip = f.ip.trim().to_string();
                if op == "allow_add" {
                    Op::AllowAdd { ip }
                } else {
                    Op::BlockedIgnore { ip }
                }
            }
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
        Op::Rename { hostname } => json!({ "op": "rename", "hostname": hostname }),
        Op::Renew => json!({ "op": "renew" }),
        Op::Allow { mode, list } => {
            let mut b = Map::new();
            b.insert("op".into(), json!("allow"));
            if !mode.is_empty() {
                b.insert("allow_mode".into(), json!(mode));
            }
            b.insert("allow".into(), json!(list));
            Value::Object(b)
        }
        Op::AllowAdd { ip } => json!({ "op": "allow_add", "ip": ip }),
        Op::BlockedIgnore { ip } => json!({ "op": "blocked_ignore", "ip": ip }),
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
        assert_eq!(
            Op::parse("rename", "", " den-ps5u ", "").unwrap(),
            Op::Rename {
                hostname: "den-ps5u".into()
            }
        );
        assert!(Op::parse("rename", "", " ", "").is_err());
        assert_eq!(Op::parse("renew", "", "", "").unwrap(), Op::Renew);
        let f = OpFields {
            allow_mode: "listed",
            allow: "100.64.0.5, 100.101.2.3",
            ..Default::default()
        };
        let op = Op::parse_fields("allow", &f).unwrap();
        assert_eq!(request_body(&op)["allow_mode"], "listed");
        let bad = OpFields {
            allow: "192.168.1.2",
            ..Default::default()
        };
        assert!(Op::parse_fields("allow", &bad).is_err());
        let mode = OpFields {
            allow_mode: "some",
            ..Default::default()
        };
        assert!(Op::parse_fields("allow", &mode).is_err());
        let add = OpFields {
            ip: "100.64.0.7",
            ..Default::default()
        };
        assert_eq!(
            request_body(&Op::parse_fields("allow_add", &add).unwrap())["ip"],
            "100.64.0.7"
        );
        assert!(Op::parse_fields("allow_add", &OpFields::default()).is_err());
        assert_eq!(
            request_body(&Op::parse_fields("blocked_ignore", &add).unwrap()),
            json!({ "op": "blocked_ignore", "ip": "100.64.0.7" })
        );
        assert!(Op::parse_fields("blocked_ignore", &OpFields::default()).is_err());
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
