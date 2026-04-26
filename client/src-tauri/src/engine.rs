//! ps5upload-engine sidecar: spawn, probe, shutdown.
//!
//! This is the Rust counterpart of the previous `engine-sidecar.js`.
//! Spawns the engine binary as a child process, waits for the HTTP API
//! on 127.0.0.1:19113 to answer `/api/jobs`, and tears it down cleanly
//! when the app exits. The child is launched with the same PS5_ADDR
//! default the Electron sidecar used — the renderer still passes
//! `?addr=...` on each call, so this default only matters for the few
//! diagnostic endpoints that don't.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    CHILD
        .get_or_init(|| async { tokio::sync::Mutex::new(None) })
        .await
}

/// Engine binary embedded at compile time via `include_bytes!`. The
/// build script (`build.rs`) sets `PS5UPLOAD_ENGINE_BYTES` to the
/// absolute path of the freshly-built engine binary. Embedding means
/// the desktop exe is self-contained on every platform — no external
/// Resources dir, no sidecar file to ship.
const EMBEDDED_ENGINE: &[u8] = include_bytes!(env!("PS5UPLOAD_ENGINE_BYTES"));

/// Extract the embedded engine binary into the app's local-data dir
/// and return the extracted path. Caches across launches: re-extracts
/// only when the existing file's size differs from the embedded
/// bytes (covers the important "new version installed, same binary
/// length" case; this happens once at launch and the sidecar is small
/// enough that the full byte compare is cheaper than stale-code bugs).
fn find_engine_binary(app: &AppHandle) -> Result<PathBuf> {
    let bin_name = if cfg!(target_os = "windows") {
        "ps5upload-engine.exe"
    } else {
        "ps5upload-engine"
    };

    let cache_root = app
        .path()
        .app_local_data_dir()
        .context("resolving app_local_data_dir")?;
    let out_dir = cache_root.join("engine");
    std::fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    let out_path = out_dir.join(bin_name);

    let needs_extract = match std::fs::metadata(&out_path) {
        Ok(m) if m.len() as usize == EMBEDDED_ENGINE.len() => {
            let current = std::fs::read(&out_path)
                .with_context(|| format!("read engine binary from {}", out_path.display()))?;
            current != EMBEDDED_ENGINE
        }
        Ok(_) => true,
        Err(_) => true,
    };
    if needs_extract {
        std::fs::write(&out_path, EMBEDDED_ENGINE)
            .with_context(|| format!("write engine binary to {}", out_path.display()))?;
        // On Unix, set the executable bit — without it, `spawn`
        // returns EACCES. Windows inherits .exe execution from the
        // extension, no chmod needed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&out_path)
                .with_context(|| format!("stat {} for chmod", out_path.display()))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&out_path, perms)
                .with_context(|| format!("chmod {}", out_path.display()))?;
        }
    }
    Ok(out_path)
}

/// Probe the readiness endpoint. Returns true if the engine answers 200.
async fn probe(url: &str, client: &reqwest::Client) -> bool {
    let u = format!("{url}{READINESS_PROBE}");
    client
        .get(&u)
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Per-startup log file for the engine sidecar's stdout + stderr.
///
/// On Windows the desktop binary is built with
/// `windows_subsystem = "windows"` (no console attached), so the
/// `eprintln!` writes inside `pipe_tagged` go nowhere visible. Without
/// a persistent record, "engine never reached readiness" failures
/// are impossible to diagnose from the user's side. This struct opens
/// a file alongside the extracted engine binary at startup; the
/// `pipe_tagged` workers append to it as bytes arrive. On readiness
/// failure we attach the path to the error so users have something
/// concrete to share in bug reports.
fn open_engine_log(path: &Path) -> Option<Arc<Mutex<std::fs::File>>> {
    use std::fs::OpenOptions;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(|f| Arc::new(Mutex::new(f)))
}

/// Start the engine child process and wait for HTTP readiness.
///
/// Idempotent: a second call is a no-op if the engine is already up.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    // Fast-path: already running.
    {
        let mut guard = child_lock().await.lock().await;
        if guard.is_some() {
            drop(guard);
            let client = reqwest::Client::new();
            if probe(DEFAULT_ENGINE_URL, &client).await {
                return Ok(DEFAULT_ENGINE_URL);
            }
            guard = child_lock().await.lock().await;
            if let Some(mut child) = guard.take() {
                drop(guard);
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
    }

    let binary = find_engine_binary(app).context("locating ps5upload-engine binary")?;
    // Log alongside the extracted binary so the user can reach it via
    // the same path that the engine binary lives at — surfaced in the
    // readiness-timeout error so support requests can include it.
    let log_path = binary
        .parent()
        .map(|d| d.join("engine.log"))
        .unwrap_or_else(|| PathBuf::from("engine.log"));
    let log_writer = open_engine_log(&log_path);
    if let Some(writer) = &log_writer {
        // Stamp a separator so successive starts in the same log are
        // visually distinct; reading the bottom of the file gives the
        // most recent attempt.
        if let Ok(mut f) = writer.lock() {
            use std::io::Write;
            let _ = writeln!(
                f,
                "\n=== engine start @ {} (binary: {}) ===",
                chrono_like_now(),
                binary.display()
            );
        }
    }

    let mut cmd = Command::new(&binary);
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    // Windows: belt-and-braces with the engine's own
    // `windows_subsystem = "windows"` — also pass CREATE_NO_WINDOW so
    // the spawn API never allocates a console for the child. With only
    // the subsystem attr set, certain DLL-load paths (e.g. if a
    // dependency pulls in a subsystem=console initializer) can still
    // briefly flash a terminal. CREATE_NO_WINDOW on the CreationFlags
    // covers that corner.
    #[cfg(target_os = "windows")]
    {
        // tokio::process::Command exposes `creation_flags` directly on
        // Windows — no `use std::os::windows::process::CommandExt`
        // needed (and importing it triggers an unused-import lint
        // because tokio's method shadows the trait method).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawning engine: {}", binary.display()))?;

    // Store the child immediately so shutdown can reach it even if
    // readiness never arrives.
    *child_lock().await.lock().await = Some(child);

    // Pipe engine stdout/stderr to our stderr with a tag AND tee to
    // the persistent log file. `take()` grabs the handles out of the
    // stored child so the lock drops fast.
    {
        let mut guard = child_lock().await.lock().await;
        if let Some(child) = guard.as_mut() {
            if let Some(out) = child.stdout.take() {
                tokio::spawn(pipe_tagged(out, "[engine] ", log_writer.clone()));
            }
            if let Some(err) = child.stderr.take() {
                tokio::spawn(pipe_tagged(
                    err,
                    "[engine:err] ",
                    log_writer.clone(),
                ));
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
                    return Err(anyhow!(
                        "engine exited during startup: {status}\n  log: {}",
                        log_path.display()
                    ));
                }
            }
        }
        sleep(READINESS_POLL).await;
    }
    Err(anyhow!(
        "engine did not become ready at {DEFAULT_ENGINE_URL}{READINESS_PROBE} within {:?}.\n  log: {}\n  Common causes on Windows: SmartScreen / antivirus blocked the freshly-extracted .exe; another process is bound to {DEFAULT_ENGINE_URL}; loopback firewall rule.",
        READINESS_TIMEOUT,
        log_path.display()
    ))
}

/// Best-effort wall-clock stamp for the log header. Avoids pulling in
/// the `chrono` crate just for one timestamp — a SystemTime epoch
/// integer is good enough as a "which run is this" marker. The user
/// reading the log knows roughly when they launched the app.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch {secs}")
}

/// Tear down the engine child. SIGTERM then SIGKILL after a grace period.
/// Idempotent — safe to call twice, safe to call when nothing is running.
pub async fn stop() {
    let lock = child_lock().await;
    let mut guard = lock.lock().await;
    let Some(mut child) = guard.take() else {
        return;
    };

    // Best-effort graceful shutdown. On UNIX we could SIGTERM; tokio's
    // Child::kill() maps to SIGKILL on unix and TerminateProcess on Windows.
    // We time-box the wait in case the child ignores signals.
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        let _ = child.kill().await;
        let _ = child.wait().await;
    })
    .await;
}

/// Read lines from a child's stdio, forward to our stderr with a tag,
/// AND tee to the engine log file (when one was opened — failure to
/// open is silent, the stderr path keeps working).
///
/// Per-line write so log rotation later can split on newlines without
/// risking torn lines mid-write. The Mutex is std (not tokio) because
/// the critical section is a sync `write_all` + flush — no awaits.
async fn pipe_tagged<R>(
    mut reader: R,
    tag: &'static str,
    log: Option<Arc<Mutex<std::fs::File>>>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use std::io::Write as _;
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    let mut line = String::new();
    let emit = |line: &str| {
        eprintln!("{tag}{line}");
        if let Some(writer) = &log {
            if let Ok(mut f) = writer.lock() {
                let _ = writeln!(f, "{tag}{line}");
                let _ = f.flush();
            }
        }
    };
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if let Ok(s) = std::str::from_utf8(&buf[..n]) {
            for ch in s.chars() {
                if ch == '\n' {
                    emit(&line);
                    line.clear();
                } else {
                    line.push(ch);
                }
            }
        }
    }
    if !line.is_empty() {
        emit(&line);
    }
}

/// The URL the renderer should use. Currently fixed, but centralised here
/// so we can later negotiate a port if 19113 is taken.
pub fn url() -> &'static str {
    DEFAULT_ENGINE_URL
}
