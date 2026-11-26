//! In-app screenshot capture for bug reports.
//!
//! The renderer captures the current app view to a PNG (DOM → image, in JS —
//! the only approach that works identically on desktop AND Android) and hands
//! the base64 here to persist. Files live next to the user config at
//! `~/.ps5upload/bug-screenshots/` (app-private config dir on mobile), named
//! `screenshot-YYYYMMDD-HHMMSS.png` by the renderer (local time). The Bug
//! Report page lists them as a selectable gallery; chosen ones ride into the
//! zip via the existing `image_paths` flow (real on-disk paths, so they read
//! back with `std::fs` on every platform — unlike Android `content://` file
//! picks).

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Keep at most this many screenshots; oldest (lexically-, i.e. chrono-,
/// earliest) are pruned. A PNG of the app window is a few hundred KB, so 50 is
/// a bounded handful of MB.
const MAX_SHOTS: usize = 50;

fn shots_dir(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let base = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("cannot resolve app config dir: {e}"))?;
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let base = app
        .path()
        .home_dir()
        .map_err(|e| format!("cannot resolve home dir: {e}"))?
        .join(".ps5upload");
    let dir = base.join("bug-screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {dir:?}: {e}"))?;
    Ok(dir)
}

/// Reject anything that isn't a plain `*.png` leaf — no path components, no
/// traversal. Returns the validated leaf or an error.
fn safe_png_name(name: &str) -> Result<String, String> {
    let leaf = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if leaf.is_empty()
        || leaf != name
        || !leaf.to_ascii_lowercase().ends_with(".png")
        || leaf.contains(['/', '\\'])
    {
        return Err(format!("invalid screenshot name: {name:?}"));
    }
    Ok(leaf.to_string())
}

/// All `*.png` files, newest-first (datetime filenames sort chronologically,
/// so a reverse lexical sort is newest-first).
fn list_png_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension()
                        .map(|x| x.eq_ignore_ascii_case("png"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

#[derive(Serialize)]
pub struct SavedShot {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    /// `data:image/png;base64,…` for an instant thumbnail without a re-read.
    pub data_url: String,
}

fn to_shot(path: &Path, data: &[u8]) -> SavedShot {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    SavedShot {
        name: path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string(),
        path: path.to_string_lossy().into_owned(),
        bytes: data.len() as u64,
        data_url: format!("data:image/png;base64,{b64}"),
    }
}

/// Persist one capture. `base64_png` is the raw base64 (no `data:` prefix) the
/// renderer produced from the canvas. Prunes oldest beyond `MAX_SHOTS`.
#[tauri::command]
pub async fn screenshot_save(
    app: AppHandle,
    name: String,
    base64_png: String,
) -> Result<SavedShot, String> {
    let leaf = safe_png_name(&name)?;
    let dir = shots_dir(&app)?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(base64_png.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;
    let path = dir.join(&leaf);
    std::fs::write(&path, &data).map_err(|e| format!("write {path:?}: {e}"))?;

    // Prune oldest beyond the cap.
    let files = list_png_files(&dir); // newest-first
    for old in files.iter().skip(MAX_SHOTS) {
        let _ = std::fs::remove_file(old);
    }
    Ok(to_shot(&path, &data))
}

/// Recent screenshots (newest-first), with inline thumbnails, capped at
/// `limit` (default: every kept capture, at most `MAX_SHOTS`).
#[tauri::command]
pub async fn screenshot_list(
    app: AppHandle,
    limit: Option<usize>,
) -> Result<Vec<SavedShot>, String> {
    let dir = shots_dir(&app)?;
    let cap = limit.unwrap_or(MAX_SHOTS);
    let mut out = Vec::new();
    for p in list_png_files(&dir).into_iter().take(cap) {
        if let Ok(data) = std::fs::read(&p) {
            out.push(to_shot(&p, &data));
        }
    }
    Ok(out)
}

/// Remove the named captures from `dir`. Each name must be a plain `*.png`
/// leaf (a path or traversal is skipped, never deleted). Returns the names
/// that are gone afterwards, including any already missing, so the gallery
/// can drop exactly those.
fn delete_shots(dir: &Path, names: &[String]) -> Vec<String> {
    let mut gone = Vec::new();
    for name in names {
        let Ok(leaf) = safe_png_name(name) else {
            continue;
        };
        let path = dir.join(&leaf);
        match std::fs::remove_file(&path) {
            Ok(()) => gone.push(leaf),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => gone.push(leaf),
            Err(_) => {}
        }
    }
    gone
}

/// Delete captures by name. Returns the names that no longer exist.
#[tauri::command]
pub async fn screenshot_delete(app: AppHandle, names: Vec<String>) -> Result<Vec<String>, String> {
    let dir = shots_dir(&app)?;
    Ok(delete_shots(&dir, &names))
}

/// Show the captures folder in the system file manager (desktop only; there
/// is no browsable file manager for app-private storage on mobile).
#[tauri::command]
pub async fn screenshot_open_dir(app: AppHandle) -> Result<(), String> {
    let dir = shots_dir(&app)?;
    super::reveal::reveal(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_png_name_accepts_plain_and_rejects_paths() {
        assert_eq!(
            safe_png_name("screenshot-20260605-120000.png").unwrap(),
            "screenshot-20260605-120000.png"
        );
        assert!(safe_png_name("../evil.png").is_err());
        assert!(safe_png_name("a/b.png").is_err());
        assert!(safe_png_name("notpng.txt").is_err());
        assert!(safe_png_name("").is_err());
    }

    #[test]
    fn newest_first_ordering() {
        let dir = crate::test_tmp::temp_path(format!("ps5up-shots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for n in [
            "screenshot-20260605-100000.png",
            "screenshot-20260605-120000.png",
        ] {
            std::fs::write(dir.join(n), b"\x89PNG").unwrap();
        }
        let files = list_png_files(&dir);
        assert!(files[0].to_string_lossy().contains("120000"));
        assert!(files[1].to_string_lossy().contains("100000"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_named_pngs_and_never_leaves_the_folder() {
        let root = crate::test_tmp::temp_path(format!("ps5up-shots-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("bug-screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::write(dir.join("b.png"), b"x").unwrap();
        std::fs::write(root.join("outside.png"), b"x").unwrap();
        let gone = delete_shots(
            &dir,
            &[
                "a.png".into(),
                "../outside.png".into(),
                "missing.png".into(),
            ],
        );
        assert_eq!(gone, vec!["a.png".to_string(), "missing.png".to_string()]);
        assert!(!dir.join("a.png").exists());
        assert!(dir.join("b.png").exists());
        assert!(root.join("outside.png").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
