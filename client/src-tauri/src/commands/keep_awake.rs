//! Keep-OS-awake inhibitor.
//!
//! One toggle in Settings; when on, the OS is asked to skip idle
//! sleep and display sleep until the app exits or the toggle flips
//! back off. Each platform uses its native primitive:
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

use std::sync::OnceLock;

use tokio::sync::Mutex;

/// Active inhibitor — one of the platform-specific variants, or
/// `None` when the toggle is off. Wrapped in a Mutex because the
/// Tauri command handler is async + could be called concurrently
/// from the renderer (e.g. rapid on/off clicks during a glitchy
/// touch event).
enum Handle {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Process(tokio::process::Child),
    #[cfg(target_os = "windows")]
    WinExecState,
}

static HOLDER: OnceLock<Mutex<Option<Handle>>> = OnceLock::new();

fn holder() -> &'static Mutex<Option<Handle>> {
    HOLDER.get_or_init(|| Mutex::new(None))
}

fn platform_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    ))
}

#[tauri::command]
pub async fn keep_awake_set(enabled: bool) -> serde_json::Value {
    let mut guard = holder().lock().await;
    if enabled {
        if guard.is_some() {
            return serde_json::json!({ "enabled": true, "supported": true });
        }
        match acquire_inhibitor() {
            Ok(Some(handle)) => {
                *guard = Some(handle);
                serde_json::json!({ "enabled": true, "supported": true })
            }
            Ok(None) => serde_json::json!({
                "enabled": false,
                "supported": false,
                "error": "keep-awake not supported on this platform",
            }),
            Err(e) => serde_json::json!({
                "enabled": false,
                "supported": true,
                "error": e,
            }),
        }
    } else {
        if let Some(handle) = guard.take() {
            release_inhibitor(handle).await;
        }
        serde_json::json!({ "enabled": false, "supported": platform_supported() })
    }
}

#[tauri::command]
pub async fn keep_awake_state() -> serde_json::Value {
    let guard = holder().lock().await;
    serde_json::json!({
        "enabled": guard.is_some(),
        "supported": platform_supported(),
    })
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
        .spawn()
        .map_err(|e| format!("spawn caffeinate: {e}"))?;
    Ok(Some(Handle::Process(child)))
}

#[cfg(target_os = "linux")]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    use tokio::process::Command;
    // systemd-inhibit is available on any modern desktop Linux (GNOME,
    // KDE, most distros with systemd-logind). Falls back gracefully if
    // not installed — we surface the error to the UI.
    let child = Command::new("systemd-inhibit")
        .args([
            "--what=idle:sleep",
            "--who=ps5upload",
            "--why=active transfer",
            "sleep",
            "infinity",
        ])
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
