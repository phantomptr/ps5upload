//! Engine→payload client for the PKG_INSTALL frame family.
//!
//! Sends a `.pkg` URL (HTTP, served by the engine's pkg-host listener)
//! to the payload, which calls `sceBgftServiceDownloadRegisterTask` +
//! `sceBgftServiceIntDownloadStartTask` to drive Sony's BGFT installer.
//! Sony's BGFT then HTTP-pulls the bytes from our listener, decrypts
//! with device keys, and installs the title.
//!
//! The payload returns a task_id immediately; the actual install runs
//! asynchronously inside PS5 firmware. Use `pkg_install_status` to
//! poll progress + the final outcome.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Request body sent in the PKG_INSTALL frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallRequest {
    /// Full HTTP URL the PS5's BGFT will fetch the `.pkg` from.
    /// Engine-hosted, ephemeral, session-token-gated.
    pub url: String,
    /// 36-byte content_id from the PKG header. BGFT records this in
    /// its task table so it can deduplicate against installed titles.
    pub content_id: String,
    /// Total size in bytes (sum of split parts when applicable).
    pub size: u64,
    /// Display title — used by BGFT for the "Downloads" notification
    /// and possibly the install progress UI on the PS5.
    pub title: String,
    /// BGFT package_type ("PS4GD", "PS4AC", etc). Affects install
    /// destination and DRM behavior; mismatch usually surfaces as a
    /// non-zero err_code from BGFT itself.
    pub package_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallResponse {
    pub task_id: i32,
    /// 0 on accept; non-zero is a BGFT error code (e.g. 0x80990088 =
    /// already installed). Caller maps to a user-facing message.
    pub err_code: u32,
    /// Human-readable detail when the err_code alone isn't enough —
    /// e.g. "libSceBgft.sprx not loadable on this firmware".
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    /// BGFT registered the task but hasn't started downloading yet.
    Queued,
    /// BGFT is pulling bytes from our HTTP listener.
    Download,
    /// All bytes received; Sony's installer is decrypting + writing.
    Install,
    /// Installer reported success; the title should be in Library.
    Done,
    /// BGFT or installer reported failure; `err_code` carries Sony's code.
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgInstallStatus {
    pub phase: InstallPhase,
    pub downloaded: u64,
    pub total: u64,
    pub err_code: u32,
    #[serde(default)]
    pub detail: String,
}

/// Send the PKG_INSTALL frame and parse the ack.
pub fn pkg_install(addr: &str, req: &PkgInstallRequest) -> Result<PkgInstallResponse> {
    let body = serde_json::to_vec(req)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PkgInstall, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PKG_INSTALL: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PkgInstallAck {
        bail!("expected PKG_INSTALL_ACK, got {ft:?}");
    }
    let parsed: PkgInstallResponse = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

/// Poll an in-flight install for status. Cheap on the payload side —
/// just a `sceBgftServiceDownloadGetProgress` call. Caller polls every
/// 1 s while the install is active.
pub fn pkg_install_status(addr: &str, task_id: i32) -> Result<PkgInstallStatus> {
    let body = serde_json::json!({ "task_id": task_id });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::PkgInstallStatus, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected PKG_INSTALL_STATUS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::PkgInstallStatusAck {
        bail!("expected PKG_INSTALL_STATUS_ACK, got {ft:?}");
    }
    let parsed: PkgInstallStatus = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

/// Map a Sony BGFT err_code to a user-facing message. Codes from
/// community PS4/PS5 references.
/// Returns None for unknown codes — caller falls back to the raw hex.
pub fn err_code_message(code: u32) -> Option<&'static str> {
    match code {
        0x0000_0000 => None, // success — no message
        0x8099_0001 => Some("BGFT initialised already (benign)"),
        0x8099_0036 => Some("DRM mismatch — this PKG isn't valid for this console"),
        0x8099_0039 => Some("Out of free space on the PS5"),
        0x8099_0085 => Some("Need defragmented free space — Settings → Storage → Free up space"),
        0x8099_0086 => Some("Leftover download in notifications — clear it from the PS5 first"),
        0x8099_0088 => Some("This title is already installed"),
        0x80A3_0026 => Some("Out of free space on the PS5"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_err_codes_have_messages() {
        assert!(err_code_message(0x80990088).is_some());
        assert!(err_code_message(0x80990085).is_some());
        assert!(err_code_message(0).is_none());
        assert!(err_code_message(0xDEADBEEF).is_none());
    }

    #[test]
    fn install_request_round_trips() {
        let req = PkgInstallRequest {
            url: "http://10.0.0.1:48710/pkg-host/abc/file.pkg".into(),
            content_id: "EP0006-CUSA12345_00-FOOBARBAZ0123456".into(),
            size: 1_234_567_890,
            title: "Test Title".into(),
            package_type: "PS4GD".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: PkgInstallRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.content_id, req.content_id);
        assert_eq!(back.size, req.size);
    }

    #[test]
    fn phase_serializes_snake_case() {
        let s = serde_json::to_string(&InstallPhase::Download).unwrap();
        assert_eq!(s, "\"download\"");
        let p: InstallPhase = serde_json::from_str("\"done\"").unwrap();
        assert_eq!(p, InstallPhase::Done);
    }
}
