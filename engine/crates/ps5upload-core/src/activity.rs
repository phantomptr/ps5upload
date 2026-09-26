//! Activity tracker proxy: get play-time stats and query the app/sl2 databases.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub launches: u64,
    #[serde(default)]
    pub total_seconds: u64,
    #[serde(default)]
    pub last_launch_ts: i64,
    #[serde(default)]
    pub last_seen_ts: i64,
    #[serde(default)]
    pub session_active: bool,
    /// Installed-title name, joined from app.db by the payload. Absent
    /// when the title is no longer installed -- play time outlives the
    /// game. Every field here is `serde(default)`, so a payload that
    /// spells a key differently reads as a default rather than an error;
    /// the payload's activity_json selftest pins the wire spellings.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityGetResponse {
    #[serde(default)]
    pub titles: Vec<ActivityEntry>,
    #[serde(default)]
    pub current_title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityDbRow {
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub total_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityDbQueryResponse {
    #[serde(default)]
    pub rows: Vec<ActivityDbRow>,
    #[serde(default)]
    pub source: String,
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

pub fn activity_get(addr: &str) -> Result<ActivityGetResponse> {
    let resp = send_recv(
        addr,
        FrameType::ActivityGet,
        FrameType::ActivityGetAck,
        None,
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn activity_db_query(addr: &str, query: &str) -> Result<ActivityDbQueryResponse> {
    let body = serde_json::json!({ "query": query });
    let resp = send_recv(
        addr,
        FrameType::ActivityDbQuery,
        FrameType::ActivityDbQueryAck,
        Some(&serde_json::to_vec(&body)?),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

/// Result of resetting recorded play time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActivityResetResult {
    #[serde(default)]
    pub ok: bool,
    /// How many titles were removed.
    #[serde(default)]
    pub removed: u32,
    #[serde(default)]
    pub error: Option<String>,
}

/// Discard all recorded play time on the console.
///
/// This is ps5upload's own tracking, kept in its own file -- not the
/// console's play-time records, which are not writable. So it resets
/// exactly what the Game Activity screen shows and nothing else.
pub fn activity_reset(addr: &str) -> Result<ActivityResetResult> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ActivityReset, b"")?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected ACTIVITY_RESET: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ActivityResetAck {
        bail!("expected ACTIVITY_RESET_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_activity_get() {
        let json = r#"{
            "titles": [
                {"title_id":"CUSA00001","launches":5,"total_seconds":3600,
                 "last_launch_ts":1700000000,"last_seen_ts":1700003600,
                 "session_active":false}
            ],
            "current_title": "CUSA00002"
        }"#;
        let resp: ActivityGetResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.titles.len(), 1);
        assert_eq!(resp.titles[0].title_id, "CUSA00001");
        assert_eq!(resp.titles[0].launches, 5);
        assert_eq!(resp.current_title, "CUSA00002");
    }

    /// The payload joins an installed-title name onto every tracked entry.
    /// Pinned here because `ActivityEntry` is `serde(default)` throughout:
    /// a key the payload spells differently deserializes to a default
    /// rather than an error, which is how "last_played"/"active" went
    /// unnoticed. The wire spellings themselves are pinned on the payload
    /// side, in payload/tests/activity_json_selftest.c.
    #[test]
    fn tracked_entry_carries_a_joined_title_name() {
        let json = r#"{
            "titles": [
                {"title_id":"CUSA00900","launches":1,"total_seconds":60,
                 "last_launch_ts":1700000000,"last_seen_ts":1700000060,
                 "session_active":true,"name":"Bloodborne"},
                {"title_id":"CUSA11111","launches":1,"total_seconds":60,
                 "last_launch_ts":1700000000,"last_seen_ts":1700000060,
                 "session_active":false}
            ],
            "current_title": ""
        }"#;
        let resp: ActivityGetResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.titles[0].name.as_deref(), Some("Bloodborne"));
        // A deleted game keeps its play time but has no name to join.
        assert_eq!(resp.titles[1].name, None);
        assert!(resp.titles[0].session_active);
        assert_eq!(resp.titles[0].last_launch_ts, 1_700_000_000);
    }

    #[test]
    fn deserialize_activity_get_empty() {
        let json = r#"{}"#;
        let resp: ActivityGetResponse = serde_json::from_str(json).unwrap();
        assert!(resp.titles.is_empty());
    }

    #[test]
    fn deserialize_db_query() {
        let json = r#"{
            "rows": [
                {"title_id":"CUSA00001","name":"Game A"},
                {"title_id":"CUSA00002","name":"Game B","total_seconds":7200}
            ],
            "source": "app_db"
        }"#;
        let resp: ActivityDbQueryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.rows.len(), 2);
        assert_eq!(resp.source, "app_db");
        assert!(resp.error.is_none());
    }

    #[test]
    fn deserialize_db_query_error() {
        let json = r#"{"rows":[],"source":"none","error":"cannot open db"}"#;
        let resp: ActivityDbQueryResponse = serde_json::from_str(json).unwrap();
        assert!(resp.rows.is_empty());
        assert_eq!(resp.error.as_deref(), Some("cannot open db"));
    }
}
