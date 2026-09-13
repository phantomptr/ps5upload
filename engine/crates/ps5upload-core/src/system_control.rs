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
            match serde_json::from_slice::<SystemControlAck>(&resp) {
                Ok(parsed) => {
                    if !parsed.ok {
                        bail!(
                            "SYSTEM_CONTROL failed: {}",
                            parsed.err.as_deref().unwrap_or("payload returned ok=false")
                        );
                    }
                    Ok(parsed)
                }
                Err(parse_err) => {
                    // Same destructive-action escape hatch we use for
                    // connection drops below, applied to JSON parse
                    // failures: 2.17.1's bundled payload had hand-
                    // counted ACK body lengths that were off-by-one on
                    // reboot / shutdown / tick (e.g. the reboot literal
                    // was 29 chars sent as 28, dropping the closing
                    // `}`). The frame layer accepts that just fine,
                    // serde_json rejects it with "EOF while parsing an
                    // object at line 1 column N". Newer payloads fix
                    // the off-by-one upstream; this branch retroactively
                    // covers clients still talking to an older payload
                    // already loaded on a PS5. Tick is non-destructive,
                    // so a corrupt tick ACK still bubbles up the error
                    // (no consequence to retry).
                    match action {
                        PowerAction::Reboot
                        | PowerAction::Shutdown
                        | PowerAction::Standby => Ok(SystemControlAck {
                            ok: true,
                            action: Some(format!("{action:?}").to_lowercase()),
                            err: Some(format!(
                                "malformed_ack (expected for {action:?} on old payloads): {parse_err}"
                            )),
                            code: None,
                        }),
                        PowerAction::Tick => Err(parse_err.into()),
                    }
                }
            }
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
    /// Why the fields above may be empty:
    ///   `ok`                     all four values read
    ///   `partial`                some read, some failed (FW 5.10)
    ///   `calls_failed`           symbols exist but every call errored
    ///   `unsupported_firmware`   no `sceKernelIccGet*` symbol resolves
    ///                            (retail 9.60, verified on hardware)
    ///
    /// Without this the client can only render four bare nulls as "—",
    /// which reads like a bug rather than a firmware limitation. Older
    /// payloads don't send it; `None` then means "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// How many of the four ICC symbols exist on this firmware (0-4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbols_resolved: Option<u8>,
    /// How many actually returned a value (0-4). Deliberately separate
    /// from `symbols_resolved`: FW 5.10 resolves all four but only two
    /// succeed, so the symbol count alone would overstate support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_ok: Option<u8>,
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
    let operating_seconds = parse_u32_or_err(map.get("operating_seconds").copied());
    let boot_cycles = parse_u32_or_err(map.get("boot_cycles").copied());
    let thermal_alert_flags = parse_u16_or_err(map.get("thermal_alert_flags").copied());
    let power_up_cause = parse_u8_or_err(map.get("power_up_cause").copied());

    // `symbols_resolved` is the payload's to report — only it can see
    // which dlsym calls succeeded. Sent by payloads >= 5.1.1; None from
    // older ones, and "we don't know" must not be conflated with "we
    // know it's unsupported".
    let symbols_resolved: Option<u8> = map.get("symbols_resolved").and_then(|s| s.parse().ok());

    // `values_ok` is derived HERE, not taken from the payload, so it can
    // never disagree with the fields the client actually receives. The
    // payload counts rc == 0; a value can still be dropped afterwards if
    // it doesn't parse, which on FW 5.10 made the payload claim 3 while
    // only 2 fields arrived.
    let values_ok = operating_seconds.is_some() as u8
        + boot_cycles.is_some() as u8
        + thermal_alert_flags.is_some() as u8
        + power_up_cause.is_some() as u8;

    // Status is likewise derived, for the same reason.
    let status = symbols_resolved.map(|resolved| {
        if resolved == 0 {
            "unsupported_firmware"
        } else if values_ok == 0 {
            "calls_failed"
        } else if values_ok < 4 {
            "partial"
        } else {
            "ok"
        }
        .to_string()
    });

    PowerTelemetry {
        operating_seconds,
        boot_cycles,
        thermal_alert_flags,
        power_up_cause,
        status,
        symbols_resolved,
        values_ok: symbols_resolved.map(|_| values_ok),
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

    // The next two tests document the payload-side off-by-one bug
    // that motivated the 2.17.2 fix: 2.17.1's payload hand-counted
    // its SYSTEM_CONTROL_ACK body lengths and dropped the closing
    // `}` on reboot / shutdown / tick. Frame layer happily delivers
    // the truncated bytes; serde_json then rejects. We pin both
    // sides (what's broken on old payloads, what new payloads must
    // produce) so a regression on either side surfaces as a test
    // failure rather than as a spurious error popup at users.
    #[test]
    fn truncated_reboot_ack_fails_to_parse() {
        // What 2.17.1's payload actually sent: length 28, missing the
        // closing brace. Matches the user-reported error literally
        // ("EOF while parsing an object at line 1 column 28").
        let truncated = br#"{"ok":true,"action":"reboot""#;
        assert_eq!(truncated.len(), 28);
        let parsed: Result<SystemControlAck, _> = serde_json::from_slice(truncated);
        assert!(
            parsed.is_err(),
            "truncated body must error so the destructive-action tolerance branch kicks in",
        );
        let err = parsed.unwrap_err().to_string();
        assert!(
            err.contains("EOF while parsing"),
            "want EOF-style serde error, got: {err}",
        );
    }

    #[test]
    fn well_formed_reboot_ack_round_trips() {
        // What the post-fix payload (strlen-based send) emits. Pins
        // the contract: the new send length must produce a JSON the
        // client can parse.
        let well_formed = br#"{"ok":true,"action":"reboot"}"#;
        assert_eq!(well_formed.len(), 29);
        let parsed: SystemControlAck =
            serde_json::from_slice(well_formed).expect("well-formed reboot ACK must parse");
        assert!(parsed.ok);
        assert_eq!(parsed.action.as_deref(), Some("reboot"));
        assert_eq!(parsed.err, None);
    }
}

#[cfg(test)]
mod telemetry_parse_tests {
    use super::*;

    #[test]
    fn a_payload_reply_carries_its_status_through() {
        // Exactly what handle_power_telemetry emits on FW 5.10: one value
        // readable, three not, plus the diagnostic lines. If status is lost
        // here the client can only render four dashes, which reads as a bug
        // rather than a firmware limit.
        let body = b"operating_seconds=err\nboot_cycles=16842752\nthermal_alert_flags=err\npower_up_cause=err\nsymbols_resolved=4\nvalues_ok=1\nstatus=partial\n";
        let t = parse_power_telemetry(body);
        assert_eq!(t.symbols_resolved, Some(4));
        assert_eq!(t.status.as_deref(), Some("partial"));
        assert_eq!(t.boot_cycles, Some(16842752));
    }

    #[test]
    fn no_symbols_resolved_is_reported_as_unsupported_firmware() {
        let body = b"operating_seconds=err\nboot_cycles=err\nthermal_alert_flags=err\npower_up_cause=err\nsymbols_resolved=0\nvalues_ok=0\nstatus=unsupported_firmware\n";
        let t = parse_power_telemetry(body);
        assert_eq!(t.status.as_deref(), Some("unsupported_firmware"));
    }
}
