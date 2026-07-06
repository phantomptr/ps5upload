//! ShadowMount+ integration — read-only awareness.
//!
//! Thin Tauri command layer over `ps5upload_core::smp::collect_status`.
//! The snapshot logic itself lives in `ps5upload-core` so the engine's
//! browser-facing HTTP route can share it — see that module's doc
//! comment for detection signals and the read-only design rationale.

use ps5upload_core::smp::{collect_status, SmpStatus};

/// One-shot status snapshot. Tauri command — invoked from the
/// Library tab's SMP panel mount + refresh button.
///
/// `addr` is the management-port address ("ip:9114"). Renderer is
/// expected to construct it via the existing `toMgmtAddr` helper.
///
/// Runs on a `spawn_blocking` so we don't tie up the async reactor;
/// `collect_status` is sync inside `ps5upload-core`.
#[tauri::command]
pub async fn smp_status(addr: String) -> Result<SmpStatus, String> {
    tokio::task::spawn_blocking(move || collect_status(&addr))
        .await
        .map_err(|e| format!("smp_status task: {e}"))?
        .map_err(|e| format!("{e:#}"))
}
