//! Host-side game folder inspection.
//!
//! Ported from the v1 `shared/game-meta.js`. Given a local folder that looks
//! like an extracted PS5 app (contains `sce_sys/`), returns the title,
//! title-id, content-id, version, and disk footprint so the client can show
//! a preview card before upload.
//!
//! Two metadata sources, tried in order:
//!
//! 1. `sce_sys/param.json` — current PS5 format, plain JSON. All recent
//!    commercial PS5 titles use this. Schema includes `titleId`, `contentId`,
//!    `contentVersion`, and per-locale `titleName` under
//!    `localizedParameters.{lang}.titleName`. The default language is in
//!    `localizedParameters.defaultLanguage`.
//!
//! 2. `sce_sys/param.sfo` — legacy Sony SFO binary. Used by PS4 games and
//!    older PS5 homebrew PKGs. Binary format (20B header + index + key
//!    table + data table). Parser stubbed here: the 2.1 target is PS5
//!    games, which ship param.json; SFO support can be added when real
//!    user reports demand it.
//!
//! Neither parser fails hard on missing fields — the client is happy to
//! show a card with just a title-id if that's all we can extract.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderInspectResult {
    /// Absolute path to the folder that was inspected.
    pub path: String,
    /// Title from param.json `localizedParameters.{defaultLanguage}.titleName`
    /// (or any locale we can find if defaultLanguage is missing).
    pub title: Option<String>,
    /// Title ID, e.g. "PPSA00000".
    pub title_id: Option<String>,
    /// Content ID, e.g. "EP0000-PPSA00000_00-XXXXXXXXXXXXXXXX".
    pub content_id: Option<String>,
    /// Content version string, e.g. "01.000.011".
    pub content_version: Option<String>,
    /// `applicationCategoryType` from param.json (0 = game, other values
    /// are patches / vouchers / special categories).
    pub application_category_type: Option<i64>,
    /// Absolute path to `sce_sys/icon0.png` if present. UI renders via
    /// Tauri's asset:// protocol.
    pub icon0_path: Option<String>,
    /// Total size across all files under `path`, in bytes.
    pub total_size: u64,
    /// File count under `path` (regular files only, not dirs / symlinks).
    pub file_count: u64,
    /// Paths that couldn't be read during the walk (permission denied,
    /// I/O error, etc.). When non-empty, `total_size` and `file_count`
    /// reflect only what could be inspected — the UI should surface a
    /// "partial inspection" note so the user doesn't pick a destination
    /// drive based on an under-reported size.
    #[serde(default)]
    pub skipped_paths: Vec<String>,
    /// Source of the metadata: "param.json", "param.sfo", or "none".
    pub meta_source: &'static str,
}

/// Inspect a folder and return a preview payload for the UI.
pub fn inspect_folder(path: &Path) -> Result<FolderInspectResult> {
    let sce_sys = path.join("sce_sys");
    let param_json_path = sce_sys.join("param.json");
    let param_sfo_path = sce_sys.join("param.sfo");
    let icon0_path = sce_sys.join("icon0.png");

    let (meta_source, mut result) = if param_json_path.is_file() {
        ("param.json", parse_param_json(&param_json_path)?)
    } else if param_sfo_path.is_file() {
        // Stub: SFO is a binary format we don't parse yet. Return just the
        // disk-footprint bits and let the UI show a fallback card.
        ("param.sfo", FolderInspectResult::empty_at(path))
    } else {
        ("none", FolderInspectResult::empty_at(path))
    };

    result.meta_source = meta_source;
    result.path = path.to_string_lossy().into_owned();

    if icon0_path.is_file() {
        result.icon0_path = Some(icon0_path.to_string_lossy().into_owned());
    }

    let walk = walk_sizes(path)?;
    result.total_size = walk.total_size;
    result.file_count = walk.file_count;
    result.skipped_paths = walk.skipped;

    Ok(result)
}

impl FolderInspectResult {
    fn empty_at(path: &Path) -> Self {
        Self {
            path: path.to_string_lossy().into_owned(),
            title: None,
            title_id: None,
            content_id: None,
            content_version: None,
            application_category_type: None,
            icon0_path: None,
            total_size: 0,
            file_count: 0,
            skipped_paths: Vec::new(),
            meta_source: "none",
        }
    }
}

fn parse_param_json(path: &Path) -> Result<FolderInspectResult> {
    let bytes = fs::read(path)?;
    parse_param_json_bytes(&bytes)
}

/// Parse a `param.json` payload directly from bytes. Used by the
/// engine's `/api/ps5/game-meta` endpoint, which streams the file
/// off the PS5 via FS_READ instead of reading it from local disk.
/// Returns a `FolderInspectResult` with `path`, `icon0_path`,
/// `total_size`, `file_count`, and `skipped_paths` left as defaults —
/// those are local-disk concepts that don't apply to a remote fetch.
pub fn parse_param_json_bytes(bytes: &[u8]) -> Result<FolderInspectResult> {
    let v: serde_json::Value = serde_json::from_slice(bytes)?;

    let title_id = v.get("titleId").and_then(|x| x.as_str()).map(String::from);
    let content_id = v
        .get("contentId")
        .and_then(|x| x.as_str())
        .map(String::from);
    let content_version = v
        .get("contentVersion")
        .and_then(|x| x.as_str())
        .map(String::from);
    let application_category_type = v.get("applicationCategoryType").and_then(|x| x.as_i64());

    let title = localized_title(&v);

    Ok(FolderInspectResult {
        path: String::new(), // set by caller
        title,
        title_id,
        content_id,
        content_version,
        application_category_type,
        icon0_path: None,
        total_size: 0,
        file_count: 0,
        skipped_paths: Vec::new(),
        meta_source: "param.json",
    })
}

fn localized_title(v: &serde_json::Value) -> Option<String> {
    let lp = v.get("localizedParameters")?.as_object()?;
    // Prefer the default language if set and present.
    if let Some(default_lang) = lp.get("defaultLanguage").and_then(|x| x.as_str()) {
        if let Some(title) = lp
            .get(default_lang)
            .and_then(|locale| locale.get("titleName"))
            .and_then(|n| n.as_str())
        {
            return Some(title.to_string());
        }
    }
    // Fall back to the first locale that has a titleName.
    for (_lang, locale) in lp {
        if let Some(title) = locale.get("titleName").and_then(|n| n.as_str()) {
            return Some(title.to_string());
        }
    }
    None
}

/// Recursive size walk. Uses an explicit stack instead of recursion so we
/// don't blow the stack on deep Unreal asset trees. Symlinks are followed
/// at the top level only; we don't descend into them (prevents cycles on
/// pathologically-linked folders).
///
/// Root-level readdir failures surface as `Err` — if the user pointed at a
/// vanished or permission-denied path, the UI needs to show an error,
/// not a "0-byte, 0-file successful inspection." Subdirectory / per-entry
/// errors are recorded on `skipped_paths` so the caller can surface a
/// partial-inspection warning instead of silently under-reporting.
fn walk_sizes(root: &Path) -> Result<WalkResult> {
    let mut total_size = 0u64;
    let mut file_count = 0u64;
    let mut skipped: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut first = true;

    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if first => {
                return Err(
                    anyhow::Error::new(e).context(format!("cannot read folder {}", dir.display()))
                );
            }
            Err(_) => {
                skipped.push(dir.to_string_lossy().into_owned());
                continue;
            }
        };
        first = false;
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else {
                skipped.push(entry.path().to_string_lossy().into_owned());
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                match entry.metadata() {
                    Ok(md) => {
                        total_size += md.len();
                        file_count += 1;
                    }
                    Err(_) => skipped.push(entry.path().to_string_lossy().into_owned()),
                }
            }
            // symlinks intentionally skipped
        }
    }
    Ok(WalkResult {
        total_size,
        file_count,
        skipped,
    })
}

struct WalkResult {
    total_size: u64,
    file_count: u64,
    skipped: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn tmpdir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ps5upload_game_meta_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn inspect_folder_without_sce_sys_returns_empty_meta() {
        let dir = tmpdir("nosysfs");
        fs::write(dir.join("readme.txt"), b"hello").unwrap();

        let r = inspect_folder(&dir).unwrap();
        assert_eq!(r.meta_source, "none");
        assert!(r.title.is_none());
        assert!(r.title_id.is_none());
        assert_eq!(r.file_count, 1);
        assert_eq!(r.total_size, 5);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inspect_folder_with_param_json_extracts_title_and_id() {
        let dir = tmpdir("paramjson");
        let sce_sys = dir.join("sce_sys");
        fs::create_dir_all(&sce_sys).unwrap();
        let pj = sce_sys.join("param.json");
        let mut f = fs::File::create(&pj).unwrap();
        f.write_all(
            r#"{
                "titleId": "PPSA00000",
                "contentId": "EP0000-PPSA00000_00-XXXXXXXXXXXXXXXX",
                "contentVersion": "01.000.011",
                "applicationCategoryType": 0,
                "localizedParameters": {
                    "defaultLanguage": "en-US",
                    "en-US": { "titleName": "Example Title" },
                    "ja-JP": { "titleName": "\u6b66\u660c" }
                }
            }"#
            .as_bytes(),
        )
        .unwrap();
        // Also make a fake eboot so total_size/file_count are non-zero
        fs::write(dir.join("eboot.bin"), vec![0u8; 1024]).unwrap();
        fs::write(sce_sys.join("icon0.png"), vec![0u8; 16]).unwrap();

        let r = inspect_folder(&dir).unwrap();
        assert_eq!(r.meta_source, "param.json");
        assert_eq!(r.title_id.as_deref(), Some("PPSA00000"));
        assert_eq!(
            r.content_id.as_deref(),
            Some("EP0000-PPSA00000_00-XXXXXXXXXXXXXXXX")
        );
        assert_eq!(r.content_version.as_deref(), Some("01.000.011"));
        assert_eq!(r.application_category_type, Some(0));
        assert_eq!(r.title.as_deref(), Some("Example Title"));
        assert!(r.icon0_path.is_some());
        assert_eq!(r.file_count, 3); // param.json + eboot.bin + icon0.png
        assert_eq!(r.total_size, 1024 + 16 + pj.metadata().unwrap().len());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inspect_folder_errors_on_nonexistent_root() {
        // User pointed at a vanished path — should error, not silently
        // report (0, 0). This is the regression guard for the
        // silent-success bug where a 0-byte "empty folder" preview
        // would render confidently for a dead path.
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ps5upload_does_not_exist_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let err = inspect_folder(&p).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cannot read folder") || msg.contains("No such"),
            "got: {msg}"
        );
    }

    #[test]
    fn inspect_folder_falls_back_to_any_locale_when_default_missing() {
        let dir = tmpdir("falllang");
        let sce_sys = dir.join("sce_sys");
        fs::create_dir_all(&sce_sys).unwrap();
        fs::write(
            sce_sys.join("param.json"),
            r#"{
                "titleId": "TEST00001",
                "localizedParameters": {
                    "ja-JP": { "titleName": "\u30c6\u30b9\u30c8\u30b2\u30fc\u30e0" }
                }
            }"#
            .as_bytes(),
        )
        .unwrap();

        let r = inspect_folder(&dir).unwrap();
        // Title round-trips through serde_json's Unicode escape handling.
        assert_eq!(
            r.title.as_deref(),
            Some("\u{30c6}\u{30b9}\u{30c8}\u{30b2}\u{30fc}\u{30e0}")
        );

        fs::remove_dir_all(&dir).unwrap();
    }
}
