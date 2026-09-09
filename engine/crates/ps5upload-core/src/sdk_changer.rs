//! SDK Changer proxy: scan installed titles for SDK version and patch
//! binaries + param.json to a target SDK version.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkTitle {
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sdk_version: String,
    #[serde(default)]
    pub fw_required: String,
    #[serde(default)]
    pub patchable: bool,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkScanResponse {
    #[serde(default)]
    pub titles: Vec<SdkTitle>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkPatchRequest {
    pub title_id: String,
    pub target_sdk: String,
    /// BestPig's libc.prx symbol swap. Opt-in: it is documented as helping
    /// SOME titles, and on hardware applying it to a title that did not need
    /// it crashed the game after five modules where it otherwise loaded
    /// seventy. Never send `true` unless the user asked for it.
    #[serde(default)]
    pub patch_libc: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkPatchResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub target_sdk: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
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

pub fn sdk_scan(addr: &str) -> Result<SdkScanResponse> {
    let resp = send_recv(addr, FrameType::SdkScan, FrameType::SdkScanAck, None)?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn sdk_patch(
    addr: &str,
    title_id: &str,
    target_sdk: &str,
    patch_libc: bool,
) -> Result<SdkPatchResponse> {
    let req = SdkPatchRequest {
        title_id: title_id.to_string(),
        target_sdk: target_sdk.to_string(),
        patch_libc,
    };
    let resp = send_recv(
        addr,
        FrameType::SdkPatch,
        FrameType::SdkPatchAck,
        Some(&serde_json::to_vec(&req)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkRestoreRequest {
    pub title_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkRestoreResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub restored: i32,
    #[serde(default)]
    pub error: Option<String>,
}

pub fn sdk_restore(addr: &str, title_id: &str) -> Result<SdkRestoreResponse> {
    let req = SdkRestoreRequest {
        title_id: title_id.to_string(),
    };
    let resp = send_recv(
        addr,
        FrameType::SdkRestore,
        FrameType::SdkRestoreAck,
        Some(&serde_json::to_vec(&req)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_sdk_scan() {
        let json = r#"{
            "titles": [
                {"title_id":"CUSA00001","name":"Game A",
                 "sdk_version":"0x0906008100000000",
                 "fw_required":"0x0906008100000000"}
            ]
        }"#;
        let resp: SdkScanResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.titles.len(), 1);
        assert_eq!(resp.titles[0].title_id, "CUSA00001");
        assert!(!resp.titles[0].sdk_version.is_empty());
    }

    #[test]
    fn deserialize_sdk_scan_empty() {
        let json = r#"{}"#;
        let resp: SdkScanResponse = serde_json::from_str(json).unwrap();
        assert!(resp.titles.is_empty());
    }

    #[test]
    fn deserialize_sdk_patch_ok() {
        let json = r#"{"ok":true,"title_id":"CUSA00001","target_sdk":"0x09060000","detail":"ELF sites: 1"}"#;
        let resp: SdkPatchResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.title_id, "CUSA00001");
        assert_eq!(resp.detail.as_deref(), Some("ELF sites: 1"));
        assert!(resp.error.is_none());
    }

    #[test]
    fn deserialize_sdk_patch_err() {
        let json = r#"{"ok":false,"error":"title not found"}"#;
        let resp: SdkPatchResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("title not found"));
    }

    #[test]
    fn deserialize_sdk_restore_ok() {
        let json = r#"{"ok":true,"title_id":"CUSA00001","restored":5}"#;
        let resp: SdkRestoreResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.title_id, "CUSA00001");
        assert_eq!(resp.restored, 5);
        assert!(resp.error.is_none());
    }

    #[test]
    fn deserialize_sdk_restore_no_backup() {
        let json =
            r#"{"ok":true,"title_id":"CUSA00001","restored":0,"error":"no .bak files found"}"#;
        let resp: SdkRestoreResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.restored, 0);
        assert!(resp.error.as_deref().is_some());
    }

    #[test]
    fn deserialize_sdk_restore_err() {
        let json = r#"{"ok":false,"error":"title CUSA99999 not found"}"#;
        let resp: SdkRestoreResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("title CUSA99999 not found"));
    }
}
