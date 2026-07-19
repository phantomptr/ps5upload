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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::{Child, Command};
use tokio::sync::OnceCell;
use tokio::time::{sleep, Instant};

use crate::DEFAULT_ENGINE_URL;

const READINESS_PROBE: &str = "/api/jobs";
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_POLL: Duration = Duration::from_millis(200);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

static CHILD: OnceCell<tokio::sync::Mutex<Option<Child>>> = OnceCell::const_new();

/// True while our spawned sidecar child is (believed to be) alive. A
/// sync mirror of "the CHILD slot is occupied", set right after spawn and
/// cleared on stop/exit/teardown, so `set_url` — which is sync and can't
/// await the async CHILD lock — can cheaply tell whether a local sidecar
/// is running. Used to stop a settings-sync from relocating a running
/// sidecar off its (possibly fallback) port.
static LOCAL_CHILD_RUNNING: AtomicBool = AtomicBool::new(false);

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

    // Re-extract if the stamp is missing/stale OR the binary itself is gone.
    // Checking only the stamp leaves a permanent-failure hole: if the binary
    // is removed but the tiny text stamp survives (Windows SmartScreen/AV
    // quarantines the freshly-extracted .exe, a disk-cleaner reaps the ~14 MiB
    // binary but keeps the stamp), `stored == embedded_hex` makes needs_extract
    // false and we hand back a path to a nonexistent file — spawn then fails on
    // every launch with no self-heal. Requiring the binary to exist restores
    // the cross-restart recovery (re-extract rewrites the binary, re-applies
    // the exec bit, and rewrites the stamp).
    let needs_extract = !out_path.exists()
        || match std::fs::read_to_string(&stamp_path) {
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

/// Recover from a leftover engine that some prior crashed launch left
/// holding port 19113. The new (2.2.22+) engine has a stdin-EOF parent
/// watcher that prevents this scenario going forward — but a user
/// upgrading from an older build still has the orphan from their last
/// session. Without this helper, every launch after that point fails
/// with "engine did not become ready" until they manually
/// `taskkill`/`pkill` the orphan from a terminal.
///
/// We shell out to a per-OS port-killer because that's far simpler
/// than pulling in `sysinfo` / `windows-sys` / `/proc` parsing — the
/// commands are universally present (PowerShell on Win10+, lsof on
/// macOS by default, fuser/ss on every Linux distro). Each command is
/// best-effort; any failure is swallowed because the worst case is
/// the existing readiness probe times out and the user sees the same
/// error they would have seen anyway.
///
/// Only called when the readiness probe finds *something* on 19113
/// that isn't a healthy version-matched engine — i.e. when we already
/// know the port is occupied by a bad citizen.
async fn reap_orphan_listener_on(port: u16) {
    use tokio::process::Command as TokioCommand;
    eprintln!("[engine] attempting to reap orphan listener on :{port}");
    #[cfg(target_os = "windows")]
    let attempts: &[(&str, &[&str])] = &[
        // PowerShell (Win10+ default). Get-NetTCPConnection +
        // Stop-Process is the cleanest path; the wrapper script
        // handles "no such connection" without erroring.
        (
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                // Inline; the parent shell cmd substitutes {port} below.
                "$ErrorActionPreference='SilentlyContinue'; \
                 $c = Get-NetTCPConnection -LocalPort $env:PORT -State Listen -ErrorAction SilentlyContinue; \
                 if ($c) { Stop-Process -Id $c.OwningProcess -Force }",
            ],
        ),
        // Fallback: classic netstat + taskkill. Works on Server SKUs
        // that may have PowerShell stripped down.
        //
        // `findstr LISTENING` runs FIRST so only listener rows survive
        // (their foreign-address column is always `0.0.0.0:0`, which
        // can't collide with our port). Then `findstr /C:":%PORT% "`
        // is a *literal* match anchored by a trailing space: that
        // pins it to the end of the local-address column and stops
        // `:9113` from also matching a `:91130`-style neighbour or a
        // bare `9113` substring elsewhere on the line. The old
        // `findstr :%PORT%` was an unanchored substring search and
        // could taskkill an unrelated PID.
        (
            "cmd.exe",
            &[
                "/C",
                "for /f \"tokens=5\" %a in ('netstat -ano ^| findstr LISTENING ^| findstr /C:\":%PORT% \"') do taskkill /F /PID %a",
            ],
        ),
    ];
    #[cfg(target_os = "macos")]
    let attempts: &[(&str, &[&str])] = &[
        // lsof + xargs kill -9. lsof is preinstalled.
        (
            "/bin/sh",
            &["-c", "lsof -ti tcp:$PORT -sTCP:LISTEN | xargs -r kill -9"],
        ),
    ];
    #[cfg(target_os = "linux")]
    let attempts: &[(&str, &[&str])] = &[
        // fuser is in psmisc, present on every distro we care about.
        ("/bin/sh", &["-c", "fuser -k -n tcp $PORT"]),
        // Fallback if fuser is missing for some reason.
        ("/bin/sh", &["-c", "ss -ltnp \"sport = :$PORT\" 2>/dev/null | awk -F'pid=' 'NR>1 {split($2,a,\",\"); print a[1]}' | xargs -r kill -9"]),
    ];
    for (program, args) in attempts {
        let mut cmd = TokioCommand::new(program);
        cmd.args(*args).env("PORT", port.to_string());
        // 2 s ceiling — the kill should be near-instant; if it hangs
        // (e.g. a wedged shell host), don't block engine startup.
        let res = tokio::time::timeout(Duration::from_secs(2), cmd.status()).await;
        match res {
            Ok(Ok(status)) if status.success() => {
                eprintln!("[engine] orphan reaper ({program}) exited {status}");
                break;
            }
            Ok(Ok(status)) => {
                eprintln!("[engine] orphan reaper ({program}) exited {status} — trying next");
            }
            Ok(Err(e)) => {
                eprintln!("[engine] orphan reaper ({program}) failed to spawn: {e}");
            }
            Err(_) => {
                eprintln!("[engine] orphan reaper ({program}) timed out");
            }
        }
    }
    // Give the OS a beat to actually release the port — kill returns
    // before the kernel finishes reaping the dead process's sockets.
    sleep(Duration::from_millis(300)).await;
}

/// True if loopback `port` can be bound right now (nothing is holding
/// it). Binds and immediately drops, so there's a tiny TOCTOU window
/// before the engine itself binds — acceptable here: the only racer on
/// the preferred port is a process we'd want to fall back away from
/// anyway, and the engine's own bind failure is surfaced by the
/// readiness path.
fn loopback_port_free(port: u16) -> bool {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
}

/// Ask the OS for a free loopback port (bind :0, read the assigned
/// port, drop the listener). Used as a fallback when the preferred
/// engine port is occupied by something we couldn't reclaim, so the
/// desktop can still bring up its own engine instead of failing to
/// start at all.
fn pick_free_loopback_port() -> Option<u16> {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Extract the TCP port from a `http://host:port` URL, defaulting to
/// 19113 when it can't be parsed. The local sidecar is always
/// `127.0.0.1`/`localhost` (no IPv6 literal), so a trailing-colon split
/// is enough.
fn port_of(url: &str) -> u16 {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(19113)
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
pub async fn start(app: &AppHandle) -> Result<String> {
    // Seed the configured URL from settings.json before the spawn
    // decision — renderer hydration runs too late to gate it here.
    if let Some(saved) = crate::commands::user_config::load_engine_url(app) {
        set_url(saved);
    }
    let configured = url();

    // Remote engine: don't spawn the bundled sidecar, just probe the
    // remote API for readiness and hand back its URL.
    if !is_loopback_url(&configured) {
        eprintln!("[engine] remote engine configured ({configured}); skipping local sidecar");
        let client = reqwest::Client::new();
        let deadline = Instant::now() + READINESS_TIMEOUT;
        while Instant::now() < deadline {
            if probe(&configured, &client).await {
                return Ok(configured);
            }
            sleep(READINESS_POLL).await;
        }
        return Err(anyhow!(
            "remote engine at {configured}{READINESS_PROBE} did not respond within {:?}",
            READINESS_TIMEOUT
        ));
    }

    // Preferred local engine location: the configured loopback URL
    // (default 127.0.0.1:19113). We try this first and only move off it
    // if it turns out to be occupied by something we can't reclaim.
    let preferred_url = configured.clone();
    let preferred_port = port_of(&preferred_url);

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
        let client = reqwest::Client::new();
        let port_responds = probe(&preferred_url, &client).await;

        if guard.is_some() && port_responds {
            // The port responds, but it might be a stale-version
            // engine left behind by a previous app instance (the
            // process didn't reap on crash, so we inherited it
            // on next start). Verify the version matches the
            // shell's expectation; if not, kill it and respawn
            // from the bundled bytes.
            if engine_version_matches(&preferred_url, &client).await {
                return Ok(preferred_url);
            }
            eprintln!(
                "[engine] stale-version sibling engine detected on {preferred_url} — killing and respawning",
            );
        }

        // Kill our child handle if we have one. take() empties the
        // slot so the spawn path below can overwrite cleanly. Clear the
        // running flag too, so the set_url() that picks this launch's
        // (possibly fallback) port below isn't rejected by its own
        // "don't relocate a running sidecar" guard.
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            LOCAL_CHILD_RUNNING.store(false, Ordering::SeqCst);
        }

        // Belt-and-braces: even if our child handle was empty (no
        // record of spawning) or already-killed, the port may still be
        // held by an orphan from a previous crashed launch — and the
        // 2.2.22 stdin-EOF watcher only protects against future
        // crashes, not retroactively. If we still see a listener on the
        // preferred port here, it's not ours; reap it so the bind below
        // succeeds. No-op when the port is free.
        if port_responds {
            // Re-probe to confirm it's still listening (we might have
            // just killed our own child above, freeing the port).
            if probe(&preferred_url, &client).await {
                reap_orphan_listener_on(preferred_port).await;
            }
        }
    }

    // Decide where our engine will actually live. Normally the preferred
    // port is now free (it always was, or we just reaped whatever was on
    // it). If it's STILL held — a squatter we couldn't kill (another
    // app, a permissions/AV block, or a standalone ps5upload-engine the
    // user launched by mistake) — fall back to an OS-assigned free port
    // so the desktop can ALWAYS bring up its own engine instead of being
    // wedged on launch forever. Without this, anything camping on 19113
    // permanently breaks "open the app".
    let (engine_url, engine_port) = if loopback_port_free(preferred_port) {
        (preferred_url, preferred_port)
    } else if let Some(free) = pick_free_loopback_port() {
        eprintln!(
            "[engine] preferred port {preferred_port} still occupied after reap attempt; \
             falling back to free port {free} so the app can start",
        );
        (format!("http://127.0.0.1:{free}"), free)
    } else {
        // Couldn't even secure a free port; try the preferred one anyway
        // and let the readiness path surface a clear error.
        (preferred_url, preferred_port)
    };
    // Point the whole app at wherever the engine actually landed. The
    // renderer's command proxies read engine::url() fresh on every call,
    // so this reroutes them with no renderer-side change. In-memory only
    // — next launch re-tries the preferred port first.
    set_url(engine_url.clone());

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
        // Pipe stdin so the engine's parent-watch thread can detect
        // our death by EOF on its stdin. We never write to the pipe
        // — only the OS closing the write end (which it does
        // unconditionally when the parent process exits, however it
        // exits) signals the child. Pairs with PS5UPLOAD_PARENT_WATCH
        // below, which gates the watcher inside the engine.
        .stdin(std::process::Stdio::piped())
        // PS5UPLOAD_PARENT_WATCH=1 enables the engine's stdin-EOF
        // watcher. Without it the engine ignores stdin (so a
        // standalone `cargo run -p ps5upload-engine` from a terminal
        // doesn't auto-exit when the dev pipes a file in or
        // backgrounds it).
        .env("PS5UPLOAD_PARENT_WATCH", "1")
        // Tell the engine which port to bind. Normally 19113, but may be
        // an OS-assigned fallback when 19113 was occupied — the engine
        // reads PS5UPLOAD_ENGINE_PORT and binds it (default 19113 if
        // unset, which keeps a standalone `cargo run` on the usual port).
        .env("PS5UPLOAD_ENGINE_PORT", engine_port.to_string())
        // Make engine panics include a backtrace in the captured stderr /
        // engine.log + the panic hook's ring entry (see run_cli's
        // install_panic_logger). Without this a panic is just a one-line
        // message with no call site, which is little help in a bug report.
        .env("RUST_BACKTRACE", "1")
        // kill_on_drop is the GRACEFUL teardown path (Drop on the
        // Child runs when the desktop shell exits cleanly); the
        // stdin-EOF watcher above is the BACKUP for ungraceful exits
        // (taskkill /F, segfault, panic, OOM-killer) where Drop never
        // gets a chance to run.
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
    LOCAL_CHILD_RUNNING.store(true, Ordering::SeqCst);

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
                // When the engine's stderr hits EOF the process has (almost
                // certainly) exited. Use that as the post-startup crash signal:
                // reap, then emit an event the renderer logs + flushes into the
                // bug bundle. Previously a mid-session engine death was
                // invisible — it just looked like API calls timing out.
                let app_exit = app.clone();
                let lw = log_writer.clone();
                tokio::spawn(async move {
                    pipe_tagged(err, "[engine:err] ", lw).await;
                    // Take the child out (deadlock-free: stderr EOF means it's
                    // already exiting, so wait() returns promptly) and read its
                    // status. If shutdown already took it, that's fine.
                    let child_opt = { child_lock().await.lock().await.take() };
                    let code = if let Some(mut c) = child_opt {
                        c.wait().await.ok().and_then(|s| s.code())
                    } else {
                        None
                    };
                    LOCAL_CHILD_RUNNING.store(false, Ordering::SeqCst);
                    eprintln!("[engine-watch] engine process exited (code {code:?})");
                    let _ = app_exit.emit("ps5upload-engine-exit", code);
                });
            }
        }
    }

    // Probe HTTP until ready or deadline.
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let client = reqwest::Client::new();
    while Instant::now() < deadline {
        if probe(&engine_url, &client).await {
            return Ok(engine_url.clone());
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
    // Readiness deadline reached without success. Reap the orphan
    // child explicitly here rather than leaving it in the slot for
    // the next start() to clean up via reap_orphan_listener_on. The
    // child holds open FDs (stdin/stdout/stderr pipes, the listener
    // socket if it bound) for as long as it sits in the slot; a user
    // who hits "Retry" minutes later would otherwise have two engine
    // processes contending for :19113.
    {
        let mut guard = child_lock().await.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        LOCAL_CHILD_RUNNING.store(false, Ordering::SeqCst);
    }
    Err(anyhow!(
        "engine did not become ready at {engine_url}{READINESS_PROBE} within {:?}.\n  log: {}\n  Common causes on Windows: SmartScreen / antivirus blocked the freshly-extracted .exe; a loopback firewall rule; or no loopback port was bindable at all.",
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
    LOCAL_CHILD_RUNNING.store(false, Ordering::SeqCst);

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
    // Byte buffer (not String) so multi-byte UTF-8 sequences split
    // across two reads don't get dropped. Prior implementation called
    // `std::str::from_utf8(&buf[..n])` and silently discarded the
    // entire chunk on any UTF-8 error — losing arbitrary bytes
    // whenever a non-ASCII char straddled a 4 KiB read boundary.
    let mut line: Vec<u8> = Vec::with_capacity(256);
    let emit_bytes = |bytes: &[u8]| {
        let s = String::from_utf8_lossy(bytes);
        eprintln!("{tag}{s}");
        if let Some(writer) = &log {
            if let Ok(mut f) = writer.lock() {
                let _ = writeln!(f, "{tag}{s}");
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
                emit_bytes(format!("[stream-read-error] {e}").as_bytes());
                break;
            }
        };
        for &b in &buf[..n] {
            if b == b'\n' {
                emit_bytes(&line);
                line.clear();
            } else {
                line.push(b);
            }
        }
    }
    if !line.is_empty() {
        emit_bytes(&line);
    }
}

/// Runtime-configurable engine base URL. Seeded from settings.json at
/// startup and overridden by the `engine_url_set` command.
static ENGINE_URL: OnceLock<RwLock<String>> = OnceLock::new();

fn engine_url_cell() -> &'static RwLock<String> {
    ENGINE_URL.get_or_init(|| RwLock::new(DEFAULT_ENGINE_URL.to_string()))
}

/// The base URL the renderer's command proxies should hit.
pub fn url() -> String {
    engine_url_cell()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Override the engine base URL (Settings → Engine URL). Empty/blank
/// resets to the bundled-sidecar default.
pub fn set_url(url: String) {
    let url = url.trim();
    let next = if url.is_empty() {
        DEFAULT_ENGINE_URL.to_string()
    } else {
        url.to_string()
    };
    // Guard against a settings-sync relocating a RUNNING local sidecar.
    // The renderer mirrors its preferred URL to us on hydrate (e.g. the
    // default 127.0.0.1:19113); if our sidecar fell back to a different
    // loopback port because 19113 was occupied, honoring that push would
    // point the command proxies at the wrong (empty) port. A running
    // sidecar lives where it bound and can only move on restart — so
    // ignore a *loopback* rewrite while it's up. Remote (non-loopback)
    // URLs still switch the target, and start()'s own set_url runs before
    // the child is marked running, so the fallback URL still takes hold.
    // `self::url()` — the local `url` param shadows the module fn by bare name.
    let current = self::url();
    if LOCAL_CHILD_RUNNING.load(Ordering::SeqCst) && is_loopback_url(&next) && next != current {
        eprintln!(
            "[engine] ignoring engine-URL change to {next} — a local sidecar is running on {current} (moving it needs a restart)",
        );
        return;
    }
    *engine_url_cell().write().unwrap_or_else(|e| e.into_inner()) = next;
}

/// True when `url` points at loopback — i.e. we should spawn the bundled
/// sidecar. A remote/LAN engine (false) is one we only talk to.
fn is_loopback_url(url: &str) -> bool {
    let authority = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("") // IPv6 literal
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}
