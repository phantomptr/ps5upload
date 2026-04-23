//! Static-string loaders for the FAQ and Changelog screens.
//!
//! CHANGELOG.md and FAQ.md are embedded at compile time via
//! `include_str!`, not fetched from a filesystem resource, because the
//! Windows `--no-bundle` build ships just the portable exe with no
//! Resources directory next to it. A runtime `resource_dir()` lookup
//! would return "file not found" on Windows while working fine on
//! macOS/Linux, producing a split-brain user experience where the
//! Changelog screen works on one platform and errors out on another.
//!
//! Embedding adds ~20 KB to the binary (the current size of both
//! files combined) — a rounding error against the ~10 MB desktop
//! binary — in exchange for "always works, no matter how the app is
//! packaged." Updates to CHANGELOG/FAQ require a rebuild, which is
//! how release cuts already work.

use tauri::AppHandle;

/// Read the bundled FAQ.md.
#[tauri::command]
pub async fn faq_load(_app: AppHandle) -> Result<String, String> {
    Ok(include_str!("../../../../FAQ.md").to_string())
}

/// Read the bundled CHANGELOG.md. Used by the Changelog landing page.
#[tauri::command]
pub async fn changelog_load(_app: AppHandle) -> Result<String, String> {
    Ok(include_str!("../../../../CHANGELOG.md").to_string())
}
