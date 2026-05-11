//! Save data + screenshot listing over FTX2.
//!
//! Both ops walk a known PS5 path tree on the payload side and return
//! per-entry metadata. The actual download/upload of save data uses
//! the existing FS_READ + transfer paths — these RPCs only do the
//! enumeration.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveEntry {
    pub title_id: String,
    pub user_id: i32,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    /// "ps5" for native, "ps4" for legacy savedata. Lets the UI
    /// group by platform without re-parsing path strings.
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveList {
    pub saves: Vec<SaveEntry>,
}

/// List save data folders. Pass `user_id == 0` to list every user's
/// saves; non-zero filters to one user.
pub fn list_saves(addr: &str, user_id: i32) -> Result<SaveList> {
    let body = serde_json::json!({ "user_id": user_id });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ListSaves, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected LIST_SAVES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ListSavesAck {
        bail!("expected LIST_SAVES_ACK, got {ft:?}");
    }
    let parsed: SaveList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotEntry {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotList {
    pub items: Vec<ScreenshotEntry>,
}

pub fn list_screenshots(addr: &str) -> Result<ScreenshotList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ListScreenshots, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected LIST_SCREENSHOTS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ListScreenshotsAck {
        bail!("expected LIST_SCREENSHOTS_ACK, got {ft:?}");
    }
    let parsed: ScreenshotList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}
