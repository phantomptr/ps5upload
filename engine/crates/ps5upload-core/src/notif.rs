//! Persistent notification browser over AVA1 management.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

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
    let resp = mgmt::call_keep(addr, m::NOTIF_CLEAR, "NOTIF_CLEAR", b"")?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn notif_list(addr: &str, since_seq: u64) -> Result<NotificationList> {
    let body = serde_json::json!({ "since_seq": since_seq });
    let resp = mgmt::call_keep(
        addr,
        m::NOTIF_LIST,
        "NOTIF_LIST",
        &serde_json::to_vec(&body)?,
    )?;
    let parsed: NotificationList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}
