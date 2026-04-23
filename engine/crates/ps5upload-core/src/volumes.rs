//! FS_LIST_VOLUMES RPC — enumerate storage volumes visible on the PS5.
//!
//! The payload probes a fixed set of well-known mount points
//! (`/data`, `/user`, `/ext0..7`, `/usb0..7`) and returns an entry for each
//! that is currently mounted. This is intentionally a read-only probe: no
//! file contents are touched, only `lstat` + `statfs`.
//!
//! Typical use:
//!   - UI "pick a destination drive" dropdown
//!   - smoke/bench tests sanity-checking that `/data` is reachable
//!   - delta transfers choosing a target drive with sufficient free space

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// One entry in the payload's volume list.
///
/// Fields mirror `struct statfs` on PS5 FreeBSD: `fs_type` is the short
/// filesystem name (`ufs`, `bfs`, `nullfs`, `tmpfs`, …), `writable`
/// reflects the mount's `MNT_RDONLY` flag, `*_bytes` are derived from
/// block counts, and `mount_from` is the device / pseudo source
/// (`/dev/nvme1`, `/dev/ssd0.user`, `tmpfs`, ...).
///
/// `is_placeholder` is true for PS5 mount slots that have no real drive
/// attached (tmpfs or <256 MiB). UIs typically filter these out by
/// default but can show them to reflect hot-plug state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub path: String,
    #[serde(default)]
    pub mount_from: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub writable: bool,
    #[serde(default)]
    pub is_placeholder: bool,
    /// For mounts under `/mnt/ps5upload/` this is the backing image file
    /// (`/data/homebrew/image.exfat`, etc.), recorded by the payload
    /// when the mount was created. Empty string for non-ours mounts or
    /// for mounts created before this tracking was added. UIs use this
    /// to surface "what file is mounted here" without needing to ask
    /// a different API.
    #[serde(default)]
    pub source_image: String,
}

impl Volume {
    /// Quick filter for "would a user call this a usable drive": present,
    /// not a placeholder, and has at least some free space. UIs building
    /// drive-picker dropdowns want this.
    pub fn is_usable(&self) -> bool {
        !self.is_placeholder && self.writable && self.free_bytes > 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeList {
    pub volumes: Vec<Volume>,
}

impl VolumeList {
    /// Find a volume by its mount-point path, for UIs that want to render
    /// a specific drive's free-space indicator without re-querying.
    pub fn find(&self, path: &str) -> Option<&Volume> {
        self.volumes.iter().find(|v| v.path == path)
    }
}

/// Connect to the payload, send FS_LIST_VOLUMES, await FS_LIST_VOLUMES_ACK,
/// return parsed list.
///
/// Returns an error if the payload replies with an unexpected frame type
/// (including `FrameType::Error`) or if the JSON body fails to parse.
pub fn list_volumes(addr: &str) -> Result<VolumeList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::FsListVolumes, b"")?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_LIST_VOLUMES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsListVolumesAck {
        bail!("expected FS_LIST_VOLUMES_ACK, got {:?}", ft);
    }
    let parsed: VolumeList =
        serde_json::from_slice(&resp).context("decode FS_LIST_VOLUMES_ACK body as JSON")?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_response() {
        let body = br#"{"volumes":[
            {"path":"/data","mount_from":"/dev/ssd0.user","fs_type":"nullfs","total_bytes":800000000000,"free_bytes":500000000000,"writable":true,"is_placeholder":false},
            {"path":"/mnt/ext1","mount_from":"/dev/nvme1","fs_type":"bfs","total_bytes":1000000000000,"free_bytes":900000000000,"writable":true,"is_placeholder":false},
            {"path":"/mnt/ext0","mount_from":"tmpfs","fs_type":"tmpfs","total_bytes":2097152,"free_bytes":1736704,"writable":true,"is_placeholder":true}
        ]}"#;
        let parsed: VolumeList = serde_json::from_slice(body).unwrap();
        assert_eq!(parsed.volumes.len(), 3);
        let data = parsed.find("/data").expect("/data present");
        assert_eq!(data.mount_from, "/dev/ssd0.user");
        assert!(!data.is_placeholder);
        assert!(data.is_usable());
        let ext1 = parsed.find("/mnt/ext1").expect("/mnt/ext1 present");
        assert_eq!(ext1.mount_from, "/dev/nvme1");
        assert!(ext1.is_usable());
        let placeholder = parsed.find("/mnt/ext0").expect("placeholder present");
        assert!(placeholder.is_placeholder);
        assert!(!placeholder.is_usable(), "placeholder should not be usable");
    }

    #[test]
    fn parse_empty_list() {
        let parsed: VolumeList = serde_json::from_slice(br#"{"volumes":[]}"#).unwrap();
        assert_eq!(parsed.volumes.len(), 0);
        assert!(parsed.find("/data").is_none());
    }

    #[test]
    fn parse_missing_new_fields_defaults_safely() {
        // Pre-upgrade mock server responses lack mount_from + is_placeholder;
        // serde-default should fill them so the upgrade is non-breaking.
        let body = br#"{"volumes":[{"path":"/data","fs_type":"ufs","total_bytes":100,"free_bytes":50,"writable":true}]}"#;
        let parsed: VolumeList = serde_json::from_slice(body).unwrap();
        assert_eq!(parsed.volumes.len(), 1);
        assert_eq!(parsed.volumes[0].mount_from, "");
        assert!(!parsed.volumes[0].is_placeholder);
    }
}
