//! Reveal a path in the OS file manager.
//!
//! Shared by `crash_reports` and `diag_log` (the bug-report flow) so the
//! per-platform spawn logic lives in exactly one place. We do this from the
//! backend rather than the renderer's `shell.open()` because the shell
//! plugin's `open` scope only permits `mailto:`/`tel:`/`https://` URLs — a
//! bare filesystem path fails its regex validation. Running the platform
//! file-manager directly has no such restriction.
//!
//! On mobile there's no browsable file manager for app-private storage, so
//! the caller surfaces the path as text instead.

use std::path::Path;

/// Open `path` (a directory) in the platform file manager. Best-effort:
/// returns an error string the caller can show, but never blocks.
#[cfg(target_os = "macos")]
pub fn reveal(path: &Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open failed: {e}"))
}

#[cfg(target_os = "windows")]
pub fn reveal(path: &Path) -> Result<(), String> {
    // explorer.exe returns a non-zero exit code even on success, so we only
    // care that the spawn itself worked.
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("explorer failed: {e}"))
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "android"),
    not(target_os = "ios")
))]
pub fn reveal(path: &Path) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("xdg-open failed: {e}"))
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn reveal(_path: &Path) -> Result<(), String> {
    Err("opening a folder isn't supported on this platform".into())
}
