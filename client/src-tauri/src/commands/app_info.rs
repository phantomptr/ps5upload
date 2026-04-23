//! Bundled-markdown loaders for the FAQ and Changelog screens.
//!
//! Historically this module also hosted `app_version`, `app_platform`,
//! `set_save_logs`, `set_ui_log_enabled`, and `open_external` — all
//! ported from Electron's main.js. None were invoked from the renderer
//! after the Tauri rewrite (the built-in `@tauri-apps/api/app.getVersion`
//! + `plugin-shell.open` cover those needs), so they've been removed.

use tauri::{AppHandle, Manager};

/// Generic doc loader — reads a markdown file bundled with the app.
/// Packaged builds find it in `Resources/<name>` alongside the engine
/// binary; dev builds fall back to the repo root so edits hot-apply
/// without having to rebuild.
///
/// `name` is whitelisted to prevent directory-traversal or reading
/// arbitrary files. Only basenames listed in ALLOWED_DOCS are served.
const ALLOWED_DOCS: &[&str] = &["FAQ.md", "CHANGELOG.md", "README.md"];

fn load_doc(app: &AppHandle, name: &str) -> Result<String, String> {
    if !ALLOWED_DOCS.contains(&name) {
        return Err(format!("doc_load refuses: {name}"));
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // Packaged: two locations to cover both in-tree and out-of-tree
    // resource paths in tauri.conf.json:
    //   - `FAQ.md` → Resources/FAQ.md
    //   - `../../FAQ.md` → Resources/_up_/_up_/FAQ.md
    // Tauri preserves relative-path escapes (../) using `_up_` segments
    // so the directory tree inside the bundle is always rooted at the
    // Resources dir. Without the second candidate, `Couldn't load
    // CHANGELOG.md` on packaged builds even though the file is present
    // a few directories up.
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join(name));
        candidates.push(resource_dir.join("_up_").join("_up_").join(name));
    }
    // Dev: repo root. CARGO_MANIFEST_DIR points at client/src-tauri, so
    // parent().parent() gets to the repo root.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(repo_root) = manifest_dir.parent().and_then(|p| p.parent()) {
        candidates.push(repo_root.join(name));
    }
    for p in &candidates {
        if p.is_file() {
            return std::fs::read_to_string(p).map_err(|e| format!("read {p:?}: {e}"));
        }
    }
    Err(format!(
        "{name} not found. Searched:\n  {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    ))
}

/// Read the bundled FAQ.md.
#[tauri::command]
pub async fn faq_load(app: AppHandle) -> Result<String, String> {
    load_doc(&app, "FAQ.md")
}

/// Read the bundled CHANGELOG.md. Used by the Changelog landing page.
#[tauri::command]
pub async fn changelog_load(app: AppHandle) -> Result<String, String> {
    load_doc(&app, "CHANGELOG.md")
}
