//! Keep-OS-awake inhibitor.
//!
//! Two independent owners ask the OS to skip idle sleep + display sleep:
//!   • the manual Settings toggle (reason `"manual"`), and
//!   • an automatic hold while an upload/install queue is running
//!     (reason `"transfer"`), so a long transfer never dies to the
//!     machine idle-sleeping mid-stream — the originally-reported bug.
//! Holds are reference-counted by name (see `Inhibitor`): the single
//! underlying OS primitive stays engaged while ANY reason is held, so
//! ending a transfer can't release a hold the user set in Settings, and
//! vice versa. Each platform uses its native primitive:
//!
//!   macOS:   `caffeinate -disu` subprocess (kill to release)
//!   Linux:   `systemd-inhibit --what=idle:sleep … sleep infinity`
//!            subprocess (kill to release)
//!   Windows: `SetThreadExecutionState` with ES_CONTINUOUS |
//!            ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED (call again
//!            with just ES_CONTINUOUS to release)
//!
//! The Windows path is in-process, not a subprocess, so the holder
//! distinguishes between "process" and "exec-state" variants. On
//! unsupported platforms (BSDs, etc.) we return `supported: false`
//! and let the UI disable the toggle.

use std::collections::HashSet;
use std::sync::OnceLock;

use tokio::sync::Mutex;

/// Reason name owned by the manual Settings toggle.
const MANUAL_REASON: &str = "manual";

/// One live OS handle — one of the platform-specific variants — or
/// `None` when nothing is held.
enum Handle {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Process(tokio::process::Child),
    #[cfg(target_os = "windows")]
    WinExecState,
}

/// Process-wide inhibitor with reference-counted reasons. The OS handle
/// is acquired when `reasons` goes empty→non-empty and released when it
/// goes non-empty→empty, so the manual toggle and the automatic
/// transfer hold never clobber each other. Wrapped in a Mutex because
/// the Tauri command handlers are async and can be called concurrently
/// from the renderer (rapid toggle clicks) and from the queue runners.
struct Inhibitor {
    handle: Option<Handle>,
    reasons: HashSet<String>,
}

static HOLDER: OnceLock<Mutex<Inhibitor>> = OnceLock::new();

fn holder() -> &'static Mutex<Inhibitor> {
    HOLDER.get_or_init(|| {
        Mutex::new(Inhibitor {
            handle: None,
            reasons: HashSet::new(),
        })
    })
}

/// Result of an acquire attempt, shaped for the JSON the renderer reads.
enum AcquireOutcome {
    /// The OS inhibitor is held; `reason` is now in the active set.
    Held,
    /// This platform has no inhibitor primitive (BSD, non-systemd Linux).
    Unsupported,
    /// The spawn/syscall failed (e.g. Windows GPO); message for the UI.
    Failed(String),
}

/// Add `reason` to the active set, acquiring the OS inhibitor if this is
/// the first reason. Idempotent per name. The lock is held across the
/// (synchronous, fast) spawn so two concurrent acquires can't both spawn
/// a child — but never across the async release (see `release_reason`).
async fn acquire_reason(reason: &str) -> AcquireOutcome {
    let mut g = holder().lock().await;
    if g.handle.is_some() {
        g.reasons.insert(reason.to_string());
        return AcquireOutcome::Held;
    }
    match acquire_inhibitor() {
        Ok(Some(handle)) => {
            g.handle = Some(handle);
            g.reasons.insert(reason.to_string());
            AcquireOutcome::Held
        }
        // Nothing to hold on this platform — don't record the reason, so
        // a later release is a clean no-op and `active` stays false.
        Ok(None) => AcquireOutcome::Unsupported,
        Err(e) => AcquireOutcome::Failed(e),
    }
}

/// Remove `reason`; release the OS inhibitor once the last reason goes.
/// Takes the handle out under the lock, then awaits the (async) kill
/// after dropping the guard so the mutex is never held across an await.
async fn release_reason(reason: &str) {
    let to_release = {
        let mut g = holder().lock().await;
        g.reasons.remove(reason);
        if g.reasons.is_empty() {
            g.handle.take()
        } else {
            None
        }
    };
    if let Some(handle) = to_release {
        release_inhibitor(handle).await;
    }
}

/// True when this platform has a working keep-awake primitive
/// *and* any required runtime component is present. macOS and
/// Windows always support it (caffeinate ships with macOS,
/// SetThreadExecutionState is a Win32 API). Linux needs
/// systemd-inhibit — absent on non-systemd distros, where we
/// report `supported: false` so the UI greys the toggle instead
/// of letting it bounce on every click.
fn platform_supported() -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/usr/bin/systemd-inhibit").exists()
            || std::path::Path::new("/bin/systemd-inhibit").exists()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// Manual Keep-Awake toggle (the Settings checkbox). Owns the `"manual"`
/// reason; independent of the automatic transfer hold.
#[tauri::command]
pub async fn keep_awake_set(enabled: bool) -> serde_json::Value {
    if enabled {
        match acquire_reason(MANUAL_REASON).await {
            AcquireOutcome::Held => serde_json::json!({ "enabled": true, "supported": true }),
            AcquireOutcome::Unsupported => serde_json::json!({
                "enabled": false,
                "supported": false,
                "error": "keep-awake not supported on this platform",
            }),
            AcquireOutcome::Failed(e) => serde_json::json!({
                "enabled": false,
                "supported": true,
                "error": e,
            }),
        }
    } else {
        release_reason(MANUAL_REASON).await;
        serde_json::json!({ "enabled": false, "supported": platform_supported() })
    }
}

#[tauri::command]
pub async fn keep_awake_state() -> serde_json::Value {
    let g = holder().lock().await;
    serde_json::json!({
        // `enabled` mirrors the MANUAL toggle only — the renderer's
        // checkbox reflects the user's explicit choice, not a transient
        // transfer hold (which can flip on/off under it without warning).
        "enabled": g.reasons.contains(MANUAL_REASON),
        "supported": platform_supported(),
        // Whether the OS inhibitor is actually engaged right now (manual
        // OR an active transfer). Informational; the toggle uses `enabled`.
        "active": g.handle.is_some(),
    })
}

/// Programmatic hold for an active transfer (the upload/install queue
/// runners). Best-effort: callers ignore the result — a transfer must
/// never fail because the OS declined to inhibit sleep. Distinct
/// `reason` strings from different subsystems coexist; the OS inhibitor
/// only drops when the last reason is released.
#[tauri::command]
pub async fn keep_awake_acquire(reason: String) -> serde_json::Value {
    match acquire_reason(&reason).await {
        AcquireOutcome::Held => serde_json::json!({ "active": true, "supported": true }),
        AcquireOutcome::Unsupported => serde_json::json!({ "active": false, "supported": false }),
        AcquireOutcome::Failed(e) => {
            serde_json::json!({ "active": false, "supported": true, "error": e })
        }
    }
}

/// Release a programmatic hold acquired via `keep_awake_acquire`.
#[tauri::command]
pub async fn keep_awake_release(reason: String) -> serde_json::Value {
    release_reason(&reason).await;
    serde_json::json!({ "ok": true })
}

/// Release the inhibitor at app exit regardless of how many reasons are
/// still held — the process is going away. `HOLDER` is a `static`, so its
/// contents are never dropped during process teardown; without this an
/// enabled `caffeinate` / `systemd-inhibit` child outlives the app and
/// the machine can't idle-sleep until the user reboots or kills it by
/// hand. Call from the `RunEvent::Exit` handler, alongside `engine::stop()`.
/// (Windows is a no-op: its exec-state flags clear automatically when the
/// process exits.)
pub async fn keep_awake_release_on_exit() {
    let to_release = {
        let mut g = holder().lock().await;
        g.reasons.clear();
        g.handle.take()
    };
    if let Some(handle) = to_release {
        release_inhibitor(handle).await;
    }
}

async fn release_inhibitor(handle: Handle) {
    match handle {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Handle::Process(mut child) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        #[cfg(target_os = "windows")]
        Handle::WinExecState => {
            // ES_CONTINUOUS alone clears all previously-set inhibitor
            // flags while preserving the continuous-application mode
            // semantics. `SetThreadExecutionState` is a cheap syscall;
            // we don't check the return value because the only non-
            // zero return is "success" and any other value means the
            // system lost the previous state (already cleared), which
            // is also fine.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

#[cfg(target_os = "macos")]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    use tokio::process::Command;
    // -d keep display awake, -i keep system awake, -s prevent system sleep,
    // -u assert user activity. Combined flags cover both idle sleep and
    // display sleep, matching v1.5.4's 'prevent-display-sleep' blocker.
    let child = Command::new("caffeinate")
        .args(["-disu"])
        // Kill the child if its Handle is ever dropped — belt to the
        // exit-handler's suspenders (the HOLDER static itself isn't
        // dropped at exit, so keep_awake_release_on_exit covers that path).
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn caffeinate: {e}"))?;
    Ok(Some(Handle::Process(child)))
}

#[cfg(target_os = "linux")]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    use tokio::process::Command;
    // Presence-check systemd-inhibit before spawning. Without this,
    // non-systemd distros (Alpine, Void, Gentoo OpenRC, NixOS without
    // systemd) get a misleading UI: `supported: true` on the state
    // query, but every enable attempt bounces to `supported: true` +
    // error. Returning `Ok(None)` here makes `keep_awake_set` report
    // `supported: false` so the UI can grey out the toggle cleanly,
    // matching how Windows/macOS surface the same "not available"
    // case. Checks the two standard systemd install paths; anything
    // exotic won't have systemd-inhibit anyway.
    let has_systemd_inhibit = std::path::Path::new("/usr/bin/systemd-inhibit").exists()
        || std::path::Path::new("/bin/systemd-inhibit").exists();
    if !has_systemd_inhibit {
        return Ok(None);
    }
    let child = Command::new("systemd-inhibit")
        .args([
            "--what=idle:sleep",
            "--who=ps5upload",
            "--why=active transfer",
            "sleep",
            "infinity",
        ])
        // See the caffeinate spawn — kill on Handle drop; exit-handler
        // release covers the never-dropped HOLDER static.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn systemd-inhibit: {e}"))?;
    Ok(Some(Handle::Process(child)))
}

// ─── Windows: SetThreadExecutionState ──────────────────────────────
//
// Inline FFI declarations instead of pulling in the `windows` or
// `winapi` crates — this is a single syscall with three small flag
// constants, and `extern "system"` with a raw function prototype
// keeps the desktop crate's dep graph lean (the windows crate family
// averages 20+ transitive deps for a meaningful build).
//
// Despite its name, SetThreadExecutionState is effectively process-
// wide on modern Windows: the flags stay asserted until a subsequent
// call to the same API clears them, regardless of which thread made
// the original call. MSDN still documents the per-thread semantics
// as a legacy API quirk.
#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
extern "system" {
    fn SetThreadExecutionState(flags: u32) -> u32;
}

#[cfg(target_os = "windows")]
const ES_CONTINUOUS: u32 = 0x8000_0000;
#[cfg(target_os = "windows")]
const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
#[cfg(target_os = "windows")]
const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

#[cfg(target_os = "windows")]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    // ES_CONTINUOUS makes the remaining flags persist across this
    // API call (without it, the flags would revert on the next call
    // from any thread in the process). SYSTEM_REQUIRED prevents
    // automatic sleep; DISPLAY_REQUIRED prevents screen blanking —
    // matches macOS `caffeinate -d -i -s` semantics.
    let prev = unsafe {
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
    };
    if prev == 0 {
        // MSDN: a zero return means SetThreadExecutionState failed.
        // Rare — the usual cause is restrictive Group Policy blocking
        // the API. Surface as a UI-visible error so users know why
        // the toggle bounced back off.
        return Err("SetThreadExecutionState failed (GPO restriction?)".to_string());
    }
    Ok(Some(Handle::WinExecState))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    Ok(None)
}
