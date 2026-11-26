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

/// The port `start` chose; a retry binds the same one, so the URL the renderer holds stays right.
static PORT_CHOSEN: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

/// `start` has run in this process.
static STARTED_ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The engine's server task is running. Its first step is binding the port, and it ends at once
/// when it cannot.
static SERVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Why the engine is not answering, as a coded error (`engine_in_app.rs`), until it answers.
/// The renderer reads it through `engine_diagnose`: the startup event alone can fire before the
/// page listens, and it was the only place the reason went (#950).
static FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// How long the engine gets to answer after a start, a retry or a return to the app. Its first
/// start sets up its data folder and journal before it serves (about 1.5 s in the simulator).
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(20);

/// The first port that can be bound now, 19113 first. A probe bind released at once: the
/// engine binds it a moment later on its own task.
fn pick_port() -> u16 {
    std::iter::once(PORT)
        .chain(FALLBACK_PORTS)
        .find(|p| std::net::TcpListener::bind((BIND_HOST, *p)).is_ok())
        .unwrap_or(PORT)
}

fn chosen_port() -> u16 {
    PORT_CHOSEN.get().copied().unwrap_or(PORT)
}

/// One line to the app's engine.log (`mobile_log.rs`), which the bug report carries.
fn log_line(text: &str) {
    let _ = crate::crash_hook::stderr_line(&format!("[engine] {text}"));
}

fn set_failure(app: &AppHandle, coded: String) {
    log_line(&format!("in-app engine not answering: {coded}"));
    if let Ok(mut f) = FAILURE.lock() {
        *f = Some(coded.clone());
    }
    let _ = app.emit("ps5upload-engine-startup-error", &coded);
}

fn clear_failure() {
    if let Ok(mut f) = FAILURE.lock() {
        *f = None;
    }
}

fn failure() -> Option<String> {
    FAILURE.lock().ok().and_then(|f| f.clone())
}

/// Something holds the engine's port: binding it fails with "in use". That is the engine's own
/// listener, as the port was free when `start` chose it.
fn port_held() -> bool {
    matches!(
        std::net::TcpListener::bind((BIND_HOST, chosen_port())),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse
    )
}

/// Run the engine's server on a task of its own and record how it ends: its error or its panic.
/// A panic used to end the task with no word anywhere but stderr.
fn spawn_serve(app: &AppHandle) {
    use crate::engine_in_app::Ended;
    let bind = format!("{BIND_HOST}:{}", chosen_port());
    SERVING.store(true, std::sync::atomic::Ordering::Relaxed);
    let task_app = app.clone();
    tokio::spawn(async move {
        let serve_bind = bind.clone();
        let served = tokio::spawn(async move {
            ps5upload_engine::serve_in_process(&serve_bind, DEFAULT_PS5_ADDR.to_string()).await
        })
        .await;
        let ended = match served {
            Ok(Ok(())) => Ended::Stopped,
            Ok(Err(e)) => Ended::Error(format!("{e:#}")),
            Err(e) if e.is_panic() => {
                let p = e.into_panic();
                Ended::Panic(
                    p.downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "no message".to_string()),
                )
            }
            Err(e) => Ended::Error(e.to_string()),
        };
        // The reason first: a waiter that sees the task gone finds it.
        set_failure(&task_app, crate::engine_in_app::ended(&ended, &bind));
        SERVING.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

fn serving() -> bool {
    SERVING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Wait until the engine answers, up to `within`. The last connection error when it does not.
/// An answer counts only while the server task runs: in the iPhone Simulator a desktop engine on
/// the same port answers for an in-app engine that failed to bind.
async fn wait_answering(within: std::time::Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + within;
    loop {
        // The server task ended: waiting longer cannot help, and it recorded why.
        if !serving() {
            return Err("it is not running".to_string());
        }
        let last = match probe().await {
            Ok(()) if serving() => return Ok(()),
            Ok(()) => continue,
            Err(e) => e,
        };
        if std::time::Instant::now() >= deadline {
            return Err(last);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Wait for the engine to answer and record the outcome: a line in engine.log either way and,
/// when it does not answer, the reason for the renderer (the coded error).
async fn confirm_answering(app: &AppHandle, what: &str) -> Result<(), String> {
    let started = std::time::Instant::now();
    match wait_answering(ANSWER_WITHIN).await {
        Ok(()) => {
            clear_failure();
            log_line(&format!(
                "{what}: answering at {} after {} ms",
                url(),
                started.elapsed().as_millis()
            ));
            Ok(())
        }
        Err(last) => {
            // A server task that ended has said why; otherwise say what was measured.
            if serving() || failure().is_none() {
                set_failure(app, crate::engine_in_app::not_answering(&last, port_held()));
            }
            Err(failure().unwrap_or(last))
        }
    }
}

/// Start the engine on a background task. Returns immediately with the loopback URL; a watcher
/// then waits for the engine to answer and records why when it does not.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    // Android can run setup again when its Activity is recreated while the process lives on;
    // the engine from the first run still holds the port, and a second bind would only fail
    // and tell the renderer the engine did not start.
    if STARTED_ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return Ok(url());
    }
    let port = pick_port();
    let _ = PORT_CHOSEN.set(port);
    // The stream-install URL the console fetches names this port, not the default.
    // Before the engine task starts, as for `PS5UPLOAD_DATA_DIR` below.
    std::env::set_var("PS5UPLOAD_ENGINE_PORT", port.to_string());
    let _ = URL.set(format!("http://127.0.0.1:{port}"));
    point_engine_at_app_data(app);
    spawn_serve(app);
    let url = url();
    if port != PORT {
        log_line(&format!(
            "19113 is taken (a desktop PS5Upload on this Mac?); in-app engine on {url}"
        ));
    }
    let watch_app = app.clone();
    tokio::spawn(async move {
        let _ = confirm_answering(&watch_app, "started").await;
    });
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
    /// Why the engine is not answering, as a coded error (`engine_in_app.rs`). None while it
    /// answers, and before it has had its time to start.
    pub startup_error: Option<String>,
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
    // Only this engine's answer counts (see `wait_answering`).
    let answering = probe.is_ok() && serving();
    Diagnosis {
        url: url().to_string(),
        local: true,
        answering,
        probe_error: probe.err(),
        child_running: Some(serving()),
        port_taken: Some(port_held()),
        binary_found: None,
        binary_error: None,
        log_path: crate::mobile_log::log_path().map(|p| p.display().to_string()),
        os: std::env::consts::OS,
        startup_error: if answering { None } else { failure() },
    }
}

/// "Try again" for the engine inside the app: a server that ended is started again on the same
/// port, and one still running gets a new listener (as after a return to the app). Then it gets
/// its time to answer. The error is the coded reason when it still does not.
pub async fn restart(app: &AppHandle) -> Result<String> {
    if !STARTED_ONCE.load(std::sync::atomic::Ordering::Relaxed) {
        start(app).await?;
    } else if serving() {
        log_line("try again: listening again");
        ps5upload_engine::rebind_in_process();
    } else {
        log_line("try again: starting the engine again");
        clear_failure();
        spawn_serve(app);
    }
    confirm_answering(app, "try again")
        .await
        .map(|()| url().to_string())
        .map_err(|e| anyhow::anyhow!(e))
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
/// iPhone here), and every renderer call goes through this one. A running engine only gets a new
/// listener; one whose server ended is started again.
pub async fn check_listener(app: AppHandle) {
    if !STARTED_ONCE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if serving() {
        if let Err(e) = probe().await {
            log_line(&format!(
                "not answering after resume ({e}); listening again"
            ));
            ps5upload_engine::rebind_in_process();
            let _ = confirm_answering(&app, "after resume").await;
        }
    } else {
        log_line("not running after resume; starting it again");
        spawn_serve(&app);
        let _ = confirm_answering(&app, "after resume").await;
    }
}
