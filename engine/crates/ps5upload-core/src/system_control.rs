//! Power control + telemetry + user enumeration over FTX2.
//!
//! These are thin client wrappers around the new SystemControl /
//! PowerTelemetry / UserList frames the payload added in this round.
//! Each call opens a fresh management-port connection (caller passes
//! the `host:9114` address), sends one frame, parses the ACK.
//!
//! Power control is treated specially: `reboot`, `shutdown`, and
//! `standby` are destructive (the PS5's network stack tears down as
//! part of these). The payload sends the ACK *before* invoking the
//! Sony API, but the TCP RST often beats the ACK to our side. We
//! treat "no ACK + connection drop within 1s of send" as success
//! for those actions — anything else would be misleading UX.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::connection::Connection;

/// Action passed to the SystemControl frame.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerAction {
    Reboot,
    Shutdown,
    Standby,
    /// Defer the auto-sleep timer by one tick. Non-destructive.
    Tick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemControlAck {
    /// True when the Sony API call returned success (or, for the
    /// destructive actions, when our pre-call ACK landed).
    #[serde(default)]
    pub ok: bool,
    /// Echo of the action we requested. Lets the renderer match an
    /// ACK to a pending request without tracking trace IDs.
    #[serde(default)]
    pub action: Option<String>,
    /// Sony API error string for the non-destructive `tick` path,
    /// or `standby_unavailable` when dlsym failed.
    #[serde(default)]
    pub err: Option<String>,
    /// Sony API error code (when `err` is set).
    #[serde(default)]
    pub code: Option<i32>,
}

/// Send a power-control action.
///
/// For `Reboot`, `Shutdown`, and `Standby` the caller should treat a
/// connection-drop *before* receiving the ACK as success — these
/// actions tear down the network. Both this function and the
/// renderer's reboot button apply that policy: we return Ok on
/// EOF/connection-reset for those three.
pub fn system_control(addr: &str, action: PowerAction) -> Result<SystemControlAck> {
    let body = serde_json::json!({
        "action": match action {
            PowerAction::Reboot => "reboot",
            PowerAction::Shutdown => "shutdown",
            PowerAction::Standby => "standby",
            PowerAction::Tick => "tick",
        },
    });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::SystemControl, &body)?;
    match c.recv_frame() {
        Ok((hdr, resp)) => {
            let ft = hdr.frame_type().unwrap_or(FrameType::Error);
            if ft == FrameType::Error {
                bail!(
                    "payload rejected SYSTEM_CONTROL: {}",
                    String::from_utf8_lossy(&resp)
                );
            }
            if ft != FrameType::SystemControlAck {
                bail!("expected SYSTEM_CONTROL_ACK, got {ft:?}");
            }
            let parsed: SystemControlAck = serde_json::from_slice(&resp)?;
            if !parsed.ok {
                bail!(
                    "SYSTEM_CONTROL failed: {}",
                    parsed.err.as_deref().unwrap_or("payload returned ok=false")
                );
            }
            Ok(parsed)
        }
        Err(e) => {
            // Destructive actions intentionally sever the connection.
            // The payload sends the ACK first, but the kernel may RST
            // before the ACK frame leaves the wire. Treat "send
            // succeeded + read failed" as success for those actions.
            match action {
                PowerAction::Reboot | PowerAction::Shutdown | PowerAction::Standby => {
                    Ok(SystemControlAck {
                        ok: true,
                        action: Some(format!("{action:?}").to_lowercase()),
                        err: Some(format!("connection_dropped (expected for {action:?}): {e}")),
                        code: None,
                    })
                }
                PowerAction::Tick => Err(e),
            }
        }
    }
}

/// Power telemetry parsed from PowerTelemetry ACK body.
///
/// Each field is `Some` when ICC reported success, `None` when the
/// payload's ICC call returned an error and emitted `<key>=err`. The
/// renderer treats `None` as "this PS5 generation doesn't expose this
/// metric" rather than as a hard failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerTelemetry {
    /// Cumulative power-on seconds since first boot.
    pub operating_seconds: Option<u32>,
    /// Boot/shutdown cycle count.
    pub boot_cycles: Option<u32>,
    /// Bit-flagged thermal alert state. 0 = no alert.
    pub thermal_alert_flags: Option<u16>,
    /// Reason for the most recent power-up (button, RTC, network).
    /// Sony doesn't document the exact codes; surfaced raw.
    pub power_up_cause: Option<u8>,
}

/// Fetch the PS5's lifetime power telemetry. Cheap — three ICC calls
/// on the payload side, no kernel R/W needed.
pub fn power_telemetry(addr: &str) -> Result<PowerTelemetry> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PowerTelemetry, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected POWER_TELEMETRY: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PowerTelemetryAck {
        bail!("expected POWER_TELEMETRY_ACK, got {ft:?}");
    }
    Ok(parse_power_telemetry(&resp))
}

fn parse_power_telemetry(body: &[u8]) -> PowerTelemetry {
    // Body is `key=value\n…` text. Keys we care about:
    //   operating_seconds, boot_cycles, thermal_alert_flags,
    //   power_up_cause. Each value is either an unsigned decimal or
    //   the string "err" — parse_value_or_err handles both.
    let text = String::from_utf8_lossy(body);
    let mut map: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim(), v.trim());
        }
    }
    PowerTelemetry {
        operating_seconds: parse_u32_or_err(map.get("operating_seconds").copied()),
        boot_cycles: parse_u32_or_err(map.get("boot_cycles").copied()),
        thermal_alert_flags: parse_u16_or_err(map.get("thermal_alert_flags").copied()),
        power_up_cause: parse_u8_or_err(map.get("power_up_cause").copied()),
    }
}

fn parse_u32_or_err(s: Option<&str>) -> Option<u32> {
    match s {
        Some("err") | None => None,
        Some(v) => v.parse().ok(),
    }
}
fn parse_u16_or_err(s: Option<&str>) -> Option<u16> {
    match s {
        Some("err") | None => None,
        Some(v) => v.parse().ok(),
    }
}
fn parse_u8_or_err(s: Option<&str>) -> Option<u8> {
    match s {
        Some("err") | None => None,
        Some(v) => v.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_telemetry() {
        let body =
            b"operating_seconds=12345\nboot_cycles=42\nthermal_alert_flags=0\npower_up_cause=2\n";
        let t = parse_power_telemetry(body);
        assert_eq!(t.operating_seconds, Some(12345));
        assert_eq!(t.boot_cycles, Some(42));
        assert_eq!(t.thermal_alert_flags, Some(0));
        assert_eq!(t.power_up_cause, Some(2));
    }

    #[test]
    fn parse_err_values_become_none() {
        let body =
            b"operating_seconds=err\nboot_cycles=42\nthermal_alert_flags=err\npower_up_cause=err\n";
        let t = parse_power_telemetry(body);
        assert_eq!(t.operating_seconds, None);
        assert_eq!(t.boot_cycles, Some(42));
        assert_eq!(t.thermal_alert_flags, None);
        assert_eq!(t.power_up_cause, None);
    }

    #[test]
    fn parse_missing_keys_become_none() {
        let body = b"";
        let t = parse_power_telemetry(body);
        assert_eq!(t.operating_seconds, None);
        assert_eq!(t.boot_cycles, None);
    }
}
