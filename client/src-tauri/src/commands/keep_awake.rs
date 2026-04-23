//! Keep-OS-awake inhibitor.
//!
//! v1.5.4 used Electron's `powerSaveBlocker`. Porting to Tauri, we take
//! the simplest cross-platform shape: spawn a long-running platform tool
//! that asks the OS not to idle-sleep, store its handle, kill it on release.
//!
//!   macOS:   `caffeinate -disu`
//!   Linux:   `systemd-inhibit --what=idle:sleep ... sleep infinity`
//!   Windows: not wired yet — `SetThreadExecutionState` is in-process,
//!            would need the `windows` crate as a new dep. Tracked as
//!            a follow-up; `supported: false` is returned so the UI can
//!            disable the toggle cleanly.
//!
//! v1.5.4's "auto" mode (enable during active transfers, release after
//! 15 min idle) is **deliberately not in this port** — wiring it
//! correctly requires a transfer-active state stream from the engine,
//! which is its own follow-up. Today the toggle is manual on/off.

use std::sync::OnceLock;

use tokio::process::Child;
use tokio::sync::Mutex;

static HOLDER: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn holder() -> &'static Mutex<Option<Child>> {
    HOLDER.get_or_init(|| Mutex::new(None))
}

fn platform_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

#[tauri::command]
pub async fn keep_awake_set(enabled: bool) -> serde_json::Value {
    let mut guard = holder().lock().await;
    if enabled {
        if guard.is_some() {
            return serde_json::json!({ "enabled": true, "supported": true });
        }
        match spawn_inhibitor() {
            Ok(Some(child)) => {
                *guard = Some(child);
                serde_json::json!({ "enabled": true, "supported": true })
            }
            Ok(None) => serde_json::json!({
                "enabled": false,
                "supported": false,
                "error": "keep-awake not yet implemented on this platform",
            }),
            Err(e) => serde_json::json!({
                "enabled": false,
                "supported": true,
                "error": e,
            }),
        }
    } else {
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
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

#[cfg(target_os = "macos")]
fn spawn_inhibitor() -> Result<Option<Child>, String> {
    use tokio::process::Command;
    // -d keep display awake, -i keep system awake, -s prevent system sleep,
    // -u assert user activity. Combined flags cover both idle sleep and
    // display sleep, matching v1.5.4's 'prevent-display-sleep' blocker.
    let child = Command::new("caffeinate")
        .args(["-disu"])
        .spawn()
        .map_err(|e| format!("spawn caffeinate: {e}"))?;
    Ok(Some(child))
}

#[cfg(target_os = "linux")]
fn spawn_inhibitor() -> Result<Option<Child>, String> {
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
    Ok(Some(child))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn spawn_inhibitor() -> Result<Option<Child>, String> {
    Ok(None)
}
