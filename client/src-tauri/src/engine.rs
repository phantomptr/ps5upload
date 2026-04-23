//! ps5upload-engine sidecar: spawn, probe, shutdown.
//!
//! This is the Rust counterpart of the previous `engine-sidecar.js`.
//! Spawns the engine binary as a child process, waits for the HTTP API
//! on 127.0.0.1:19113 to answer `/api/jobs`, and tears it down cleanly
//! when the app exits. The child is launched with the same PS5_ADDR
//! default the Electron sidecar used — the renderer still passes
//! `?addr=...` on each call, so this default only matters for the few
//! diagnostic endpoints that don't.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tauri::{AppHandle, Manager};
use tokio::process::{Child, Command};
use tokio::sync::OnceCell;
use tokio::time::{sleep, Instant};

const DEFAULT_ENGINE_URL: &str = "http://127.0.0.1:19113";
const READINESS_PROBE: &str = "/api/jobs";
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_POLL: Duration = Duration::from_millis(200);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

static CHILD: OnceCell<tokio::sync::Mutex<Option<Child>>> = OnceCell::const_new();

async fn child_lock() -> &'static tokio::sync::Mutex<Option<Child>> {
    CHILD.get_or_init(|| async { tokio::sync::Mutex::new(None) }).await
}

/// Locate the engine binary. Search order:
///
///   1. `<exe-dir>/resources/engine/<binary>` — Windows portable zip.
///      The `--no-bundle` build has no Tauri Resources dir, so the
///      release workflow packs the engine next to the exe.
///   2. `Resources/engine/<binary>` — clean packaged path.
///   3. `Resources/_up_/_up_/engine/target/release/<binary>` — current
///      packaged path on macOS/Linux bundles because
///      `resources: ["../../engine/..."]` in tauri.conf.json preserves
///      the `../..` segments as `_up_`.
///   4. `<repo>/engine/target/release/<binary>` — dev build.
///   5. `<repo>/engine/target/debug/<binary>` — dev fallback.
fn find_engine_binary(app: &AppHandle) -> Result<PathBuf> {
    let bin_name = if cfg!(target_os = "windows") {
        "ps5upload-engine.exe"
    } else {
        "ps5upload-engine"
    };

    let mut candidates: Vec<PathBuf> = Vec::new();

    // Windows portable: engine lives at <exe-dir>/resources/engine/.
    // `current_exe()` is the path of the running binary; its parent
    // is the directory it sits in. Check this first so portable-zip
    // users don't fall through to the (nonexistent) Resources dir.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("resources").join("engine").join(bin_name));
        }
    }

    if let Ok(rd) = app.path().resource_dir() {
        candidates.push(rd.join("engine").join(bin_name));
        candidates.push(
            rd.join("_up_")
                .join("_up_")
                .join("engine")
                .join("target")
                .join("release")
                .join(bin_name),
        );
    }

    // Dev builds — resolved from CARGO_MANIFEST_DIR at compile time
    // (`desktop/src-tauri`) then up two levels to the repo root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo_root) = manifest_dir.parent().and_then(|p| p.parent()) {
        for profile in ["release", "debug"] {
            candidates.push(
                repo_root
                    .join("engine")
                    .join("target")
                    .join(profile)
                    .join(bin_name),
            );
        }
    }

    for p in &candidates {
        if p.is_file() {
            return Ok(p.clone());
        }
    }

    Err(anyhow!(
        "ps5upload-engine binary not found. Searched:\n  {}\n\
        Build it with `make engine` or \
        `cargo build --release -p ps5upload-engine`.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    ))
}

/// Probe the readiness endpoint. Returns true if the engine answers 200.
async fn probe(url: &str, client: &reqwest::Client) -> bool {
    let u = format!("{url}{READINESS_PROBE}");
    client.get(&u).timeout(Duration::from_millis(500)).send().await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Start the engine child process and wait for HTTP readiness.
///
/// Idempotent: a second call is a no-op if the engine is already up.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    // Fast-path: already running.
    {
        let guard = child_lock().await.lock().await;
        if let Some(ref child) = *guard {
            // try_wait returns Ok(None) while the child is still alive.
            if child.id().is_some() {
                return Ok(DEFAULT_ENGINE_URL);
            }
        }
    }

    let binary = find_engine_binary(app).context("locating ps5upload-engine binary")?;

    let child = Command::new(&binary)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning engine: {}", binary.display()))?;

    // Store the child immediately so shutdown can reach it even if
    // readiness never arrives.
    *child_lock().await.lock().await = Some(child);

    // Pipe engine stdout/stderr to our stderr with a tag. `take()` grabs
    // the handles out of the stored child so the lock drops fast.
    {
        let mut guard = child_lock().await.lock().await;
        if let Some(child) = guard.as_mut() {
            if let Some(out) = child.stdout.take() {
                tokio::spawn(pipe_tagged(out, "[engine] "));
            }
            if let Some(err) = child.stderr.take() {
                tokio::spawn(pipe_tagged(err, "[engine:err] "));
            }
        }
    }

    // Probe HTTP until ready or deadline.
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let client = reqwest::Client::new();
    while Instant::now() < deadline {
        if probe(DEFAULT_ENGINE_URL, &client).await {
            return Ok(DEFAULT_ENGINE_URL);
        }
        // Detect early exit so we surface crashes rather than waiting out
        // the full 30s deadline.
        {
            let mut guard = child_lock().await.lock().await;
            if let Some(child) = guard.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(anyhow!("engine exited during startup: {status}"));
                }
            }
        }
        sleep(READINESS_POLL).await;
    }
    Err(anyhow!(
        "engine did not become ready at {DEFAULT_ENGINE_URL}{READINESS_PROBE} within {:?}",
        READINESS_TIMEOUT
    ))
}

/// Tear down the engine child. SIGTERM then SIGKILL after a grace period.
/// Idempotent — safe to call twice, safe to call when nothing is running.
pub async fn stop() {
    let lock = child_lock().await;
    let mut guard = lock.lock().await;
    let Some(mut child) = guard.take() else { return };

    // Best-effort graceful shutdown. On UNIX we could SIGTERM; tokio's
    // Child::kill() maps to SIGKILL on unix and TerminateProcess on Windows.
    // We time-box the wait in case the child ignores signals.
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        let _ = child.kill().await;
        let _ = child.wait().await;
    })
    .await;
}

/// Read lines from a child's stdio and forward to our stderr with a tag.
async fn pipe_tagged<R>(mut reader: R, tag: &'static str)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    let mut line = String::new();
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if let Ok(s) = std::str::from_utf8(&buf[..n]) {
            for ch in s.chars() {
                if ch == '\n' {
                    eprintln!("{tag}{line}");
                    line.clear();
                } else {
                    line.push(ch);
                }
            }
        }
    }
    if !line.is_empty() {
        eprintln!("{tag}{line}");
    }
}

/// The URL the renderer should use. Currently fixed, but centralised here
/// so we can later negotiate a port if 19113 is taken.
pub fn url() -> &'static str {
    DEFAULT_ENGINE_URL
}
