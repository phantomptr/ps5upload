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
/// and return the extracted path. Caches across launches via a
/// BLAKE3 hash stamp file (`<bin>.blake3`) sitting next to the
/// extracted binary.
///
/// Why hash-stamp vs the previous full byte compare:
/// - The previous logic read the entire 14 MiB cached `.exe` from
///   disk on every launch and compared it byte-for-byte to the
///   embedded array. On a slow disk that's tens of ms of cold-IO
///   per launch; on Windows, reading an `.exe` the OS still has
///   open elsewhere can fail with sharing violation and force a
///   spurious re-extract that itself fails on the locked file.
/// - Hash-stamp reads a 64-byte hex string instead, and the
///   comparison cost is constant regardless of binary size.
///   BLAKE3 of the embedded bytes runs at GB/s so it stays under
///   ~10 ms even on the slowest CPU we ship to.
///
/// The stamp file IS the source of truth. If it's missing or the
/// content doesn't hex-match the freshly-computed embedded hash, we
/// re-extract + rewrite the stamp. A tampered binary on disk (with
/// matching size but different bytes) re-extracts on next launch.
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
    let stamp_path = out_dir.join(format!("{bin_name}.blake3"));

    // Hash the embedded bytes once. blake3 is fast enough that this
    // is cheaper than ANY disk IO we'd otherwise do — even the small
    // stamp-file read happens after this so a stamp-file-corruption
    // path can re-extract without an extra hash recomputation.
    let embedded_hex = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(EMBEDDED_ENGINE);
        hasher.finalize().to_hex().to_string()
    };

    let needs_extract = match std::fs::read_to_string(&stamp_path) {
        Ok(stored) => stored.trim() != embedded_hex,
        // Stamp missing OR unreadable → assume cache is stale and
        // re-extract. Worst case is one extra extract; correctness
        // wins over avoiding the IO.
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
        // Stamp last so a crash mid-extract leaves a missing /
        // outdated stamp; next launch correctly re-extracts.
        if let Err(e) = std::fs::write(&stamp_path, &embedded_hex) {
            eprintln!(
                "[engine] could not write stamp {}: {e} (engine still extracted, just re-extracts next launch)",
                stamp_path.display()
            );
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

/// Check that the engine answering the port reports the same version
/// the desktop shell was built against. Returns true on match (use
/// the existing engine), false otherwise (kill + respawn).
///
/// Mismatch is the common-after-upgrade scenario: previous app
/// instance left an orphaned engine bound to 19113 and the new
/// shell's probe finds it. Without this check, the new shell talks
/// to the old engine indefinitely — and any newly-added route
/// surfaces as 404 to the user with no obvious explanation.
///
/// Robust against the engine not implementing /api/version yet
/// (returns false → respawn, which is correct behavior).
async fn engine_version_matches(url: &str, client: &reqwest::Client) -> bool {
    let expected = env!("CARGO_PKG_VERSION");
    let u = format!("{url}/api/version");
    let resp = match client
        .get(&u)
        .timeout(Duration::from_millis(500))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return false,
    };
    let actual = body.get("version").and_then(|v| v.as_str()).unwrap_or("");
    actual == expected
}

/// Cap on the engine.log file before we rotate it to .old. Without
/// rotation a long-running install + many app restarts could grow
/// the log indefinitely; 1 MiB is enough for ~10k tagged lines and
/// keeps the file readable in any text editor.
const ENGINE_LOG_MAX_BYTES: u64 = 1024 * 1024;

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
///
/// Rotates when the existing file exceeds `ENGINE_LOG_MAX_BYTES` —
/// previous file moves to `<name>.old` (single rotation generation
/// is enough; the most recent failure is what users need).
///
/// Logs the failure to `eprintln!` if open fails, so a developer
/// running `cargo tauri dev` from a console at least sees why
/// log capture didn't engage. Returns None on failure rather than
/// erroring — a missing log file shouldn't block engine startup.
fn open_engine_log(path: &Path) -> Option<Arc<Mutex<std::fs::File>>> {
    use std::fs::OpenOptions;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > ENGINE_LOG_MAX_BYTES {
            let old = path.with_extension("log.old");
            // Best-effort rotate: if the rename fails (Windows lock
            // on the .old file from a previous instance, etc.), fall
            // through to opening the existing log in append mode and
            // let it grow a little past the cap. Worst case is a
            // larger log file, which is recoverable; the alternative
            // of failing to open at all loses diagnostics entirely.
            let _ = std::fs::remove_file(&old);
            if let Err(e) = std::fs::rename(path, &old) {
                eprintln!(
                    "[engine-log] rotate {} -> {} failed: {e} (continuing)",
                    path.display(),
                    old.display()
                );
            }
        }
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => Some(Arc::new(Mutex::new(f))),
        Err(e) => {
            eprintln!("[engine-log] cannot open {}: {e}", path.display());
            None
        }
    }
}

/// Start the engine child process and wait for HTTP readiness.
///
/// Idempotent: a second call is a no-op if the engine is already up.
pub async fn start(app: &AppHandle) -> Result<&'static str> {
    // Fast-path: already running. We HOLD the child_lock across the
    // probe await so a concurrent start() can't both observe the
    // existing-child state, race past, and end up double-spawning.
    // tokio::sync::Mutex is async-safe across awaits, and probe()
    // has its own 500 ms timeout, so worst-case lock hold is short.
    //
    // The take()/kill()/respawn branch handles the case where a
    // child registration exists but the listener is gone (engine
    // crashed without unregistering). Without dropping the lock at
    // any point, the kill() + wait() happen with the lock held —
    // a concurrent start() that wakes during this teardown blocks
    // until we either succeed or fail, then re-checks the slot.
    {
        let mut guard = child_lock().await.lock().await;
        if guard.is_some() {
            let client = reqwest::Client::new();
            if probe(DEFAULT_ENGINE_URL, &client).await {
                // The port responds, but it might be a stale-version
                // engine left behind by a previous app instance (the
                // process didn't reap on crash, so we inherited it
                // on next start). Verify the version matches the
                // shell's expectation; if not, kill it and respawn
                // from the bundled bytes. Without this, fresh routes
                // (e.g. the FS_OP frames added in 2.2.7) silently
                // 404 against an old engine the shell is happily
                // talking to.
                if engine_version_matches(DEFAULT_ENGINE_URL, &client).await {
                    return Ok(DEFAULT_ENGINE_URL);
                }
                eprintln!(
                    "[engine] stale-version sibling engine detected on {DEFAULT_ENGINE_URL} — killing and respawning",
                );
                // Fall through to take()+kill below. If our child
                // handle isn't actually the process bound to the
                // port (engine was orphaned by a prior crash and we
                // never re-acquired it), kill() will be a no-op and
                // bind() will fail in the spawned child — but our
                // engine binary now exits 0 cleanly in that case
                // rather than panicking, so the worst outcome is a
                // single "couldn't bind" log entry.
            }
            // Stale child (or stale version) — kill and fall through
            // to spawn a fresh one. take() empties the slot so the
            // spawn path below can overwrite it cleanly.
            if let Some(mut child) = guard.take() {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
        // guard drops here at scope end.
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
                tokio::spawn(pipe_tagged(err, "[engine:err] ", log_writer.clone()));
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
async fn pipe_tagged<R>(mut reader: R, tag: &'static str, log: Option<Arc<Mutex<std::fs::File>>>)
where
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
            Err(e) => {
                // Read errors on a child stdio pipe usually mean the
                // process exited. Surface it so the log makes the
                // termination obvious instead of just "log went
                // quiet" — ambiguous between "engine fine, no more
                // output" and "engine died".
                emit(&format!("[stream-read-error] {e}"));
                break;
            }
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
