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

/// Default PS5 address. The renderer passes `?addr=...` on
/// every call, so this only matters for the few diagnostic endpoints
/// that don't — same contract as the desktop sidecar's `PS5_ADDR`.
const DEFAULT_PS5_ADDR: &str = "192.168.137.2";

/// Bind for the in-process server: every interface, like the desktop
/// sidecar, so the PS5 can download a package from the phone (Stream &
/// install). Loopback-only meant a phone could only stage packages on the
/// console first, and that route is refused (0x80B2116F) on FW 9.60 and
/// 13.60 — a user's every install from a phone failed while the same package
/// streamed from a PC installed. The engine's loopback guard still answers
/// only `/pkg-host/*` to anything off this device; `/api/*` stays local.
/// UNVERIFIED on a physical phone: the emulator is NATed, so the PS5 can't
/// reach it to prove the fetch end to end.
const BIND: &str = "0.0.0.0:19113";

/// Start the engine on a background task. Returns immediately with the
/// loopback URL; the server finishes binding within a few milliseconds.
/// The renderer's engine-status tick tolerates the brief startup race
/// (it retries), matching how the desktop readiness probe is advisory.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    point_engine_at_app_data(app);
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

/// A phone has no home folder, so the engine's data directory (saved connections, AVA1 identity,
/// pairing, job journals) would resolve to nothing: saved connections failed with "no home folder"
/// (#379) and AVA1 reported itself unavailable. Point it at this app's private data folder before
/// the engine reads it (the AVA1 pool and the connection store read the variable when first used,
/// which is after this). An explicit `PS5UPLOAD_DATA_DIR` wins.
fn point_engine_at_app_data(app: &AppHandle) {
    use tauri::Manager;
    if std::env::var_os("PS5UPLOAD_DATA_DIR").is_some_and(|v| !v.is_empty()) {
        return;
    }
    match app.path().app_data_dir() {
        Ok(dir) => {
            let dir = dir.join("ps5upload");
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!(
                    "[engine] cannot create the data folder {}: {e}",
                    dir.display()
                );
                return;
            }
            // Set before the engine task starts; nothing else reads or writes the environment
            // concurrently at this point of startup.
            std::env::set_var("PS5UPLOAD_DATA_DIR", &dir);
        }
        Err(e) => eprintln!("[engine] no app data folder for the engine: {e}"),
    }
}

/// No-op on mobile: the in-process server shares the app's lifecycle and
/// is torn down when the process exits. There is no child to kill.
pub async fn stop() {}

/// The URL the renderer should use — fixed loopback, same as desktop.
pub fn url() -> &'static str {
    DEFAULT_ENGINE_URL
}

/// The mobile counterpart of the desktop diagnosis: the engine is in this process, so the
/// only measurable facts are whether it answers and why not.
#[derive(Debug, serde::Serialize)]
pub struct Diagnosis {
    pub url: String,
    pub local: bool,
    pub answering: bool,
    pub probe_error: Option<String>,
    pub child_running: Option<bool>,
    pub port_taken: Option<bool>,
    pub binary_found: Option<bool>,
    pub binary_error: Option<String>,
    pub log_path: Option<String>,
    pub os: &'static str,
}

pub async fn diagnose(_app: &AppHandle) -> Diagnosis {
    let probe = match crate::engine_http::engine_client_builder().build() {
        Ok(client) => match client
            .get(format!("{DEFAULT_ENGINE_URL}/api/version"))
            .timeout(std::time::Duration::from_millis(800))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("HTTP {}", r.status())),
            Err(e) => Err(crate::engine_http::error_chain(&e)),
        },
        Err(e) => Err(e.to_string()),
    };
    Diagnosis {
        url: DEFAULT_ENGINE_URL.to_string(),
        local: true,
        answering: probe.is_ok(),
        probe_error: probe.err(),
        child_running: None,
        port_taken: None,
        binary_found: None,
        binary_error: None,
        log_path: None,
        os: std::env::consts::OS,
    }
}

/// The in-process engine cannot be restarted without restarting the app.
pub async fn restart(_app: &AppHandle) -> Result<String> {
    Err(anyhow::anyhow!(
        "on this device the engine runs inside the app; close the app fully and open it again"
    ))
}

/// No-op on mobile. The desktop build made the engine URL runtime-configurable
/// (point the app at a remote/self-hosted engine), but mobile links the engine
/// in-process — there is no sidecar and no remote-engine story here, so the
/// `engine_url_set` command is inert. Defined so the shared command module
/// (`commands::ps5_engine`) compiles for the Android/iOS target. The renderer
/// may still store an Engine URL setting; mobile simply ignores it.
pub fn set_url(_url: String) {}
