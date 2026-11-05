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

/// Whether `path` may be opened as a folder: a directory that is not a program. A macOS app
/// bundle is a directory, and `open` would launch it.
fn is_plain_folder(path: &Path) -> bool {
    path.is_dir()
        && !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

/// Open a folder in the file manager, for the renderer (`lib/openLocalPath.ts`). Done here so
/// the webview never has a way to hand the OS opener an arbitrary path, an executable included.
#[tauri::command]
pub async fn open_local_folder(path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = Path::new(&path);
        if !is_plain_folder(path) {
            return Err(format!("{} is not a folder", path.display()));
        }
        reveal(path)
    })
    .await
    .map_err(|e| format!("open_local_folder: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::is_plain_folder;

    #[test]
    fn only_a_real_folder_opens() {
        let dir = std::env::temp_dir().join(format!("ps5upload-reveal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Game.app")).unwrap();
        std::fs::write(dir.join("program.exe"), b"x").unwrap();
        assert!(is_plain_folder(&dir));
        assert!(!is_plain_folder(&dir.join("Game.app")), "launches on macOS");
        assert!(!is_plain_folder(&dir.join("program.exe")));
        assert!(!is_plain_folder(&dir.join("missing")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
