//! Persistent notification browser over FTX2.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub ts: i64,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub read: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationList {
    #[serde(default)]
    pub notifications: Vec<Notification>,
}

/// Result of clearing the notification ring.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifClearResult {
    #[serde(default)]
    pub ok: bool,
    /// How many entries were removed.
    #[serde(default)]
    pub removed: u32,
    #[serde(default)]
    pub error: Option<String>,
}

/// Empty the payload's notification ring.
///
/// These entries are messages ps5upload itself put on the console's
/// screen and kept in its own ring buffer -- not Sony's notification
/// panel, which is not readable. So this really does clear everything
/// the screen can show.
pub fn notif_clear(addr: &str) -> Result<NotifClearResult> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::NotifClear, b"")?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected NOTIF_CLEAR: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::NotifClearAck {
        bail!("expected NOTIF_CLEAR_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

pub fn notif_list(addr: &str, since_seq: u64) -> Result<NotificationList> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::json!({ "since_seq": since_seq });
    c.send_frame(FrameType::NotifList, &serde_json::to_vec(&body)?)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected NOTIF_LIST: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::NotifListAck {
        bail!("expected NOTIF_LIST_ACK, got {ft:?}");
    }
    let parsed: NotificationList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}
