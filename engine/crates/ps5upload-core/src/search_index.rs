//! Filesystem search index — build / status / query / cancel.
//!
//! The payload owns a single in-memory index of every regular file
//! under a configurable set of roots. Build runs on a payload-side
//! thread; the renderer polls `index_status` to wait, then issues
//! `search_index` queries against it.
//!
//! Design choice: index lives on the payload, not the desktop. A
//! 200K-file index walked over the LAN every search would be slow
//! and would burn battery on a laptop client. Local payload indexing
//! gives sub-100ms search results regardless of network conditions.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStartResult {
    /// True when a new build started; false if one was already in
    /// progress (caller should poll `index_status` instead).
    pub started: bool,
    /// Reason the build did not start, when `started=false`.
    #[serde(default)]
    pub err: Option<String>,
}

/// Trigger an index build on the payload. `roots` is the list of
/// absolute paths to walk; pass empty to use the payload's default
/// (`/user`, `/data`).
pub fn index_start(addr: &str, roots: &[&str]) -> Result<IndexStartResult> {
    let body = serde_json::json!({ "roots": roots });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::IndexStart, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected INDEX_START: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::IndexStartAck {
        bail!("expected INDEX_START_ACK, got {ft:?}");
    }
    let parsed: IndexStartResult = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStatus {
    /// One of "idle", "building", "ready".
    pub phase: String,
    /// Files counted so far (during build) or total (when ready).
    pub files: u64,
    pub started_at: i64,
    pub completed_at: i64,
}

pub fn index_status(addr: &str) -> Result<IndexStatus> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::IndexStatus, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected INDEX_STATUS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::IndexStatusAck {
        bail!("expected INDEX_STATUS_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Glob: `*.pkg`, `*game*`, etc. Case-insensitive. Matched against
    /// each file's basename, not full path.
    pub query: String,
    #[serde(default)]
    pub size_min: u64,
    #[serde(default)]
    pub size_max: u64,
    #[serde(default)]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    pub results: Vec<SearchHit>,
}

pub fn search_index(addr: &str, q: &SearchQuery) -> Result<SearchResults> {
    let body = serde_json::to_vec(q)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::SearchIndex, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected SEARCH_INDEX: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::SearchIndexAck {
        bail!("expected SEARCH_INDEX_ACK, got {ft:?}");
    }
    Ok(serde_json::from_slice(&resp)?)
}

pub fn index_cancel(addr: &str) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::IndexCancel, &[])?;
    let (hdr, _resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft != FrameType::IndexCancelAck {
        bail!("expected INDEX_CANCEL_ACK, got {ft:?}");
    }
    Ok(())
}
