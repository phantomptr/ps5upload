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
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
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
