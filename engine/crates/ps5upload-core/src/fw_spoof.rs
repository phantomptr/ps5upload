//! FW Spoof detection proxy: read the PS5's reported system software
//! version and kernel release, and flag potential spoofing.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FwSpoofStatusResponse {
    #[serde(default)]
    pub system_sw_version: String,
    #[serde(default)]
    pub system_sw_raw: String,
    #[serde(default)]
    pub kernel_release: String,
    #[serde(default)]
    pub kernel_fw_version: String,
    #[serde(default)]
    pub kernel_version: String,
    #[serde(default)]
    pub spoofed: bool,
}

fn send_recv(
    addr: &str,
    req_type: FrameType,
    ack_type: FrameType,
    body: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    let empty: Vec<u8> = Vec::new();
    let body = body.unwrap_or(&empty);
    c.send_frame(req_type, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {:?}: {}",
            req_type,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack_type {
        bail!("expected {:?}, got {:?}", ack_type, ft);
    }
    Ok(resp)
}

pub fn fw_spoof_status(addr: &str) -> Result<FwSpoofStatusResponse> {
    let resp = send_recv(
        addr,
        FrameType::FwSpoofStatus,
        FrameType::FwSpoofStatusAck,
        None,
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_normal() {
        let json = r#"{
            "system_sw_version":"09.60.00",
            "system_sw_raw":"0x09600000",
            "kernel_release":"9.60.0",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.system_sw_version, "09.60.00");
        assert!(!resp.spoofed);
    }

    #[test]
    fn deserialize_spoofed() {
        let json = r#"{
            "system_sw_version":"05.10.00",
            "system_sw_raw":"0x05100000",
            "kernel_release":"9.60.0",
            "spoofed":true
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(resp.spoofed);
        assert_eq!(resp.kernel_release, "9.60.0");
    }

    #[test]
    fn deserialize_empty() {
        let json = r#"{}"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.spoofed);
    }

    #[test]
    fn deserialize_zero_version_not_spoofed() {
        // After the fix: fw=0 means "unknown", NOT "spoofed".
        // The old payload returned spoofed:true for fw=0 which was a false
        // positive — sceKernelGetSystemSwVersion() returns 0 in kthread
        // context but that doesn't mean the firmware is spoofed.
        let json = r#"{
            "system_sw_version":"unknown",
            "system_sw_raw":"0x00000000",
            "kernel_release":"unknown",
            "kernel_fw_version":"unknown",
            "kernel_version":"unknown",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.spoofed);
        assert_eq!(resp.system_sw_version, "unknown");
    }

    #[test]
    fn deserialize_genuine_960() {
        // Real PS5 Pro FW 9.60 — kernel_get_fw_version returns 0x09600000.
        // system_sw_version and kernel_fw_version are now the SAME value
        // (both from kernel memory). spoofed must be false.
        let json = r#"{
            "system_sw_version":"09.60.00",
            "system_sw_raw":"0x09600000",
            "kernel_release":"0.0-prototype",
            "kernel_fw_version":"09.60.00",
            "kernel_version":"FreeBSD 12.0.0 PlayStation(R)5",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.spoofed);
        assert_eq!(resp.system_sw_version, "09.60.00");
        assert_eq!(resp.kernel_fw_version, "09.60.00");
        assert_eq!(resp.kernel_version, "FreeBSD 12.0.0 PlayStation(R)5");
    }

    #[test]
    fn deserialize_genuine_510() {
        // Real PS5 Fat FW 5.10 — should also not be spoofed.
        let json = r#"{
            "system_sw_version":"05.10.00",
            "system_sw_raw":"0x05100000",
            "kernel_release":"0.0-prototype",
            "kernel_fw_version":"05.10.00",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.spoofed);
    }

    #[test]
    fn deserialize_missing_fields() {
        let json = r#"{"spoofed":false}"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.spoofed);
        assert!(resp.system_sw_version.is_empty());
        assert!(resp.kernel_release.is_empty());
    }

    #[test]
    fn deserialize_with_raw() {
        let json = r#"{
            "system_sw_version":"11.00.00",
            "system_sw_raw":"0x0b000000",
            "kernel_release":"11.0.0",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.system_sw_raw, "0x0b000000");
    }

    #[test]
    fn deserialize_with_kernel_fw_version() {
        let json = r#"{
            "system_sw_version":"09.60.00",
            "system_sw_raw":"0x09600000",
            "kernel_release":"9.60.0",
            "kernel_fw_version":"09.60.00",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.kernel_fw_version, "09.60.00");
        assert!(!resp.spoofed);
    }

    #[test]
    fn deserialize_spoofed_with_kernel_fw() {
        let json = r#"{
            "system_sw_version":"09.60.00",
            "system_sw_raw":"0x09600000",
            "kernel_release":"5.10.0",
            "kernel_fw_version":"05.10.00",
            "spoofed":true
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.kernel_fw_version, "05.10.00");
        assert!(resp.spoofed);
    }

    #[test]
    fn deserialize_missing_kernel_fw_version() {
        let json = r#"{
            "system_sw_version":"09.60.00",
            "system_sw_raw":"0x09600000",
            "kernel_release":"9.60.0",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(resp.kernel_fw_version.is_empty());
    }

    #[test]
    fn deserialize_kernel_version_field() {
        let json = r#"{
            "system_sw_version":"09.60.00",
            "system_sw_raw":"0x09600000",
            "kernel_release":"0.0-prototype",
            "kernel_fw_version":"09.60.00",
            "kernel_version":"FreeBSD 12.0.0 PlayStation(R)5 ...",
            "spoofed":false
        }"#;
        let resp: FwSpoofStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.kernel_version.is_empty());
        assert!(resp.kernel_version.contains("FreeBSD"));
    }
}
