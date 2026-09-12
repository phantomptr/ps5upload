//! Remote Play PIN generation over FTX2.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

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
}

pub fn remoteplay_request(addr: &str, manual_account_id: Option<&str>) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::json!({ "manual_account_id": manual_account_id.unwrap_or("") });
    c.send_frame(FrameType::RemotePlayRequest, &serde_json::to_vec(&body)?)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected REMOTEPLAY_REQUEST: {}",
            String::from_utf8_lossy(&resp)
        );
    }
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
    }
    let ack: RequestAck = serde_json::from_slice(&resp)
        .map_err(|e| anyhow::anyhow!("bad REMOTEPLAY_REQUEST ack: {e}"))?;
    if !ack.ok {
        bail!("payload reports the Remote Play request failed on-console");
    }
    Ok(())
}

pub fn remoteplay_status(addr: &str) -> Result<RemotePlayStatus> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::RemotePlayStatus, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected REMOTEPLAY_STATUS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::RemotePlayStatus {
        bail!("expected REMOTEPLAY_STATUS response, got {ft:?}");
    }
    let parsed: RemotePlayStatus = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

pub fn remoteplay_cancel(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::RemotePlayCancel, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected REMOTEPLAY_CANCEL: {}",
            String::from_utf8_lossy(&resp)
        );
    }
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
    /// Everything Remote Play needs is in place.
    ///
    /// Gated on `account_uid`, not `foreground_uid`: a console with a
    /// signed-in user but nobody in the foreground can pair perfectly
    /// well, and gating on the foreground reported those as unusable.
    pub fn ready_to_pair(&self) -> bool {
        self.registry_ok()
            && self.symbols_ok != 0
            && self.account_uid > 0
            && self.activated()
            && self.service_on()
            && (!self.needs_per_user() || self.user_on())
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
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::RemotePlayReadiness, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected RemotePlayReadiness: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::RemotePlayReadiness {
        bail!("unexpected reply to RemotePlayReadiness: {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

/// Enable Remote Play. `scope` is "service" or "user".
///
/// Returns the re-read readiness snapshot, so the caller never has to
/// assume the write took effect.
pub fn remoteplay_enable(addr: &str, scope: &str) -> Result<RemotePlayReadiness> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::json!({ "scope": scope });
    c.send_frame(FrameType::RemotePlayEnable, &serde_json::to_vec(&body)?)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected RemotePlayEnable: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::RemotePlayEnable {
        bail!("unexpected reply to RemotePlayEnable: {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

/// Devices this console has been paired with.
pub fn remoteplay_devices(addr: &str) -> Result<RemotePlayDevices> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::RemotePlayDevices, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected RemotePlayDevices: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::RemotePlayDevices {
        bail!("unexpected reply to RemotePlayDevices: {ft:?}");
    }
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
