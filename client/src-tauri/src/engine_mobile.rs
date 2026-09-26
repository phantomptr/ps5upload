//! In-process engine for the Tauri **mobile** build (Android/iOS).
//!
//! Tauri mobile has no sidecar-binary spawn model (that's desktop-only),
//! so instead of extracting and exec'ing `ps5upload-engine`, we link the
//! engine as a library and run its Axum server on a background tokio
//! task bound to loopback. The renderer keeps calling
//! `http://127.0.0.1:19113` exactly as on desktop — only the server's
//! host changes from a child process to this in-process task.
//!
//! This module is the `#[cfg(mobile)]` counterpart of `engine.rs` and
//! exposes the same `start` / `stop` / `url` surface the rest of the
//! crate uses. The desktop `engine.rs` is left untouched.

use anyhow::Result;
use tauri::{AppHandle, Emitter};

use crate::DEFAULT_ENGINE_URL;

/// Default PS5 transfer address. The renderer passes `?addr=...` on
/// every call, so this only matters for the few diagnostic endpoints
/// that don't — same contract as the desktop sidecar's `PS5_ADDR`.
const DEFAULT_PS5_ADDR: &str = "192.168.137.2:9113";

/// Bind for the in-process server. Loopback only — there is no PS5
/// pkg-host fetch story on mobile yet, so we don't need the desktop's
/// `0.0.0.0` bind.
const BIND: &str = "127.0.0.1:19113";

/// Start the engine on a background task. Returns immediately with the
/// loopback URL; the server finishes binding within a few milliseconds.
/// The renderer's engine-status tick tolerates the brief startup race
/// (it retries), matching how the desktop readiness probe is advisory.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    let app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = ps5upload_engine::serve_in_process(BIND, DEFAULT_PS5_ADDR.to_string()).await
        {
            let message = format!("Android engine failed to start on {BIND}: {e}");
            let _ = app.emit("ps5upload-engine-startup-error", &message);
            // Log and let the renderer surface "engine unreachable" via
            // its normal probe — don't panic the app over it.
            eprintln!("[engine] in-process serve failed: {message}");
        }
    });
    Ok(DEFAULT_ENGINE_URL)
}

/// No-op on mobile: the in-process server shares the app's lifecycle and
/// is torn down when the process exits. There is no child to kill.
pub async fn stop() {}

/// The URL the renderer should use — fixed loopback, same as desktop.
pub fn url() -> &'static str {
    DEFAULT_ENGINE_URL
}

/// No-op on mobile. The desktop build made the engine URL runtime-configurable
/// (point the app at a remote/self-hosted engine), but mobile links the engine
/// in-process — there is no sidecar and no remote-engine story here, so the
/// `engine_url_set` command is inert. Defined so the shared command module
/// (`commands::ps5_engine`) compiles for the Android/iOS target. The renderer
/// may still store an Engine URL setting; mobile simply ignores it.
pub fn set_url(_url: String) {}
