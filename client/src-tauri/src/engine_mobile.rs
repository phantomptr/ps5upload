//! In-process engine for the Tauri **mobile** build (Android/iOS).
//!
//! Tauri mobile has no sidecar-binary spawn model (that's desktop-only),
//! so instead of extracting and exec'ing `ps5upload-engine`, we link the
//! engine as a library and run its Axum server on a background tokio
//! task (see `BIND_HOST`). The renderer keeps calling
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
const BIND_HOST: &str = "0.0.0.0";
const PORT: u16 = 19113;
/// Tried in order when 19113 is taken. Only the iPhone Simulator (and the Android emulator's
/// host-shared cases) can hit it: a simulator app is a process on the Mac, so a desktop
/// PS5Upload running there already holds 19113 and the in-app engine failed to bind, leaving the
/// simulator's app talking to the desktop engine instead of its own. A phone never shares a port.
const FALLBACK_PORTS: std::ops::RangeInclusive<u16> = 19123..=19132;

/// The engine's loopback URL once `start` chose its port (the renderer reads it through
/// `engine_url_get`).
static URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// `start` has run in this process.
static STARTED_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The first port that can be bound now, 19113 first. A probe bind released at once: the
/// engine binds it a moment later on its own task.
fn pick_port() -> u16 {
    std::iter::once(PORT)
        .chain(FALLBACK_PORTS)
        .find(|p| std::net::TcpListener::bind((BIND_HOST, *p)).is_ok())
        .unwrap_or(PORT)
}

/// Start the engine on a background task. Returns immediately with the
/// loopback URL; the server finishes binding within a few milliseconds.
/// The renderer's engine-status tick tolerates the brief startup race
/// (it retries), matching how the desktop readiness probe is advisory.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    // Android can run setup again when its Activity is recreated while the process lives on;
    // the engine from the first run still holds the port, and a second bind would only fail
    // and tell the renderer the engine did not start.
    if STARTED_ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return Ok(url());
    }
    let port = pick_port();
    // The stream-install URL the console fetches names this port, not the default.
    // Before the engine task starts, as for `PS5UPLOAD_DATA_DIR` below.
    std::env::set_var("PS5UPLOAD_ENGINE_PORT", port.to_string());
    let _ = URL.set(format!("http://127.0.0.1:{port}"));
    let bind = format!("{BIND_HOST}:{port}");
    point_engine_at_app_data(app);
    let task_app = app.clone();
    tokio::spawn(async move {
        if let Err(e) =
            ps5upload_engine::serve_in_process(&bind, DEFAULT_PS5_ADDR.to_string()).await
        {
            let message = format!("in-app engine failed to start on {bind}: {e}");
            let _ = task_app.emit("ps5upload-engine-startup-error", &message);
            // Log and let the renderer surface "engine unreachable" via
            // its normal probe — don't panic the app over it.
            eprintln!("[engine] in-process serve failed: {message}");
        }
    });
    let url = url();
    if port != PORT {
        eprintln!(
            "[engine] 19113 is taken (a desktop PS5Upload on this Mac?); in-app engine on {url}"
        );
    }
    let _ = app.emit("ps5upload-engine-ready", url);
    Ok(url)
}

/// A phone has no home folder, so the engine's data directory (saved connections, AVA1 identity,
/// pairing, job journals) would resolve to nothing: saved connections failed with "no home folder"
/// (#379) and AVA1 reported itself unavailable. Point it at this app's private data folder before
/// the engine reads it (the AVA1 pool and the connection store read the variable when first used,
/// which is after this). An explicit `PS5UPLOAD_DATA_DIR` wins.
fn point_engine_at_app_data(app: &AppHandle) {
    use tauri::Manager;
    // The Payload Library's folder: the in-process /api/payloads and the Payloads screen share it.
    if let Ok(dir) = crate::commands::payloads::library_dir(app) {
        std::env::set_var(ps5upload_core::payload_library::PAYLOADS_DIR_ENV, dir);
    }
    if std::env::var_os("PS5UPLOAD_DATA_DIR").is_some_and(|v| !v.is_empty()) {
        return;
    }
    match app.path().app_data_dir() {
        Ok(base) => {
            let dir = base.join("ps5upload");
            // A factory reset the last run could not finish, its engine still running.
            crate::commands::user_config::finish_engine_data_reset(&base, &dir);
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

/// The URL the renderer should use: loopback on the port `start` chose (19113 unless taken).
pub fn url() -> &'static str {
    URL.get().map(String::as_str).unwrap_or(DEFAULT_ENGINE_URL)
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

async fn probe() -> Result<(), String> {
    match crate::engine_http::engine_client_builder().build() {
        Ok(client) => match client
            .get(format!("{}/api/version", url()))
            .timeout(std::time::Duration::from_millis(800))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("HTTP {}", r.status())),
            Err(e) => Err(crate::engine_http::error_chain(&e)),
        },
        Err(e) => Err(e.to_string()),
    }
}

pub async fn diagnose(_app: &AppHandle) -> Diagnosis {
    let probe = probe().await;
    Diagnosis {
        url: url().to_string(),
        local: true,
        answering: probe.is_ok(),
        probe_error: probe.err(),
        child_running: None,
        port_taken: None,
        binary_found: None,
        binary_error: None,
        log_path: crate::mobile_log::log_path().map(|p| p.display().to_string()),
        os: std::env::consts::OS,
    }
}

/// The in-process engine cannot be restarted without restarting the app.
pub async fn restart(_app: &AppHandle) -> Result<String> {
    Err(anyhow::anyhow!(crate::commands::shell_error::code_only(
        "engine_restart_in_app"
    )))
}

/// No-op on mobile, returning where the proxies stay. The desktop build made the engine URL
/// runtime-configurable (point the app at a remote/self-hosted engine), but mobile links the engine
/// in-process, its native commands always reach that engine and Android allows plain http to
/// loopback only. Defined so the shared command module (`commands::ps5_engine`) compiles for the
/// Android/iOS target. The page does not apply a stored remote URL on a phone either
/// (state/engine.ts).
pub fn set_url(_url: String) -> String {
    url().to_string()
}

/// The app is back in front: if the engine has stopped answering, listen again.
///
/// iOS can reclaim a suspended app's listening sockets (seen by others; not yet measured on an
/// iPhone here), and every renderer call goes through this one. The engine itself is fine, so
/// only the listener is re-created.
pub async fn check_listener() {
    if !STARTED_ONCE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Err(e) = probe().await {
        eprintln!("[engine] not answering after resume ({e}); listening again");
        ps5upload_engine::rebind_in_process();
    }
}
