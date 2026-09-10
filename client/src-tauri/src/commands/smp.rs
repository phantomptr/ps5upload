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

// ─── Edit sessions (checkout / check-in) ─────────────────────────────────
//
// ShadowMount+ mounts everything read-only and re-attaches any image whose
// mount disappears, so editing an image in place means taking it out of SMP's
// scan folders for the duration. `ps5upload_core::smp_checkout` owns that
// sequence — including the on-console journal that makes an interrupted
// session recoverable. These are thin command wrappers over it.

use ps5upload_core::fs_ops::MountResult;
use ps5upload_core::smp_checkout::{self, CheckoutState};
use ps5upload_core::smp_image_rw::{self, ImageRwSession};

/// What, if anything, is currently checked out for editing on this console.
///
/// The renderer calls this on connect: an edit session interrupted by a crash
/// or a reboot leaves the image outside SMP's scan folders, where the user's
/// game has effectively vanished, and this is how we offer to put it back.
#[tauri::command]
pub async fn smp_checkout_status(addr: String) -> Result<Option<CheckoutState>, String> {
    tokio::task::spawn_blocking(move || smp_checkout::read_state(&addr))
        .await
        .map_err(|e| format!("smp_checkout_status task: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[derive(serde::Serialize)]
pub struct CheckoutBeginResult {
    pub checkout: CheckoutState,
    pub mount: MountResult,
}

/// Take `image_path` out of ShadowMount+'s scan folders and mount it
/// read-write at `mount_point`.
///
/// Slow on purpose — it blocks until ShadowMount+'s scan sweep (15 s by
/// default) releases its own read-only mount. The renderer should show this
/// as a running task rather than a click-and-wait.
#[tauri::command]
pub async fn smp_checkout_begin(
    addr: String,
    image_path: String,
    mount_point: String,
    title_id: Option<String>,
) -> Result<CheckoutBeginResult, String> {
    tokio::task::spawn_blocking(move || {
        smp_checkout::begin(
            &addr,
            &image_path,
            &mount_point,
            title_id.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(|e| format!("smp_checkout_begin task: {e}"))?
    .map(|(checkout, mount)| CheckoutBeginResult { checkout, mount })
    .map_err(|e| format!("{e:#}"))
}

/// Unmount the edited image, put it back where ShadowMount+ will find it, and
/// clear the journal. Safe to call for a session recovered after a crash.
#[tauri::command]
pub async fn smp_checkout_finish(addr: String) -> Result<CheckoutState, String> {
    tokio::task::spawn_blocking(move || smp_checkout::finish(&addr))
        .await
        .map_err(|e| format!("smp_checkout_finish task: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

/// Read/write image sessions used by one-button backporting. Unlike checkout,
/// the image stays beside its siblings and is only briefly renamed so SMP
/// remounts it under a temporary `image_rw=` rule.
#[tauri::command]
pub async fn smp_image_rw_status(addr: String) -> Result<Option<ImageRwSession>, String> {
    tokio::task::spawn_blocking(move || smp_image_rw::read_state(&addr))
        .await
        .map_err(|e| format!("smp_image_rw_status task: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn smp_image_rw_begin(addr: String, title_id: String) -> Result<ImageRwSession, String> {
    tokio::task::spawn_blocking(move || smp_image_rw::begin(&addr, &title_id))
        .await
        .map_err(|e| format!("smp_image_rw_begin task: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn smp_image_rw_finish(addr: String) -> Result<ImageRwSession, String> {
    tokio::task::spawn_blocking(move || smp_image_rw::finish(&addr))
        .await
        .map_err(|e| format!("smp_image_rw_finish task: {e}"))?
        .map_err(|e| format!("{e:#}"))
}
