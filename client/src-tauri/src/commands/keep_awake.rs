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
//!   Windows: a power request object (`PowerCreateRequest`) set to
//!            SystemRequired + DisplayRequired (close its handle to
//!            release)
//!
//! The Windows path is in-process, not a subprocess, so the holder
//! distinguishes between "process" and "power request" variants. On
//! unsupported platforms (BSDs, etc.) we return `supported: false`
//! and let the UI disable the toggle.

use std::collections::HashSet;
use std::sync::OnceLock;

#[cfg(target_os = "windows")]
use std::ffi::c_void;

use tokio::sync::Mutex;

/// Reason name owned by the manual Settings toggle.
const MANUAL_REASON: &str = "manual";

/// One live OS handle — one of the platform-specific variants — or
/// `None` when nothing is held.
enum Handle {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Process(tokio::process::Child),
    /// The power request's HANDLE, kept as an integer so `Handle`
    /// stays `Send` for the static holder.
    #[cfg(target_os = "windows")]
    WinPowerRequest(isize),
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
/// power requests are a Win32 API). Linux needs
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
/// (On Windows this only tidies up: the power request handle closes with
/// the process anyway.)
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
        Handle::WinPowerRequest(request) => {
            // Return values are ignored: a request that fails to clear
            // is still dropped when its handle closes, and there is
            // nothing more to do about a handle that fails to close.
            let request = request as *mut c_void;
            unsafe {
                PowerClearRequest(request, POWER_REQUEST_DISPLAY_REQUIRED);
                PowerClearRequest(request, POWER_REQUEST_SYSTEM_REQUIRED);
                CloseHandle(request);
            }
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

// ─── Windows: power request ────────────────────────────────────────
//
// Inline FFI declarations instead of pulling in the `windows` or
// `winapi` crates: four kernel32 calls and a few constants, and
// `extern "system"` with raw function prototypes keeps the desktop
// crate's dep graph lean (the windows crate family averages 20+
// transitive deps for a meaningful build).
//
// A power request is a kernel object owned by its handle, so the hold
// can be released from any thread. That matters here: these commands
// run on tokio worker threads, and the acquire and release of one hold
// usually land on different workers. `SetThreadExecutionState`, used
// before, is per thread: the ES_CONTINUOUS reset ran on another worker
// and cleared nothing, so every transfer left the display and the
// system held until the app quit (`powercfg /requests` listed one
// DISPLAY and one SYSTEM entry per stuck worker).
#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
extern "system" {
    fn PowerCreateRequest(context: *const ReasonContext) -> *mut c_void;
    fn PowerSetRequest(request: *mut c_void, request_type: i32) -> i32;
    fn PowerClearRequest(request: *mut c_void, request_type: i32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// `REASON_CONTEXT`. Its C `Reason` field is a union; with
/// `POWER_REQUEST_CONTEXT_SIMPLE_STRING` only the leading
/// `SimpleReasonString` pointer is read, and the unused fields after it
/// give the struct the size of the union's larger `Detailed` arm.
#[cfg(target_os = "windows")]
#[repr(C)]
struct ReasonContext {
    version: u32,
    flags: u32,
    simple_reason_string: *const u16,
    _localized_reason_id: u32,
    _reason_string_count: u32,
    _reason_strings: *const c_void,
}

#[cfg(target_os = "windows")]
const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;
#[cfg(target_os = "windows")]
const POWER_REQUEST_CONTEXT_SIMPLE_STRING: u32 = 0x1;
#[cfg(target_os = "windows")]
const POWER_REQUEST_DISPLAY_REQUIRED: i32 = 0;
#[cfg(target_os = "windows")]
const POWER_REQUEST_SYSTEM_REQUIRED: i32 = 1;
#[cfg(target_os = "windows")]
const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

#[cfg(target_os = "windows")]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    // Shown next to the process in `powercfg /requests`.
    let reason: Vec<u16> = "PS5 Upload: keeping the computer awake"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let context = ReasonContext {
        version: POWER_REQUEST_CONTEXT_VERSION,
        flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
        simple_reason_string: reason.as_ptr(),
        _localized_reason_id: 0,
        _reason_string_count: 0,
        _reason_strings: std::ptr::null(),
    };
    let request = unsafe { PowerCreateRequest(&context) };
    if request == INVALID_HANDLE_VALUE {
        return Err(format!(
            "PowerCreateRequest failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SystemRequired prevents automatic sleep; DisplayRequired prevents
    // screen blanking. Matches macOS `caffeinate -d -i -s` semantics.
    for request_type in [
        POWER_REQUEST_SYSTEM_REQUIRED,
        POWER_REQUEST_DISPLAY_REQUIRED,
    ] {
        if unsafe { PowerSetRequest(request, request_type) } == 0 {
            // Rare; restrictive Group Policy is the usual cause. Surface
            // it as a UI-visible error so users know why the toggle
            // bounced back off.
            let err = std::io::Error::last_os_error();
            unsafe { CloseHandle(request) };
            return Err(format!("PowerSetRequest failed: {err}"));
        }
    }
    Ok(Some(Handle::WinPowerRequest(request as isize)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn acquire_inhibitor() -> Result<Option<Handle>, String> {
    Ok(None)
}
