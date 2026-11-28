//! A USB Ethernet adapter on the PS5 (EXPERIMENTAL): the helper drives a Realtek 2.5G/5G/10G
//! adapter with its own driver and TCP stack, and serves the helper's AVA1 port (9120) and
//! Minion (9180) on the adapter's own address, each connection joined to 127.0.0.1 on the
//! console. The console's built-in network is not touched. Management method 206 `usbnet`
//! (`payload/include/usbnet.h` has the contract); the engine's routing is
//! `ps5upload-engine/src/usbnet_engine.rs`.
//!
//! Every call answers `Ok(None)` when the helper predates this feature (`ERR_UNKNOWN_METHOD`);
//! a refusal is an `Err` whose cause carries the helper's `{"ok":false,"err":..}`.

use std::net::Ipv4Addr;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::console_files::is_unsupported;
use crate::mgmt::{self, m};

/// What the app asks of the console's adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Op {
    #[default]
    Status,
    /// The setting, kept on the console.
    Enable,
    Disable,
    /// A static address (`ip` "" goes back to DHCP; then `mask` and `gateway` are not sent).
    Address {
        ip: String,
        mask: String,
        gateway: String,
    },
}

/// A dotted IPv4 address the helper takes: four plain numbers, no leading zeros, no spaces
/// (`usbnet_ipv4_ok` in the helper).
pub fn ipv4_ok(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

impl Op {
    /// `op` with its fields as the route takes them. `mask` and `gateway` are optional with a
    /// static `ip` (the helper keeps its defaults), but must be addresses when given.
    pub fn parse(op: &str, ip: &str, mask: &str, gateway: &str) -> Result<Op> {
        Ok(match op {
            "" | "status" => Op::Status,
            "enable" => Op::Enable,
            "disable" => Op::Disable,
            "address" => {
                let (ip, mask, gateway) = (ip.trim(), mask.trim(), gateway.trim());
                if !ip.is_empty() {
                    if !ipv4_ok(ip) {
                        bail!("ip must be an IPv4 address like 192.168.1.50 (or empty for DHCP)");
                    }
                    for (name, v) in [("mask", mask), ("gateway", gateway)] {
                        if !v.is_empty() && !ipv4_ok(v) {
                            bail!("{name} must be an IPv4 address");
                        }
                    }
                }
                Op::Address {
                    ip: ip.to_string(),
                    mask: mask.to_string(),
                    gateway: gateway.to_string(),
                }
            }
            other => bail!("unknown op {other:?}"),
        })
    }

    pub fn mutates(&self) -> bool {
        !matches!(self, Op::Status)
    }
}

/// The request body the helper reads.
pub fn request_body(op: &Op) -> Value {
    match op {
        Op::Status => json!({ "op": "status" }),
        Op::Enable => json!({ "op": "enable" }),
        Op::Disable => json!({ "op": "disable" }),
        Op::Address { ip, .. } if ip.is_empty() => json!({ "op": "address", "ip": "" }),
        Op::Address { ip, mask, gateway } => {
            let mut b = json!({ "op": "address", "ip": ip });
            if !mask.is_empty() {
                b["mask"] = json!(mask);
            }
            if !gateway.is_empty() {
                b["gateway"] = json!(gateway);
            }
            b
        }
    }
}

/// One call to the console's adapter. `Ok(None)`: the helper is older than this feature.
pub fn console(addr: &str, op: &Op) -> Result<Option<Value>> {
    let body = serde_json::to_vec(&request_body(op))?;
    match mgmt::call(addr, m::USBNET, &body) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
        Err(e) if is_unsupported(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The adapter while it serves: where this computer may reach the helper's AVA1 port on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Brief {
    pub ip: Ipv4Addr,
    pub port: u16,
    /// The link's speed (Mb/s; 0 when the helper did not say).
    pub mbps: u32,
    /// "RTL8156B", "RTL8157", "RTL8159", or "" when not said.
    pub chip: String,
}

/// What a status said about the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Nothing: a helper from before this feature (no `usbnet` key).
    Absent,
    /// The helper knows the feature, and no adapter serves now.
    NotServing,
    Serving(Brief),
}

fn brief_fields(v: &Value, mbps_key: &str, chip: Option<&str>) -> Option<Brief> {
    let ip: Ipv4Addr = v.get("ip")?.as_str()?.parse().ok()?;
    if ip.is_unspecified() || ip.is_loopback() {
        return None;
    }
    let port = v
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .filter(|&p| p != 0)
        .unwrap_or(9120);
    Some(Brief {
        ip,
        port,
        mbps: v
            .get(mbps_key)
            .and_then(Value::as_u64)
            .map_or(0, |n| n.min(u32::MAX as u64) as u32),
        chip: chip
            .or_else(|| v.get("chip").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
    })
}

/// node.status's `usbnet`: `{"ip","port","mbps","chip"}` while the adapter serves, `null`
/// otherwise, absent from an older helper.
pub fn from_node_status(status: &Value) -> Seen {
    match status.get("usbnet") {
        None => Seen::Absent,
        Some(Value::Null) => Seen::NotServing,
        Some(v) => brief_fields(v, "mbps", None).map_or(Seen::NotServing, Seen::Serving),
    }
}

/// The method's own answer (`state` "ready" with an address is serving).
pub fn from_status(status: &Value) -> Seen {
    if status.get("state").and_then(Value::as_str) != Some("ready") {
        return Seen::NotServing;
    }
    let chip = status
        .get("adapter")
        .and_then(|a| a.get("chip"))
        .and_then(Value::as_str);
    brief_fields(status, "link_mbps", chip).map_or(Seen::NotServing, Seen::Serving)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_parse_and_bodies() {
        assert_eq!(Op::parse("", "", "", "").unwrap(), Op::Status);
        assert_eq!(Op::parse("enable", "", "", "").unwrap(), Op::Enable);
        assert!(!Op::Status.mutates());
        assert!(Op::Disable.mutates());
        assert!(Op::parse("reboot", "", "", "").is_err());
        let a = Op::parse("address", " 192.168.1.50 ", "255.255.255.0", "").unwrap();
        assert_eq!(
            request_body(&a),
            json!({ "op": "address", "ip": "192.168.1.50", "mask": "255.255.255.0" })
        );
        // Back to DHCP: the address alone, empty.
        let dhcp = Op::parse("address", "", "255.255.255.0", "x").unwrap();
        assert_eq!(request_body(&dhcp), json!({ "op": "address", "ip": "" }));
        assert!(Op::parse("address", "192.168.1.050", "", "").is_err());
        assert!(Op::parse("address", "192.168.1.5", "", "gw").is_err());
        assert!(Op::parse("address", "300.1.1.1", "", "").is_err());
    }

    #[test]
    fn ipv4_is_four_plain_numbers() {
        assert!(ipv4_ok("10.0.0.1"));
        assert!(ipv4_ok("255.255.255.0"));
        for bad in [
            "",
            "10.0.0",
            "10.0.0.1.2",
            "01.0.0.1",
            " 10.0.0.1",
            "10.0.0.256",
            "a.b.c.d",
        ] {
            assert!(!ipv4_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_node_status_brief_reads_three_ways() {
        assert_eq!(from_node_status(&json!({ "version": "x" })), Seen::Absent);
        assert_eq!(
            from_node_status(&json!({ "usbnet": null })),
            Seen::NotServing
        );
        assert_eq!(
            from_node_status(
                &json!({ "usbnet": { "ip": "192.168.1.60", "port": 9120, "mbps": 2500, "chip": "RTL8156B" } })
            ),
            Seen::Serving(Brief {
                ip: "192.168.1.60".parse().unwrap(),
                port: 9120,
                mbps: 2500,
                chip: "RTL8156B".into(),
            })
        );
        // A brief with no usable address is not serving.
        assert_eq!(
            from_node_status(&json!({ "usbnet": { "ip": "0.0.0.0" } })),
            Seen::NotServing
        );
    }

    #[test]
    fn the_status_answer_serves_only_when_ready() {
        let ready = json!({
            "ok": true, "state": "ready", "ip": "10.0.0.7", "port": 9120, "link_mbps": 5000,
            "adapter": { "usb_id": "0bda:8157", "chip": "RTL8157", "max_mbps": 5000 },
        });
        assert_eq!(
            from_status(&ready),
            Seen::Serving(Brief {
                ip: "10.0.0.7".parse().unwrap(),
                port: 9120,
                mbps: 5000,
                chip: "RTL8157".into(),
            })
        );
        let mut no_link = ready.clone();
        no_link["state"] = json!("no_link");
        assert_eq!(from_status(&no_link), Seen::NotServing);
        assert_eq!(
            from_status(&json!({ "ok": true, "state": "ready", "ip": "" })),
            Seen::NotServing
        );
    }
}
