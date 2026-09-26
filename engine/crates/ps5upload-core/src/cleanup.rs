//! CLEANUP RPC — asks the payload to recursively remove a path under one of
//! its allowlisted prefixes (see `payload/src/runtime.c cleanup_path_allowed`).
//!
//! This is *not* a general-purpose delete primitive; the payload refuses
//! anything outside the unified test sandbox:
//!
//!   - `/data/ps5upload/tests[/...]`
//!   - `/mnt/{ext,usb}<digits>/ps5upload/tests[/...]`
//!
//! Intended use: bench sweep + smoke harness reset-between-profiles so
//! generated artifacts do not pile up on PS5.

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Read-timeout cap for the CLEANUP → CLEANUP_ACK wait.
///
/// CLEANUP is one frame that asks the payload to recursively unlink an
/// entire tree, so the ACK cannot arrive until the last file is gone.
/// Under the connection's default 30 s that made cleanup of a large
/// sandbox fail with a bare "Resource temporarily unavailable (os error
/// 35)" — observed live on FW 5.10 clearing ~20k files, where the call
/// errored despite the payload still deleting successfully (a retry
/// picked up where it left off and reported the remaining count).
///
/// Same reasoning and shape as `transfer::COMMIT_TX_ACK_TIMEOUT`: outlast
/// a realistic worst case, but still surface a genuinely wedged payload.
/// A crashed console surfaces immediately via TCP RST regardless.
const CLEANUP_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

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
    // Raise before sending: the payload starts unlinking as soon as the
    // frame lands, and the whole recursive delete happens before it acks.
    c.set_io_timeout(CLEANUP_ACK_TIMEOUT)
        .context("raise read timeout for CLEANUP_ACK wait")?;
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
