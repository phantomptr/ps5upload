//! CLEANUP RPC — asks the payload to recursively remove a path under one of
//! its allowlisted prefixes (see `payload2/src/runtime.c CLEANUP_ALLOWED_PREFIXES`).
//!
//! This is *not* a general-purpose delete primitive; the payload refuses
//! anything outside `/data/ps5upload-bench/`, `/data/ps5upload-sweep/`, or
//! `/data/ps5upload-smoke/`. Intended use: bench sweep + smoke harness
//! reset-between-profiles so generated artifacts do not pile up on PS5.

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupResult {
    pub ok: bool,
    pub path: String,
    pub removed_files: u64,
    pub removed_dirs: u64,
}

/// Connect, send CLEANUP `{"path":...}`, await CLEANUP_ACK, return parsed body.
///
/// Returns an error if the payload rejects the path (e.g. `cleanup_path_denied`),
/// returns an I/O error, or replies with an unexpected frame type. The latter
/// two map to typical anyhow errors; the first surfaces the payload error
/// string verbatim so bench tooling can display it.
pub fn cleanup_path(addr: &str, path: &str) -> Result<CleanupResult> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::to_vec(&serde_json::json!({ "path": path }))
        .context("serialize cleanup body")?;
    c.send_frame(FrameType::Cleanup, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload refused cleanup: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::CleanupAck {
        bail!("expected CLEANUP_ACK, got {:?}", ft);
    }
    let parsed: CleanupResult =
        serde_json::from_slice(&resp).context("decode CLEANUP_ACK body as JSON")?;
    Ok(parsed)
}
