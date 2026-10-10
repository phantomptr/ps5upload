//! Remote Play PIN generation over AVA1 management.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemotePlayStatus {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub pin: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub seconds_left: i32,
    /// Diagnostic the payload writes when entering a FAILED/TIMEOUT state
    /// (e.g. "sceRemoteplayInitialize failed: 0x8094xxxx"). Surfaced to the
    /// UI so the user sees *why* the PIN couldn't be generated, not just
    /// the bare word "failed".
    #[serde(default)]
    pub err: String,
    /// ConfirmDeviceRegist probes made for the live PIN, and the last answer (rc, status,
    /// reason code). Diagnostics only: `state` is already the payload's verdict. Absent
    /// (zero) from payloads before the pairing-state rewrite.
    #[serde(default)]
    pub probes: u32,
    #[serde(default)]
    pub confirm_rc: u32,
    #[serde(default)]
    pub confirm_status: u32,
    #[serde(default)]
    pub confirm_err: u32,
}

/// Where a pairing stands, from the payload's `state` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingPhase {
    /// No PIN outstanding.
    Idle,
    /// A PIN is live; `seconds_left` counts down to its expiry. Older payloads also said
    /// `starting` while making the PIN.
    Waiting,
    /// A device registration was confirmed by the console.
    Paired,
    /// The request or the registration failed; `err` says why.
    Failed,
    /// The PIN expired with no registration.
    Timeout,
    /// A word this engine does not know (a newer payload).
    Unknown,
}

impl RemotePlayStatus {
    pub fn phase(&self) -> PairingPhase {
        match self.state.as_str() {
            "idle" => PairingPhase::Idle,
            "waiting" | "starting" => PairingPhase::Waiting,
            "paired" => PairingPhase::Paired,
            "failed" => PairingPhase::Failed,
            "timeout" => PairingPhase::Timeout,
            _ => PairingPhase::Unknown,
        }
    }
}

pub fn remoteplay_request(addr: &str, manual_account_id: Option<&str>) -> Result<PinSnapshot> {
    let body = serde_json::json!({ "manual_account_id": manual_account_id.unwrap_or("") });
    let resp = mgmt::call_keep(
        addr,
        m::RP_REQUEST,
        "REMOTEPLAY_REQUEST",
        &serde_json::to_vec(&body)?,
    )?;
    // The payload acks with frame type RemotePlayStatus (189) and body
    // {"ok":true|false}. A non-Error frame was previously treated as success
    // without inspecting the body — so a genuine on-console failure
    // (libSceRemoteplay absent, Initialize returned non-zero, no foreground
    // user, …) was silently swallowed and the user only saw "failed" on the
    // next status poll. Parse the ack and bail when the payload says !ok.
    #[derive(Deserialize)]
    struct RequestAck {
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        snapshot: Option<PinSnapshot>,
    }
    let ack: RequestAck = serde_json::from_slice(&resp)
        .map_err(|e| anyhow::anyhow!("bad REMOTEPLAY_REQUEST ack: {e}"))?;
    if !ack.ok {
        bail!("payload reports the Remote Play request failed on-console");
    }
    Ok(ack.snapshot.unwrap_or_default())
}

/// The PIN and account id, read without probing for pairing completion.
///
/// `remoteplay_status` drives `sceRemoteplayConfirmDeviceRegist` as a side
/// effect, which finalises a pending registration on the console. Anything
/// that intends to perform the registration itself must take the PIN from
/// here instead, or it consumes the pairing it is about to attempt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinSnapshot {
    #[serde(default)]
    pub pin: String,
    #[serde(default)]
    pub account_id: String,
}

pub fn remoteplay_status(addr: &str) -> Result<RemotePlayStatus> {
    let resp = mgmt::call_keep(addr, m::RP_STATUS, "REMOTEPLAY_STATUS", &[])?;
    let parsed: RemotePlayStatus = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

pub fn remoteplay_cancel(addr: &str) -> Result<()> {
    mgmt::call_keep(addr, m::RP_CANCEL, "REMOTEPLAY_CANCEL", &[])?;
    Ok(())
}

/// Everything that decides whether Remote Play can work on this console.
///
/// The payload sends 0/1 integers for the flags rather than JSON booleans,
/// so these are `u8` and converted by the helpers below. Do not "simplify"
/// them to `bool` and expect serde to coerce — it will not, and the frame
/// will fail to parse. See the payload↔engine key contract note.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RemotePlayReadiness {
    #[serde(default)]
    pub fw_magic: u32,
    #[serde(default)]
    pub has_per_user: u8,
    /// The user in the foreground, or -1 when there is none. Reported
    /// literally — a console can sit signed in with nobody in front.
    #[serde(default)]
    pub foreground_uid: i64,
    /// The user whose account pairing will actually use. Usually the
    /// foreground one; falls back to the first signed-in activated user.
    #[serde(default)]
    pub account_uid: i64,
    /// How `account_uid` was chosen: `foreground`, `login-list`, `none`.
    #[serde(default)]
    pub account_via: String,
    #[serde(default)]
    pub user_slot: i32,
    #[serde(default)]
    pub account_id_b64: String,
    #[serde(default)]
    pub account_id_raw: u64,
    #[serde(default)]
    pub account_type: String,
    #[serde(default)]
    pub service_enabled: u8,
    #[serde(default)]
    pub user_enabled: u8,
    #[serde(default)]
    pub symbols_ok: u8,
    /// Non-zero means the registry could not be READ — which is very
    /// different from "the setting is off". Everything else in this struct
    /// is meaningless when it is set.
    #[serde(default)]
    pub registry_err: u32,
}

impl RemotePlayReadiness {
    /// Firmware as (major, minor), e.g. 0x09600004 -> (9, 60).
    ///
    /// Both bytes are **BCD**, not binary: 9.60 is 0x0960 and 10.00 is
    /// 0x1000. Reading them as plain integers gives 9.96 and 16.0 — which
    /// is exactly the bug this replaced. The magic also carries low-order
    /// bits past the version (5.10 reads as 0x05100023), so the raw number
    /// is never something to show a user.
    pub fn firmware(&self) -> Option<(u8, u8)> {
        if self.fw_magic == 0 {
            return None;
        }
        let bcd = |b: u8| (b >> 4) * 10 + (b & 0x0F);
        let major = bcd(((self.fw_magic >> 24) & 0xFF) as u8);
        let minor = bcd(((self.fw_magic >> 16) & 0xFF) as u8);
        Some((major, minor))
    }

    pub fn registry_ok(&self) -> bool {
        self.registry_err == 0
    }
    pub fn service_on(&self) -> bool {
        self.service_enabled != 0
    }
    pub fn user_on(&self) -> bool {
        self.user_enabled != 0
    }
    pub fn needs_per_user(&self) -> bool {
        self.has_per_user != 0
    }
    /// An account exists and has been activated.
    pub fn activated(&self) -> bool {
        self.account_id_raw != 0 && self.account_type == "np"
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RemotePlayDevice {
    #[serde(default)]
    pub slot: u32,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub client_type: i32,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RemotePlayDevices {
    #[serde(default)]
    pub devices: Vec<RemotePlayDevice>,
}

/// Read the readiness snapshot. Performs no writes on the console.
pub fn remoteplay_readiness(addr: &str) -> Result<RemotePlayReadiness> {
    let resp = mgmt::call_keep(addr, m::RP_READINESS, "RemotePlayReadiness", &[])?;
    Ok(serde_json::from_slice(&resp)?)
}

/// Enable Remote Play. `scope` is "service" or "user".
///
/// Returns the re-read readiness snapshot, so the caller never has to
/// assume the write took effect.
pub fn remoteplay_enable(addr: &str, scope: &str) -> Result<RemotePlayReadiness> {
    let body = serde_json::json!({ "scope": scope });
    let resp = mgmt::call_keep(
        addr,
        m::RP_ENABLE,
        "RemotePlayEnable",
        &serde_json::to_vec(&body)?,
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

/// Devices this console has been paired with.
pub fn remoteplay_devices(addr: &str) -> Result<RemotePlayDevices> {
    let resp = mgmt::call_keep(addr, m::RP_DEVICES, "RemotePlayDevices", &[])?;
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod firmware_tests {
    use super::RemotePlayReadiness;

    fn with_magic(fw_magic: u32) -> RemotePlayReadiness {
        RemotePlayReadiness {
            fw_magic,
            has_per_user: 0,
            foreground_uid: 0,
            account_uid: 0,
            account_via: String::new(),
            user_slot: 0,
            account_id_b64: String::new(),
            account_id_raw: 0,
            account_type: String::new(),
            service_enabled: 0,
            user_enabled: 0,
            symbols_ok: 0,
            registry_err: 0,
        }
    }

    #[test]
    fn decodes_bcd_not_binary() {
        // Real magics read off the two test consoles. Decoding these as
        // plain integers yields 5.16 and 9.96 — the bug this pins.
        assert_eq!(with_magic(0x05100023).firmware(), Some((5, 10)));
        assert_eq!(with_magic(0x09600004).firmware(), Some((9, 60)));
    }

    #[test]
    fn a_major_of_ten_is_not_sixteen() {
        // 10.00 is where per-user Remote Play arrives, so getting this
        // wrong would mislabel exactly the firmware that matters most.
        assert_eq!(with_magic(0x10000000).firmware(), Some((10, 0)));
        assert_eq!(with_magic(0x12700000).firmware(), Some((12, 70)));
        assert_eq!(with_magic(0x13200000).firmware(), Some((13, 20)));
    }

    #[test]
    fn unknown_firmware_has_no_version() {
        assert_eq!(with_magic(0).firmware(), None);
    }
}

#[cfg(test)]
mod status_tests {
    use super::{PairingPhase, RemotePlayStatus};

    fn parse(json: &str) -> RemotePlayStatus {
        serde_json::from_str(json).expect("a payload status body")
    }

    #[test]
    fn a_live_pin_is_waiting_with_its_countdown() {
        // Shape of the payload's answer three seconds after a request (Phat, FW 13.60).
        let s = parse(
            r#"{"state":"waiting","pin":"36876659","account_id":"XCDiqZluNXo=",
                "seconds_left":297,"err":"","probes":1,"confirm_rc":0,
                "confirm_status":0,"confirm_err":0}"#,
        );
        assert_eq!(s.phase(), PairingPhase::Waiting);
        assert_eq!(s.seconds_left, 297);
        assert_eq!(s.probes, 1);
    }

    #[test]
    fn every_payload_state_maps() {
        for (word, phase) in [
            ("idle", PairingPhase::Idle),
            ("waiting", PairingPhase::Waiting),
            ("starting", PairingPhase::Waiting),
            ("paired", PairingPhase::Paired),
            ("failed", PairingPhase::Failed),
            ("timeout", PairingPhase::Timeout),
            ("", PairingPhase::Unknown),
            ("registering", PairingPhase::Unknown),
        ] {
            let s = parse(&format!(r#"{{"state":"{word}"}}"#));
            assert_eq!(s.phase(), phase, "{word:?}");
        }
    }

    #[test]
    fn paired_timeout_and_failed_carry_no_live_pin() {
        let paired = parse(r#"{"state":"paired","pin":"","seconds_left":0,"confirm_status":2}"#);
        assert_eq!(paired.phase(), PairingPhase::Paired);
        assert_eq!(paired.confirm_status, 2);

        let timeout = parse(
            r#"{"state":"timeout","pin":"","seconds_left":0,
                "err":"the PIN expired before a device paired"}"#,
        );
        assert_eq!(timeout.phase(), PairingPhase::Timeout);
        assert!(timeout.err.contains("expired"));

        let failed = parse(
            r#"{"state":"failed","err":"pairing failed: the PIN was entered wrong (status 3, 0x80FC1047)",
                "confirm_status":3,"confirm_err":2164002887}"#,
        );
        assert_eq!(failed.phase(), PairingPhase::Failed);
        assert_eq!(failed.confirm_err, 0x80FC1047);
    }

    #[test]
    fn an_older_payload_without_the_diagnostics_still_parses() {
        let s = parse(
            r#"{"state":"idle","pin":"","account_id":"XCDiqZluNXo=","seconds_left":0,"err":""}"#,
        );
        assert_eq!(s.phase(), PairingPhase::Idle);
        assert_eq!((s.probes, s.confirm_rc, s.confirm_status), (0, 0, 0));
    }
}
