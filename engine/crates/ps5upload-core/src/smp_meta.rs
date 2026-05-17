//! ShadowMountPlus metadata self-healer RPCs.
//!
//! The PS5 payload runs a background worker (`payload/src/smp_meta.c`)
//! that copies missing `icon0.png` / `pic*.png` / `param.json` from
//! `/user/app/<TID>/sce_sys` into `/user/appmeta/<TID>` to fix SMP's
//! "blank home-screen tile" failure mode. Default off. The desktop
//! opts in via `smp_meta_control` with action="start", then polls
//! `smp_meta_stats` periodically to render the panel.
//!
//! All RPCs are synchronous and use a single round-trip on the
//! mgmt-port FTX2 channel (Connection::connect).

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmpMetaControlRequest {
    /// One of "start", "run_now", "set_poll". Anything else is a no-op.
    /// `start` is idempotent — subsequent calls are silently ignored
    /// inside the payload (the worker only launches once per payload
    /// lifetime).
    pub action: String,
    /// Required when `action == "set_poll"`; ignored otherwise.
    /// Payload clamps to [5, 600] and returns the post-clamp value in
    /// the ACK so the desktop slider can reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmpMetaControlAck {
    pub ok: bool,
    /// Post-clamp interval. Present even on failure, set to the
    /// current stored value so the UI never shows a stale slider
    /// after a rejected change.
    #[serde(default)]
    pub poll_seconds: i32,
    /// Sentinel for known failure modes — currently the only one is
    /// `pthread_create_failed`. Empty string on success.
    #[serde(default)]
    pub err: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmpMetaStats {
    pub running: bool,
    pub poll_seconds: i32,
    /// Unix seconds. 0 means "no sweep has completed yet".
    pub last_run_unix: u64,
    pub games_scanned: i32,
    pub icons_healed: i32,
    pub pics_healed: i32,
    pub json_healed: i32,
    pub still_missing: i32,
    /// TITLE_ID of the most recent game whose icon0.png couldn't be
    /// located in either sce_sys/ or the bare app root. Empty when
    /// everything healed cleanly.
    #[serde(default)]
    pub last_missing: String,
}

pub fn smp_meta_control(addr: &str, req: &SmpMetaControlRequest) -> Result<SmpMetaControlAck> {
    let body = serde_json::to_vec(req)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::SmpMetaControl, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected SMP_META_CONTROL: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::SmpMetaControlAck {
        bail!("expected SMP_META_CONTROL_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

pub fn smp_meta_stats(addr: &str) -> Result<SmpMetaStats> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::SmpMetaStats, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected SMP_META_STATS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::SmpMetaStatsAck {
        bail!("expected SMP_META_STATS_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}
