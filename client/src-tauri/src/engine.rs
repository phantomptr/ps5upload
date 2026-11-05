//! ps5upload-engine sidecar: spawn, probe, shutdown.
//!
//! This is the Rust counterpart of the previous `engine-sidecar.js`.
//! Spawns the engine binary as a child process, waits for the HTTP API
//! on 127.0.0.1:19113 to answer `/api/version`, and tears it down cleanly
//! when the app exits. The child is launched with the same PS5_ADDR
//! default the Electron sidecar used — the renderer still passes
//! `?addr=...` on each call, so this default only matters for the few
//! diagnostic endpoints that don't.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::{Child, Command};
use tokio::sync::OnceCell;
use tokio::time::{sleep, Instant};

use crate::engine_http::{engine_client_builder, error_chain};
use crate::DEFAULT_ENGINE_URL;

const READINESS_PROBE: &str = "/api/version";
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_POLL: Duration = Duration::from_millis(200);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long `stop()` gives the engine to exit on its own after its stdin
/// closes before killing it.
const SHUTDOWN_EOF_GRACE: Duration = Duration::from_secs(2);

static CHILD: OnceCell<tokio::sync::Mutex<Option<Child>>> = OnceCell::const_new();

/// True while our spawned sidecar child is (believed to be) alive. A
/// sync mirror of "the CHILD slot is occupied", set right after spawn and
/// cleared on stop/exit/teardown, so `set_url` — which is sync and can't
/// await the async CHILD lock — can cheaply tell whether a local sidecar
/// is running. Used to stop a settings-sync from relocating a running
/// sidecar off its (possibly fallback) port.
static LOCAL_CHILD_RUNNING: AtomicBool = AtomicBool::new(false);

/// Where the sidecar this shell spawned (or is about to spawn) listens.
/// Pinned before `start()` points the proxies at it, so a settings push
/// that lands between that and the spawn cannot move them; cleared with
/// `LOCAL_CHILD_RUNNING`. `set_url` lets a loopback URL through only when
/// it is this one, so Settings can come back from a remote engine to the
/// sidecar without a restart.
static SIDECAR_URL: RwLock<Option<String>> = RwLock::new(None);

/// Bumped every time a child is put in the slot, and when `stop()` takes one
/// out. The stderr watcher of an earlier child compares it before taking
/// "its" child out of the slot, so a watcher waking up after a restart cannot
/// take the newer child, and one whose child was stopped on purpose does not
/// report that as the engine exiting.
static SPAWN_GENERATION: AtomicU64 = AtomicU64::new(0);

fn set_sidecar_url(url: Option<String>) {
    *SIDECAR_URL.write().unwrap_or_else(|e| e.into_inner()) = url;
}

fn sidecar_url() -> Option<String> {
    SIDECAR_URL
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// The sidecar is gone (taken, killed or exited): clear both marks.
fn mark_sidecar_stopped() {
    LOCAL_CHILD_RUNNING.store(false, Ordering::SeqCst);
    set_sidecar_url(None);
}

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

/// The RAR helper `ps5upload-unrar`, embedded the same way (build.rs). UnRAR is
/// GPL-incompatible, so it is a separate program the engine starts over pipes,
/// never linked into the engine; it is extracted next to the engine and the engine
/// is told where (PS5UPLOAD_UNRAR_BIN).
const EMBEDDED_UNRAR: &[u8] = include_bytes!(env!("PS5UPLOAD_UNRAR_BYTES"));

/// Extracts the RAR helper into `dir` (the engine's directory) and returns its path.
/// Same stamp and locked-file fallback as the engine.
fn extract_unrar_helper(dir: &Path) -> Result<PathBuf> {
    let bin_name = if cfg!(target_os = "windows") {
        "ps5upload-unrar.exe"
    } else {
        "ps5upload-unrar"
    };
    let hex = blake3::hash(EMBEDDED_UNRAR).to_hex().to_string();
    extract_engine(dir, bin_name, EMBEDDED_UNRAR, &hex)
}

/// Where this app keeps its engine: `<app local data>/engine`.
fn engine_folder(app: &AppHandle) -> Result<PathBuf> {
    Ok(app
        .path()
        .app_local_data_dir()
        .context("resolving app_local_data_dir")?
        .join("engine"))
}

/// Extract the embedded engine binary into the app's local-data dir
/// and return the extracted path. Caches across launches via a
/// BLAKE3 hash stamp file (`<bin>.blake3`) sitting next to the
/// extracted binary.
///
/// Why hash-stamp vs the previous full byte compare:
/// - The previous logic read the entire cached `.exe` from
///   disk on every launch and compared it byte-for-byte to the
///   embedded array. On a slow disk that's tens of ms of cold-IO
///   per launch; on Windows, reading an `.exe` the OS still has
///   open elsewhere can fail with sharing violation and force a
///   spurious re-extract that itself fails on the locked file.
/// - Hash-stamp reads a 64-byte hex string instead, and the
///   comparison cost is constant regardless of binary size. The
///   embedded engine's hash (50-57 MB of it) is computed once, by
///   build.rs, so a start pages none of it in unless it extracts.
///
/// The stamp file IS the source of truth. If it's missing or the
/// content doesn't hex-match the freshly-computed embedded hash, we
/// re-extract + rewrite the stamp. A tampered binary on disk (with
/// matching size but different bytes) re-extracts on next launch.
pub(crate) fn find_engine_binary(app: &AppHandle) -> Result<PathBuf> {
    let bin_name = if cfg!(target_os = "windows") {
        "ps5upload-engine.exe"
    } else {
        "ps5upload-engine"
    };

    let out_dir = engine_folder(app)?;
    std::fs::create_dir_all(&out_dir).with_context(|| format!("mkdir {}", out_dir.display()))?;
    extract_engine(
        &out_dir,
        bin_name,
        EMBEDDED_ENGINE,
        env!("PS5UPLOAD_ENGINE_BLAKE3"),
    )
}

/// Writes `bytes` (BLAKE3 `hex`) into `dir` as `bin_name` and returns its path.
///
/// The usual name is tried first (a firewall rule the user allowed names that path). When it
/// cannot be written — on Windows an older engine still running, or an antivirus scan, holds
/// the file (#427) — the engine goes next to it under a name carrying its hash instead, so
/// the app still starts. Engines left over from older versions are removed when nothing holds
/// them.
fn extract_engine(dir: &Path, bin_name: &str, bytes: &[u8], hex: &str) -> Result<PathBuf> {
    let usual = dir.join(bin_name);
    let err = match extract_at(&usual, bytes, hex) {
        Ok(()) => {
            remove_stale_engines(dir, bin_name, None);
            return Ok(usual);
        }
        Err(e) => e,
    };
    let (stem, ext) = bin_name.split_once('.').unwrap_or((bin_name, ""));
    let dot = if ext.is_empty() { "" } else { "." };
    let versioned = dir.join(format!("{stem}-{}{dot}{ext}", &hex[..hex.len().min(16)]));
    eprintln!(
        "[engine] could not write {}: {err:#}; using {} instead",
        usual.display(),
        versioned.display()
    );
    extract_at(&versioned, bytes, hex).with_context(|| {
        format!(
            "could not write the engine to {} ({err:#}) or to {}",
            usual.display(),
            versioned.display()
        )
    })?;
    remove_stale_engines(dir, bin_name, Some(&versioned));
    Ok(versioned)
}

/// Writes the engine at `path` unless the copy there already is this one. A BLAKE3 stamp
/// (`<path>.blake3`) next to it says which engine it is; the binary must exist too (an
/// antivirus or disk cleaner may remove it and keep the stamp).
fn extract_at(path: &Path, bytes: &[u8], hex: &str) -> Result<()> {
    let stamp = PathBuf::from(format!("{}.blake3", path.display()));
    let current = path.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == hex);
    if current {
        return Ok(());
    }
    // Written beside it and renamed over it, never rewritten in place: on Apple Silicon a
    // binary overwritten in place keeps the old code signature cached for that file, and macOS
    // kills the next launch (SIGKILL) until a reboot. A rename gives it a new file.
    let tmp = PathBuf::from(format!("{}.new", path.display()));
    let written = (|| -> Result<()> {
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("write engine binary to {}", tmp.display()))?;
        // On Unix, set the executable bit — without it, `spawn` returns EACCES. Windows
        // runs .exe files by extension.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("chmod {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, path)
            .with_context(|| format!("replace engine binary at {}", path.display()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    // Stamp last so a crash mid-extract leaves a missing or outdated stamp; the next
    // launch re-extracts.
    if let Err(e) = std::fs::write(&stamp, hex) {
        eprintln!(
            "[engine] could not write stamp {}: {e} (engine still extracted, just re-extracts next launch)",
            stamp.display()
        );
    }
    Ok(())
}

/// Removes hash-named engines (and their stamps) other than `keep`. One still running stays
/// locked and is skipped; a later launch removes it.
fn remove_stale_engines(dir: &Path, bin_name: &str, keep: Option<&Path>) {
    let prefix = format!("{}-", bin_name.split_once('.').map_or(bin_name, |(s, _)| s));
    let keep_stamp = keep.map(|k| PathBuf::from(format!("{}.blake3", k.display())));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let kept = keep == Some(path.as_path()) || keep_stamp.as_ref() == Some(&path);
        if !kept && e.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// A process listening on the engine's port, as far as this OS tells.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listener {
    pid: u32,
    /// The program it runs. None when it cannot be read (another user's process, say).
    exe: Option<PathBuf>,
    /// The process that started it is still running. None when that cannot be told.
    parent_alive: Option<bool>,
}

/// Whether `listener` is an engine this app left behind, the only process the app may kill for
/// its port. Its program must be in this app's own engine folder (`engine_dir`), which rules out
/// anything else on the port: a Docker proxy, another app, an engine the user runs themselves. And
/// the process that started it must be gone, which rules out another copy of PS5Upload in use (a
/// stable and a pre-release side by side share that folder): killing that one's engine made its
/// app start the engine again, which then got in this app's way, each killing the other's until
/// one was left with none. Anything not known for certain is left alone: the app moves to a free
/// port instead.
fn is_our_orphan(listener: &Listener, engine_dir: &Path) -> bool {
    listener.parent_alive == Some(false)
        && listener
            .exe
            .as_deref()
            .is_some_and(|exe| exe.parent() == Some(engine_dir) && is_engine_file_name(exe))
}

/// `ps5upload-engine`, or the hash-named copy `extract_engine` falls back to.
fn is_engine_file_name(exe: &Path) -> bool {
    let name = exe.file_stem().and_then(|n| n.to_str()).unwrap_or("");
    name == "ps5upload-engine" || name.starts_with("ps5upload-engine-")
}

/// Runs a short command and returns what it printed; None when it failed to run in time. A cold
/// PowerShell that loads the NetTCPIP module can take several seconds, more with an antivirus
/// scanning it; a wedged shell host still can't hold up engine startup for long.
async fn command_output(program: &str, args: &[&str], port: u16) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env("PORT", port.to_string())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    // No console window for a moment: the app has none of its own.
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    match tokio::time::timeout(Duration::from_secs(8), cmd.output()).await {
        Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(Err(e)) => {
            log_line(format_args!("[engine] could not run {program}: {e}"));
            None
        }
        Err(_) => {
            log_line(format_args!("[engine] {program} timed out"));
            None
        }
    }
}

/// One line to stderr, without the panic `eprintln!` has when our stderr is a closed pipe.
fn log_line(args: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{args}");
}

/// `ps -o ppid=,comm= -p <pid>`: the parent's pid, then the program's path (which may hold spaces).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn parse_ps_line(line: &str) -> Option<(u32, PathBuf)> {
    let (ppid, exe) = line.trim().split_once(char::is_whitespace)?;
    Some((ppid.parse().ok()?, PathBuf::from(exe.trim())))
}

/// The pids in `ss -ltnp` output: `users:(("name",pid=1234,fd=9),…)`.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn parse_ss_pids(output: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = output
        .split("pid=")
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// The parent pid in `/proc/<pid>/stat`: the field after the state, which follows the program's
/// name in parentheses (a name that can itself hold spaces and parentheses).
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn parse_proc_stat_ppid(stat: &str) -> Option<u32> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}

/// One line of the PowerShell query below: `<pid>|<1 parent alive, else 0>|<program path>`.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
fn parse_windows_listener(line: &str) -> Option<Listener> {
    let mut fields = line.trim().splitn(3, '|');
    let pid = fields.next()?.trim().parse().ok()?;
    let parent_alive = match fields.next()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    };
    let exe = fields
        .next()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    Some(Listener {
        pid,
        exe,
        parent_alive,
    })
}

/// Who listens on `port`.
#[cfg(target_os = "windows")]
async fn listeners_on(port: u16) -> Vec<Listener> {
    // A parent's pid can be reused on Windows, which reads as alive: the listener is then left
    // alone, the safe way to be wrong.
    // Single quotes only: Windows command lines have no one way to escape a double quote.
    const QUERY: &str = "$ErrorActionPreference='SilentlyContinue'; \
        Get-NetTCPConnection -LocalPort $env:PORT -State Listen | \
        Select-Object -ExpandProperty OwningProcess -Unique | ForEach-Object { \
          $id = $_; \
          $p = Get-CimInstance Win32_Process -Filter ('ProcessId=' + $id); \
          $alive = if ($p -and (Get-Process -Id $p.ParentProcessId)) { 1 } else { 0 }; \
          '{0}|{1}|{2}' -f $id, $alive, $p.ExecutablePath \
        }";
    command_output(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", QUERY],
        port,
    )
    .await
    .map(|out| out.lines().filter_map(parse_windows_listener).collect())
    .unwrap_or_default()
}

#[cfg(target_os = "macos")]
async fn listeners_on(port: u16) -> Vec<Listener> {
    // Named by full path: a GUI app's PATH is not a shell's, and lsof lives in /usr/sbin.
    let Some(out) = command_output(
        "/bin/sh",
        &["-c", "/usr/sbin/lsof -nP -t -iTCP:$PORT -sTCP:LISTEN"],
        port,
    )
    .await
    else {
        return Vec::new();
    };
    let mut listeners = Vec::new();
    for pid in out.lines().filter_map(|l| l.trim().parse::<u32>().ok()) {
        let pid_arg = pid.to_string();
        let line = command_output(
            "/bin/ps",
            &["-ww", "-o", "ppid=,comm=", "-p", &pid_arg],
            port,
        )
        .await
        .unwrap_or_default();
        let parsed = parse_ps_line(&line);
        listeners.push(Listener {
            pid,
            exe: parsed.as_ref().map(|(_, exe)| exe.clone()),
            // An orphan is adopted by launchd, pid 1.
            parent_alive: parsed.map(|(ppid, _)| ppid > 1),
        });
    }
    listeners
}

#[cfg(target_os = "linux")]
async fn listeners_on(port: u16) -> Vec<Listener> {
    let Some(out) = command_output(
        "/bin/sh",
        &["-c", "ss -ltnpH \"sport = :$PORT\" 2>/dev/null"],
        port,
    )
    .await
    else {
        return Vec::new();
    };
    parse_ss_pids(&out)
        .into_iter()
        .map(|pid| {
            // Reads " (deleted)" once an update renamed a new engine over this one.
            let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .map(|p| {
                    let text = p.to_string_lossy();
                    PathBuf::from(text.strip_suffix(" (deleted)").unwrap_or(&text))
                });
            let ppid = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| parse_proc_stat_ppid(&stat));
            Listener {
                pid,
                exe,
                // An orphan is adopted by init, or by the session's subreaper (systemd --user),
                // which is no PS5Upload.
                parent_alive: ppid.map(|ppid| {
                    ppid > 1
                        && std::fs::read_link(format!("/proc/{ppid}/exe"))
                            .ok()
                            .is_none_or(|parent| !is_init_like(&parent))
                }),
            }
        })
        .collect()
}

/// A process that adopts orphans rather than starting programs of ours.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn is_init_like(exe: &Path) -> bool {
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(name, "systemd" | "init" | "launchd")
}

/// Recover from an engine an earlier launch left behind on the preferred port (see
/// `is_our_orphan`): it is stopped so this launch can have the port. The parent watch has kept
/// that from happening since 2.2.22, so this is rare. Whatever else holds the port is never
/// touched, and `start` moves to a free port. Failing to find out who listens is the same case.
async fn reap_our_orphan_on(port: u16, engine_dir: &Path) {
    // What the OS reports is the real path; the folder may have been named through a link.
    let real_dir = std::fs::canonicalize(engine_dir).unwrap_or_else(|_| engine_dir.to_path_buf());
    let mut reaped = false;
    for listener in listeners_on(port).await {
        if is_our_orphan(&listener, engine_dir) || is_our_orphan(&listener, &real_dir) {
            log_line(format_args!(
                "[engine] stopping the engine an earlier launch left on :{port}: {listener:?}"
            ));
            crate::exit_guard::kill(listener.pid);
            reaped = true;
        } else {
            log_line(format_args!(
                "[engine] leaving what listens on :{port} alone: {listener:?}"
            ));
        }
    }
    if reaped {
        // Give the OS a beat to actually release the port: kill returns before the kernel
        // finishes reaping the dead process's sockets.
        sleep(Duration::from_millis(300)).await;
    }
}

/// True if `port` can be bound right now (nothing is holding it). Binds
/// and immediately drops, so there's a tiny TOCTOU window before the engine
/// itself binds — acceptable here: the only racer on the preferred port is a
/// process we'd want to fall back away from anyway, and the engine's own bind
/// failure is surfaced by the readiness path.
///
/// Asked for both addresses the engine's port is reached on. The engine binds every interface
/// (and is reached on loopback), and on macOS a bind to one of them shares the port with a socket
/// on the other: a Docker engine listening on every interface looked like a free port there.
fn loopback_port_free(port: u16) -> bool {
    use std::net::{Ipv4Addr, TcpListener};
    // Separate statements: each listener must be gone before the next bind.
    let loopback = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    let every_interface = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok();
    loopback && every_interface
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
    probe_detail(url, client).await.is_ok()
}

/// Like `probe`, but says why it failed, so a readiness timeout can report
/// "connection refused" vs "timed out" vs a proxy error instead of nothing.
async fn probe_detail(url: &str, client: &reqwest::Client) -> Result<(), String> {
    let u = format!("{url}{READINESS_PROBE}");
    match client
        .get(&u)
        .timeout(Duration::from_millis(500))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!("HTTP {}", r.status())),
        Err(e) => Err(error_chain(&e)),
    }
}

/// The client the sidecar's own probes use. Panics only where
/// `reqwest::Client::new()` (which this replaces) would have panicked too.
fn engine_probe_client() -> reqwest::Client {
    engine_client_builder()
        .build()
        .expect("failed to build engine probe HTTP client")
}

/// Check that the engine answering the port reports the same version
/// the desktop shell was built against. Returns true on match (use
/// the existing engine), false otherwise (start this app's own).
///
/// Mismatch is what another engine on the port looks like: a previous app
/// instance's orphan, or another copy or version of the app in use. Without
/// this check, the new shell talks to that engine indefinitely — and any
/// newly-added route surfaces as 404 to the user with no obvious explanation.
///
/// Robust against the engine not implementing /api/version yet
/// (returns false, which is correct behavior).
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
/// Rotated files kept besides engine.log: `engine.log.old` (the newest), then `.old.2` and on.
/// A busy session writes 1 MiB in a few hours, so one old file lost what a report filed that
/// evening was about, while the app's own log still had it (bug_report.rs ships them all).
const ENGINE_LOG_OLD_FILES: usize = 4;

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
/// Rotates when the file exceeds `ENGINE_LOG_MAX_BYTES` — at open and
/// again whenever a line written during the session takes it past the
/// cap, so a long session cannot grow it without bound. The previous
/// file moves to `<name>.old`, older ones along to `.old.2` and on
/// (`ENGINE_LOG_OLD_FILES`).
///
/// Logs the failure to `eprintln!` if open fails, so a developer
/// running `cargo tauri dev` from a console at least sees why
/// log capture didn't engage. Returns None on failure rather than
/// erroring — a missing log file shouldn't block engine startup.
fn open_engine_log(path: &Path) -> Option<Arc<Mutex<EngineLog>>> {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len > ENGINE_LOG_MAX_BYTES {
        rotate_engine_log(path);
    }
    match EngineLog::open(path) {
        Ok(log) => Some(Arc::new(Mutex::new(log))),
        Err(e) => {
            eprintln!("[engine-log] cannot open {}: {e}", path.display());
            None
        }
    }
}

/// Best-effort rotate: if the rename fails (Windows lock on the .old
/// file from a previous instance, etc.), the caller goes on appending
/// to the existing log and lets it grow past the cap. Worst case is a
/// larger log file, which is recoverable; the alternative of failing
/// to open at all loses diagnostics entirely.
fn rotate_engine_log(path: &Path) {
    let old_file = |n: usize| {
        if n == 1 {
            path.with_extension("log.old")
        } else {
            path.with_extension(format!("log.old.{n}"))
        }
    };
    let _ = std::fs::remove_file(old_file(ENGINE_LOG_OLD_FILES));
    for n in (1..ENGINE_LOG_OLD_FILES).rev() {
        let _ = std::fs::rename(old_file(n), old_file(n + 1));
    }
    let old = old_file(1);
    if let Err(e) = std::fs::rename(path, &old) {
        eprintln!(
            "[engine-log] rotate {} -> {} failed: {e} (continuing)",
            path.display(),
            old.display()
        );
    }
}

/// The engine log file with the size it has reached, so a line that takes
/// it past the cap rotates it in place (the file is reopened afterwards).
struct EngineLog {
    path: PathBuf,
    file: std::fs::File,
    len: u64,
}

impl EngineLog {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            file,
            len,
        })
    }

    /// Append one line (a newline is added) and rotate when the cap is passed.
    fn write_line(&mut self, line: &str) {
        use std::io::Write;
        let _ = writeln!(self.file, "{line}");
        let _ = self.file.flush();
        self.len = self.len.saturating_add(line.len() as u64 + 1);
        if self.len > ENGINE_LOG_MAX_BYTES {
            // Reopen first so a failed rotation keeps appending to the same file.
            rotate_engine_log(&self.path);
            match Self::open(&self.path) {
                Ok(next) => *self = next,
                Err(e) => {
                    eprintln!(
                        "[engine-log] reopen {} after rotation failed: {e}",
                        self.path.display()
                    );
                    self.len = 0;
                }
            }
        }
    }
}

/// Start the engine child process and wait for HTTP readiness.
///
/// Idempotent: a second call is a no-op if the engine is already up.
pub async fn start(app: &AppHandle) -> Result<String> {
    // Seed the configured URL from settings.json before the spawn
    // decision — renderer hydration runs too late to gate it here. Only
    // when the renderer has not said yet: what it pushed may be newer than the
    // file, whose writes are debounced, and a restart must not undo it.
    let wanted = preferred_url().or_else(|| crate::commands::user_config::load_engine_url(app));
    if let Some(wanted) = wanted {
        apply_url(wanted);
    }
    let configured = url();

    // Remote engine: don't spawn the bundled sidecar, just probe the
    // remote API for readiness and hand back its URL.
    if !is_loopback_url(&configured) {
        eprintln!("[engine] remote engine configured ({configured}); skipping local sidecar");
        let client = engine_probe_client();
        let deadline = Instant::now() + READINESS_TIMEOUT;
        while Instant::now() < deadline {
            if probe(&configured, &client).await {
                return Ok(configured);
            }
            sleep(READINESS_POLL).await;
        }
        return Err(anyhow!(crate::commands::shell_error::coded(
            "engine_remote_unreachable",
            serde_json::json!({ "url": configured, "seconds": READINESS_TIMEOUT.as_secs() }),
        )));
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
        let client = engine_probe_client();
        let port_responds = probe(&preferred_url, &client).await;

        if port_responds {
            // The port responds, but it might be a stale-version
            // engine left behind by a previous app instance, or
            // another copy of the app's engine in use. Verify the
            // version matches the shell's expectation; if not, this
            // app runs its own from the bundled bytes. A matching engine
            // that is not our child — one the user runs themselves, or a
            // sidecar whose shell died — is used as it is rather than
            // killed: the reaper below is only for an engine we cannot use.
            if engine_version_matches(&preferred_url, &client).await {
                if guard.is_none() {
                    eprintln!(
                        "[engine] using the engine already answering on {preferred_url} (not started by this app)",
                    );
                }
                return Ok(preferred_url);
            }
            eprintln!(
                "[engine] another version's engine answers on {preferred_url}; this app runs its own",
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
            mark_sidecar_stopped();
        }

        // Even if our child handle was empty (no record of spawning) or
        // already-killed, the port may still be held by an engine a
        // previous crashed launch left behind. If we still see a listener
        // on the preferred port here, it's not our child; only that orphan
        // is stopped (`is_our_orphan`), nothing else ever.
        if port_responds {
            // Re-probe to confirm it's still listening (we might have
            // just killed our own child above, freeing the port).
            if probe(&preferred_url, &client).await {
                match engine_folder(app) {
                    Ok(dir) => reap_our_orphan_on(preferred_port, &dir).await,
                    Err(e) => eprintln!("[engine] no engine folder to compare with: {e:#}"),
                }
            }
        }
    }

    // Decide where our engine will actually live. Normally the preferred
    // port is now free (it always was, or the orphan on it just went). If
    // it's STILL held — another app, another copy of PS5Upload in use, a
    // Docker engine, or a standalone ps5upload-engine the user launched —
    // fall back to an OS-assigned free port so the desktop can ALWAYS bring
    // up its own engine instead of being wedged on launch forever. Without
    // this, anything camping on 19113 permanently breaks "open the app".
    let (engine_url, engine_port) = if loopback_port_free(preferred_port) {
        (preferred_url, preferred_port)
    } else if let Some(free) = pick_free_loopback_port() {
        eprintln!(
            "[engine] preferred port {preferred_port} is taken; \
             using free port {free} so the app can start",
        );
        (format!("http://127.0.0.1:{free}"), free)
    } else {
        // Couldn't even secure a free port; try the preferred one anyway
        // and let the readiness path surface a clear error.
        (preferred_url, preferred_port)
    };
    let binary = find_engine_binary(app).context("locating ps5upload-engine binary")?;
    // Without the helper the engine still runs; only RAR archives say "not installed".
    let unrar_helper = binary
        .parent()
        .and_then(|dir| match extract_unrar_helper(dir) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("[engine] could not extract the RAR helper: {e:#}");
                None
            }
        });
    // Point the whole app at wherever the engine actually landed. The
    // renderer's command proxies read engine::url() fresh on every call,
    // so this reroutes them with no renderer-side change. In-memory only
    // — next launch re-tries the preferred port first. Pinned first, so
    // a settings push arriving before the child is marked running cannot
    // move the proxies back to the preferred port.
    set_sidecar_url(Some(engine_url.clone()));
    apply_url(engine_url.clone());
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
            f.write_line(&format!(
                "\n=== engine start @ {} (binary: {}) ===",
                chrono_like_now(),
                binary.display()
            ));
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

    if let Some(p) = &unrar_helper {
        cmd.env("PS5UPLOAD_UNRAR_BIN", p);
    }

    // The Payload Library's folder: the engine's /api/payloads and this app's Payloads screen
    // share one cache and one repo list.
    if let Ok(dir) = crate::commands::payloads::library_dir(app) {
        cmd.env(ps5upload_core::payload_library::PAYLOADS_DIR_ENV, dir);
    }

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

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            set_sidecar_url(None);
            return Err(e).with_context(|| format!("spawning engine: {}", binary.display()));
        }
    };

    // Store the child immediately so shutdown can reach it even if
    // readiness never arrives.
    let generation;
    {
        let mut guard = child_lock().await.lock().await;
        if let Some(pid) = child.id() {
            crate::exit_guard::register("engine", pid);
        }
        *guard = Some(child);
        generation = SPAWN_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        LOCAL_CHILD_RUNNING.store(true, Ordering::SeqCst);
    }

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
                    // status. If shutdown already took it, that's fine. After a
                    // restart the slot holds a newer child: that one is left
                    // alone, and its exit is not this child's.
                    let child_opt = {
                        let mut guard = child_lock().await.lock().await;
                        if SPAWN_GENERATION.load(Ordering::SeqCst) != generation {
                            return;
                        }
                        guard.take()
                    };
                    // Ours to reap: it stopped on its own (a stop or a restart takes it first).
                    let unexpected = child_opt.is_some();
                    let code = if let Some(mut c) = child_opt {
                        let pid = c.id();
                        let code = c.wait().await.ok().and_then(|s| s.code());
                        if let Some(pid) = pid {
                            crate::exit_guard::unregister(pid);
                        }
                        code
                    } else {
                        None
                    };
                    mark_sidecar_stopped();
                    eprintln!("[engine-watch] engine process exited (code {code:?})");
                    let _ = app_exit.emit("ps5upload-engine-exit", code);
                    if unexpected && !crate::exit_guard::exiting() {
                        restart_after_crash(app_exit);
                    }
                });
            }
        }
    }

    // Probe HTTP until ready or deadline.
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let client = engine_probe_client();
    let mut last_probe_error = String::from("no probe completed");
    while Instant::now() < deadline {
        match probe_detail(&engine_url, &client).await {
            Ok(()) => return Ok(engine_url.clone()),
            Err(e) => last_probe_error = e,
        }
        // Detect early exit so we surface crashes rather than waiting out
        // the full 30s deadline.
        {
            let mut guard = child_lock().await.lock().await;
            if let Some(child) = guard.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(anyhow!(crate::commands::shell_error::coded(
                        "engine_exited_at_start",
                        serde_json::json!({
                            "detail": status.to_string(),
                            "log": log_path.display().to_string(),
                        }),
                    )));
                }
            } else if !LOCAL_CHILD_RUNNING.load(Ordering::SeqCst) {
                // The stderr watcher saw it exit first and took it from the slot: still a
                // crash, not a slow start to wait out.
                return Err(anyhow!(crate::commands::shell_error::coded(
                    "engine_exited_at_start",
                    serde_json::json!({ "log": log_path.display().to_string() }),
                )));
            }
        }
        sleep(READINESS_POLL).await;
    }
    // Readiness deadline reached without success. Reap the orphan
    // child explicitly here rather than leaving it in the slot for
    // the next start() to clean up. The
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
        mark_sidecar_stopped();
    }
    Err(anyhow!(not_ready_message(
        &engine_url,
        &last_probe_error,
        &log_path
    )))
}

/// The renderer's words for a readiness timeout (`lib/shellError.ts`): the likely causes on this
/// platform (`hint`; the Windows list used to be shown everywhere, which told a Mac user about
/// SmartScreen) and the facts for a bug report.
fn not_ready_message(url: &str, last_error: &str, log_path: &Path) -> String {
    let hint = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    };
    crate::commands::shell_error::coded(
        "engine_not_ready",
        serde_json::json!({
            "seconds": READINESS_TIMEOUT.as_secs(),
            "url": url,
            "error": last_error,
            "log": log_path.display().to_string(),
            "hint": hint,
        }),
    )
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

/// Tear down the engine child: close its stdin so its parent watch shuts
/// it down in an orderly way, then kill it if it is still there after
/// `SHUTDOWN_EOF_GRACE`. Idempotent — safe to call twice, safe to call
/// when nothing is running.
pub async fn stop() {
    let lock = child_lock().await;
    let mut guard = lock.lock().await;
    let Some(mut child) = guard.take() else {
        return;
    };
    // Its stderr watcher sees the EOF this causes: a stop asked for is not an
    // engine exit, so retire its generation (the watcher then returns quietly).
    SPAWN_GENERATION.fetch_add(1, Ordering::SeqCst);
    mark_sidecar_stopped();

    // The engine's parent watch (PS5UPLOAD_PARENT_WATCH) exits on stdin
    // EOF, the same signal it gets when this process dies, so dropping
    // our end lets it finish in-flight work on its own terms. Child::kill()
    // is SIGKILL on unix and TerminateProcess on Windows: the backstop, and
    // the whole stop is time-boxed in case the child ignores both.
    drop(child.stdin.take());
    let pid = child.id();
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        if tokio::time::timeout(SHUTDOWN_EOF_GRACE, child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    })
    .await;
    if let Some(pid) = pid {
        crate::exit_guard::unregister(pid);
    }
}

/// What the shell can see about the engine when the app cannot reach it. Every field is
/// measured; nothing here is a guess at the cause (the renderer words the cause from these).
#[derive(Debug, serde::Serialize)]
pub struct Diagnosis {
    /// Where the command proxies send engine requests.
    pub url: String,
    /// The engine is this app's own sidecar on this machine (not a remote engine).
    pub local: bool,
    /// It answered its readiness probe just now.
    pub answering: bool,
    /// Why the probe failed, with every underlying cause.
    pub probe_error: Option<String>,
    /// The shell still holds a live sidecar process. `None` for a remote engine.
    pub child_running: Option<bool>,
    /// Something is listening on the engine's port. `None` for a remote engine.
    pub port_taken: Option<bool>,
    /// The bundled engine program is on disk where the app expects it.
    pub binary_found: Option<bool>,
    pub binary_error: Option<String>,
    pub log_path: Option<String>,
    pub os: &'static str,
}

pub async fn diagnose(app: &AppHandle) -> Diagnosis {
    let url = url();
    let local = is_loopback_url(&url);
    let probe = probe_detail(&url, &engine_probe_client()).await;
    let (child_running, port_taken, binary_found, binary_error, log_path) = if local {
        let child_running = {
            let mut guard = child_lock().await.lock().await;
            match guard.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => false,
            }
        };
        let binary = find_engine_binary(app);
        let log_path = binary
            .as_ref()
            .ok()
            .and_then(|b| b.parent())
            .map(|d| d.join("engine.log").display().to_string());
        (
            Some(child_running),
            Some(!loopback_port_free(port_of(&url))),
            Some(binary.is_ok()),
            binary.err().map(|e| format!("{e:#}")),
            log_path,
        )
    } else {
        (None, None, None, None, None)
    };
    Diagnosis {
        url,
        local,
        answering: probe.is_ok(),
        probe_error: probe.err(),
        child_running,
        port_taken,
        binary_found,
        binary_error,
        log_path,
        os: std::env::consts::OS,
    }
}

/// When the engine last restarted itself after a crash (ms since the epoch).
static LAST_AUTO_RESTART_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// At most one restart of its own per this long: an engine that keeps dying is left stopped,
/// with the banner's Restart button and its reason, rather than spun.
const AUTO_RESTART_COOLDOWN_MS: u64 = 120_000;

/// The engine stopped on its own while the app runs: start it again once, after a moment, and
/// tell the renderer (the same ready event as the Restart button).
fn restart_after_crash(app: AppHandle) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_AUTO_RESTART_MS.load(Ordering::SeqCst);
    if last != 0 && now.saturating_sub(last) < AUTO_RESTART_COOLDOWN_MS {
        eprintln!("[engine-watch] engine stopped again soon after a restart; leaving it stopped");
        return;
    }
    LAST_AUTO_RESTART_MS.store(now, Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        sleep(Duration::from_millis(1500)).await;
        if crate::exit_guard::exiting() {
            return;
        }
        eprintln!("[engine-watch] starting the engine again");
        // Boxed: start() spawned the task this runs in.
        let started: std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> =
            Box::pin(start(&app));
        match started.await {
            Ok(url) => {
                let _ = app.emit("ps5upload-engine-ready", &url);
            }
            Err(e) => {
                eprintln!("[engine-watch] restart failed: {e:#}");
                let _ = app.emit("ps5upload-engine-startup-error", format!("{e:#}"));
            }
        }
    });
}

/// Stop the sidecar (if any) and start it again. For a remote engine this only re-probes it.
pub async fn restart(app: &AppHandle) -> Result<String> {
    if is_loopback_url(&url()) {
        stop().await;
    }
    start(app).await
}

/// Read lines from a child's stdio, forward to our stderr with a tag,
/// AND tee to the engine log file (when one was opened — failure to
/// open is silent, the stderr path keeps working).
///
/// Per-line write so log rotation later can split on newlines without
/// risking torn lines mid-write. The Mutex is std (not tokio) because
/// the critical section is a sync `write_all` + flush — no awaits.
/// True when a line the engine emitted already states its own level, e.g.
/// `[engine:debug] ...`. Such a line must not be re-tagged by the pipe it
/// happened to arrive on — the engine sends every level to stderr, so the
/// pipe says nothing about severity.
fn line_is_pre_tagged(line: &str) -> bool {
    line.trim_start().starts_with("[engine")
}

async fn pipe_tagged<R>(mut reader: R, tag: &'static str, log: Option<Arc<Mutex<EngineLog>>>)
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
        // The engine writes ALL of its own logs to stderr, each already
        // carrying its real level (`[engine:debug]`, `[engine:info]`, ...).
        // Blanket-prefixing the stderr pipe with `[engine:err] ` therefore
        // relabelled every successful debug line as an error: a healthy
        // session reads as a wall of `[engine:err] ... -> 200 (0ms)`, which
        // is alarming and hides the failures that matter. A line that
        // already states its level is passed through untouched; only
        // genuinely untagged output (a panic, a linker message) still gets
        // the pipe's tag, which is exactly the case the tag is for.
        let line = if line_is_pre_tagged(&s) {
            s.into_owned()
        } else {
            format!("{tag}{s}")
        };
        // Not eprintln!: that panics when our own stderr is a closed pipe,
        // which would end this drain and leave the engine blocked on a
        // full pipe (see the parent-watch history in the engine).
        let _ = writeln!(std::io::stderr(), "{line}");
        if let Some(writer) = &log {
            if let Ok(mut f) = writer.lock() {
                f.write_line(&line);
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

/// What the renderer last asked for (Settings → Engine URL), as it asked: where the engine goes
/// on its next start. None until it has said, when settings.json is what counts.
static PREFERRED_URL: RwLock<Option<String>> = RwLock::new(None);

fn preferred_url() -> Option<String> {
    PREFERRED_URL
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Empty/blank means the bundled-sidecar default.
fn normalize_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        DEFAULT_ENGINE_URL.to_string()
    } else {
        url.to_string()
    }
}

/// Override the engine base URL (Settings → Engine URL). Returns where the command proxies go
/// now, which is not what was asked while the local sidecar runs elsewhere (`apply_url`): the
/// renderer follows it and says so, and the next start of the engine honours the request.
pub fn set_url(url: String) -> String {
    let next = normalize_url(&url);
    *PREFERRED_URL.write().unwrap_or_else(|e| e.into_inner()) = Some(next.clone());
    apply_url(next);
    self::url()
}

/// Point the command proxies at `next`, unless the local sidecar runs elsewhere.
fn apply_url(next: String) {
    // Guard against a settings-sync relocating a RUNNING local sidecar.
    // The renderer mirrors its preferred URL to us on hydrate (e.g. the
    // default 127.0.0.1:19113); if our sidecar fell back to a different
    // loopback port because 19113 was occupied, honoring that push would
    // point the command proxies at the wrong (empty) port. A running
    // sidecar lives where it bound and can only move on restart — so
    // ignore a *loopback* rewrite to anywhere but the sidecar itself
    // while it's up. Remote (non-loopback) URLs still switch the target,
    // and the sidecar's own URL is always accepted, so Settings can come
    // back from a remote engine to the sidecar. start() pins the sidecar
    // URL before its own apply_url, so the fallback URL takes hold.
    if let Some(sidecar) = sidecar_url() {
        if is_loopback_url(&next) && next != sidecar {
            return;
        }
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

#[cfg(test)]
mod log_tag_tests {
    use super::line_is_pre_tagged;

    #[test]
    fn engine_own_levels_are_not_relabelled_as_errors() {
        // Straight from a user report: a healthy session showed hundreds of
        // `[engine:err] [engine:debug] ... -> 200 (0ms)` lines. Every one was
        // a success logged at debug, mislabelled because it arrived on stderr.
        for line in [
            "[engine:debug] ts=1788748874711 GET /api/engine-logs -> 200 (0ms)",
            "[engine:info] fs_delete ok: /data/ps5upload in 359 ms",
            "[engine:warn] GET /api/ps5/status -> 502 (2576ms)",
            "  [engine:debug] leading whitespace still counts",
        ] {
            assert!(line_is_pre_tagged(line), "must pass through: {line}");
        }
    }

    #[test]
    fn untagged_output_still_gets_the_pipe_tag() {
        // Panics and linker noise carry no level of their own, which is the
        // case the pipe tag exists for.
        for line in [
            "thread 'main' panicked at src/lib.rs:1:1",
            "",
            "Segmentation fault",
        ] {
            assert!(!line_is_pre_tagged(line), "must be tagged: {line}");
        }
    }
}

#[cfg(test)]
mod engine_log_tests {
    use super::{EngineLog, ENGINE_LOG_MAX_BYTES};

    #[test]
    fn a_long_session_rotates_the_log_at_the_cap() {
        let dir = std::env::temp_dir().join(format!("ps5upload-englog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("engine.log");
        let mut log = EngineLog::open(&path).unwrap();
        let line = "x".repeat(1023);
        let per_file = (ENGINE_LOG_MAX_BYTES / 1024) as usize;
        for _ in 0..per_file {
            log.write_line(&line);
        }
        // Exactly at the cap: nothing rotated yet.
        assert!(!path.with_extension("log.old").exists());
        log.write_line("one more");
        let old = path.with_extension("log.old");
        assert_eq!(
            std::fs::metadata(&old).unwrap().len(),
            ENGINE_LOG_MAX_BYTES + 9
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        log.write_line("after");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod engine_log_generation_tests {
    use super::{rotate_engine_log, ENGINE_LOG_OLD_FILES};

    #[test]
    fn rotation_keeps_the_older_files_and_drops_only_the_oldest() {
        let dir = std::env::temp_dir().join(format!("ps5upload-englog-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("engine.log");
        for round in 0..ENGINE_LOG_OLD_FILES + 2 {
            std::fs::write(&path, format!("round {round}")).unwrap();
            rotate_engine_log(&path);
        }
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        let last = ENGINE_LOG_OLD_FILES + 1;
        assert_eq!(read("engine.log.old"), format!("round {last}"));
        assert_eq!(read("engine.log.old.2"), format!("round {}", last - 1));
        assert_eq!(
            read(&format!("engine.log.old.{ENGINE_LOG_OLD_FILES}")),
            format!("round {}", last + 1 - ENGINE_LOG_OLD_FILES)
        );
        assert!(!dir
            .join(format!("engine.log.old.{}", ENGINE_LOG_OLD_FILES + 1))
            .exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod reaper_tests {
    use super::*;

    const DIR: &str = "/home/me/.local/share/PS5Upload/engine";

    fn listener(exe: Option<&str>, parent_alive: Option<bool>) -> Listener {
        Listener {
            pid: 42,
            exe: exe.map(PathBuf::from),
            parent_alive,
        }
    }

    #[test]
    fn only_an_engine_of_ours_with_no_parent_left_may_be_stopped() {
        let dir = Path::new(DIR);
        let engine = format!("{DIR}/ps5upload-engine");
        let hashed = format!("{DIR}/ps5upload-engine-aaaa1111bbbb2222");
        assert!(is_our_orphan(&listener(Some(&engine), Some(false)), dir));
        assert!(is_our_orphan(&listener(Some(&hashed), Some(false)), dir));
    }

    #[test]
    fn another_copy_of_the_app_in_use_keeps_its_engine() {
        // F4: a stable and a pre-release side by side share the engine folder. Stopping the other
        // one's engine made them take turns killing each other's.
        let engine = format!("{DIR}/ps5upload-engine");
        assert!(!is_our_orphan(
            &listener(Some(&engine), Some(true)),
            Path::new(DIR)
        ));
    }

    #[test]
    fn nothing_else_on_the_port_is_ever_stopped() {
        let dir = Path::new(DIR);
        // A Docker proxy, a browser, an engine the user runs themselves (even one with no
        // parent left, a service): all outside this app's folder.
        for exe in [
            "/usr/bin/docker-proxy",
            "/Applications/Docker.app/Contents/MacOS/com.docker.backend",
            "/usr/local/bin/ps5upload-engine",
            "/home/me/ps5upload/engine/target/release/ps5upload-engine",
        ] {
            assert!(
                !is_our_orphan(&listener(Some(exe), Some(false)), dir),
                "{exe}"
            );
        }
        // In our folder but not an engine: the RAR helper.
        let unrar = format!("{DIR}/ps5upload-unrar");
        assert!(!is_our_orphan(&listener(Some(&unrar), Some(false)), dir));
    }

    #[test]
    fn what_cannot_be_told_is_left_alone() {
        let dir = Path::new(DIR);
        let engine = format!("{DIR}/ps5upload-engine");
        assert!(!is_our_orphan(&listener(None, Some(false)), dir));
        assert!(!is_our_orphan(&listener(Some(&engine), None), dir));
        assert!(!is_our_orphan(&listener(None, None), dir));
    }

    #[test]
    fn windows_engine_names_count_too() {
        let dir = Path::new(r"C:\Users\me\AppData\Local\PS5Upload\engine");
        let exe = dir.join("ps5upload-engine.exe");
        let l = Listener {
            pid: 42,
            exe: Some(exe),
            parent_alive: Some(false),
        };
        assert!(is_engine_file_name(l.exe.as_deref().unwrap()));
    }

    #[test]
    fn who_listens_is_read_from_each_tool() {
        assert_eq!(
            parse_ss_pids(
                "LISTEN 0 1024 127.0.0.1:19113 0.0.0.0:* users:((\"ps5upload-engin\",pid=1234,fd=9),(\"x\",pid=1234,fd=10),(\"y\",pid=77,fd=3))\n"
            ),
            vec![77, 1234]
        );
        assert!(parse_ss_pids("").is_empty());
        assert_eq!(
            parse_ps_line(
                "    1 /Users/me/Library/Application Support/PS5Upload/engine/ps5upload-engine\n"
            ),
            Some((
                1,
                PathBuf::from(
                    "/Users/me/Library/Application Support/PS5Upload/engine/ps5upload-engine"
                )
            ))
        );
        assert_eq!(parse_ps_line(""), None);
        // A program name can hold spaces and parentheses.
        assert_eq!(
            parse_proc_stat_ppid("1234 (ps5upload (x) y) S 987 1234 1234 0 -1"),
            Some(987)
        );
        assert_eq!(parse_proc_stat_ppid("garbage"), None);
        assert_eq!(
            parse_windows_listener("1234|0|C:\\Apps\\ps5upload-engine.exe\r"),
            Some(Listener {
                pid: 1234,
                exe: Some(PathBuf::from("C:\\Apps\\ps5upload-engine.exe")),
                parent_alive: Some(false),
            })
        );
        // Another user's process: no path to read.
        assert_eq!(
            parse_windows_listener("88|1|"),
            Some(Listener {
                pid: 88,
                exe: None,
                parent_alive: Some(true),
            })
        );
        assert_eq!(parse_windows_listener("not a line"), None);
        assert!(is_init_like(Path::new("/usr/lib/systemd/systemd")));
        assert!(!is_init_like(Path::new("/opt/PS5Upload/ps5upload")));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn a_listener_that_is_not_ours_survives_the_reaper() {
        // This test process listens on the port: wrongly killing what holds it would end the run.
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let dir = std::env::temp_dir().join("ps5upload-no-such-engine-folder");
        reap_our_orphan_on(port, &dir).await;
        // Still bound, and the port still counts as taken: `start` moves to a free one.
        assert!(!loopback_port_free(port));
        drop(held);
    }

    #[test]
    fn a_port_held_on_any_address_the_engine_uses_counts_as_taken() {
        // A Docker engine listens on every interface: the app must move, not fail to bind.
        let every = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = every.local_addr().unwrap().port();
        assert!(!loopback_port_free(port));
        drop(every);
        let loopback = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = loopback.local_addr().unwrap().port();
        assert!(!loopback_port_free(port));
        drop(loopback);
        assert!(loopback_port_free(port), "both released");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn this_process_is_seen_listening_on_its_port() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let found = listeners_on(port).await;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].pid, std::process::id());
        assert_eq!(found[0].parent_alive, Some(true));
        assert!(found[0].exe.is_some());
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    /// The one test that touches the process-wide URL state, so nothing races it.
    #[test]
    fn a_running_sidecar_stays_put_and_says_where_while_the_request_is_kept() {
        set_sidecar_url(Some("http://127.0.0.1:51234".into()));
        apply_url("http://127.0.0.1:51234".into());
        // Another loopback URL: the proxies stay, and the caller is told so.
        assert_eq!(
            set_url("http://127.0.0.1:19113".into()),
            "http://127.0.0.1:51234"
        );
        // F20: kept for the engine's next start, which reads it before the settings file.
        assert_eq!(preferred_url().as_deref(), Some("http://127.0.0.1:19113"));
        // A remote engine switches at once.
        assert_eq!(
            set_url("http://192.168.1.20:19113".into()),
            "http://192.168.1.20:19113"
        );
        // Back to the sidecar's own URL.
        assert_eq!(
            set_url("http://127.0.0.1:51234".into()),
            "http://127.0.0.1:51234"
        );
        // Stopped: the next loopback URL holds.
        mark_sidecar_stopped();
        assert_eq!(set_url("  ".into()), DEFAULT_ENGINE_URL);
        assert_eq!(preferred_url().as_deref(), Some(DEFAULT_ENGINE_URL));
    }
}

#[cfg(test)]
mod probe_error_tests {
    use super::{engine_probe_client, probe_detail};

    #[tokio::test]
    async fn a_failed_probe_says_why_not_just_that_it_failed() {
        // From a user report: every engine request failed with only
        // "error sending request for url (…)", which cannot tell a refused
        // connection from a timeout or a proxy failure. The cause must
        // survive into the message.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // The listener is dropped, so nothing answers on this port.
        let err = probe_detail(&format!("http://127.0.0.1:{port}"), &engine_probe_client())
            .await
            .unwrap_err();
        assert!(err.contains("error sending request"), "{err}");
        // The underlying cause must be there. Which one depends on the OS:
        // macOS and Linux refuse a closed port at once; Windows retries the
        // connect, so the 500 ms probe times out first.
        let lower = err.to_lowercase();
        assert!(
            lower.contains("refused") || lower.contains("timed out"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod extract_tests {
    use super::extract_engine;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ps5upload-extract-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn extracts_to_the_usual_name() {
        let dir = scratch("usual");
        let p =
            extract_engine(&dir, "ps5upload-engine", b"new engine", "aaaa1111bbbb2222").unwrap();
        assert_eq!(p, dir.join("ps5upload-engine"));
        assert_eq!(std::fs::read(&p).unwrap(), b"new engine");
        // A second launch reuses it.
        assert_eq!(
            extract_engine(&dir, "ps5upload-engine", b"new engine", "aaaa1111bbbb2222").unwrap(),
            p
        );
    }

    #[test]
    fn a_locked_old_engine_does_not_stop_the_new_one() {
        // #427: on Windows an older engine still running (or an antivirus scan) holds
        // ps5upload-engine.exe, so writing the new one over it fails and the app never starts.
        // A path that cannot be written stands in for the locked file.
        let dir = scratch("locked");
        std::fs::create_dir_all(dir.join("ps5upload-engine")).unwrap();
        let p =
            extract_engine(&dir, "ps5upload-engine", b"new engine", "aaaa1111bbbb2222").unwrap();
        assert_ne!(p, dir.join("ps5upload-engine"));
        assert_eq!(std::fs::read(&p).unwrap(), b"new engine");
    }
}
