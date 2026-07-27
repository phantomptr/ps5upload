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
//!    table + data table). Parsed by [`parse_sfo_string_keys`].
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
        ("param.sfo", parse_param_sfo_file(&param_sfo_path)?)
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

/// Parse a `param.sfo` file from local disk (PS4 / legacy PS5 homebrew).
fn parse_param_sfo_file(path: &Path) -> Result<FolderInspectResult> {
    let bytes = fs::read(path)?;
    parse_param_sfo_bytes(&bytes)
}

/// Parse a `param.sfo` byte slice into a [`FolderInspectResult`].
///
/// Extracts `TITLE`, `TITLE_ID`, `CONTENT_ID`, `APP_VER`, and `CATEGORY`
/// from the binary PSF key/value table. Non-string fields are ignored.
pub fn parse_param_sfo_bytes(bytes: &[u8]) -> Result<FolderInspectResult> {
    let kv = parse_sfo_string_keys(bytes).map_err(|e| anyhow::anyhow!("SFO parse error: {e}"))?;
    Ok(FolderInspectResult {
        path: String::new(),
        title: kv
            .get("TITLE")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        title_id: kv.get("TITLE_ID").cloned(),
        content_id: kv.get("CONTENT_ID").cloned(),
        content_version: kv.get("APP_VER").cloned(),
        application_category_type: None,
        icon0_path: None,
        total_size: 0,
        file_count: 0,
        skipped_paths: Vec::new(),
        meta_source: "param.sfo",
    })
}

/// Parse the string keys from a PARAM.SFO blob.
///
/// PSF layout: 20-byte header (magic `\x00PSF`, version, key-table offset,
/// data-table offset, entry count) followed by `entry_count` 16-byte index
/// entries, then the key table (NUL-terminated strings), then the data table.
///
/// Returns a flat `HashMap` of key → string value for string-typed entries
/// (PSF format `0x0004` utf-8 special and `0x0204` utf-8 normal). Non-string
/// entries (`PARENTAL_LEVEL` uint32, etc.) are silently skipped.
///
/// All offset arithmetic uses checked math to prevent integer-overflow panics
/// on truncated / malicious SFO blobs.
pub fn parse_sfo_string_keys(
    bytes: &[u8],
) -> std::result::Result<std::collections::HashMap<String, String>, String> {
    if bytes.len() < 20 {
        return Err("SFO too small".into());
    }
    if &bytes[0..4] != b"\x00PSF" {
        return Err("SFO magic mismatch".into());
    }
    let key_table_off = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let data_table_off = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let entry_count = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;
    let mut out = std::collections::HashMap::new();
    let table_off: usize = 20;
    for i in 0..entry_count {
        let e = match i.checked_mul(16).and_then(|m| table_off.checked_add(m)) {
            Some(e) => e,
            None => break,
        };
        match e.checked_add(16) {
            Some(end) if end <= bytes.len() => {}
            _ => break,
        }
        let key_off = u16::from_le_bytes([bytes[e], bytes[e + 1]]) as usize;
        let format = u16::from_le_bytes([bytes[e + 2], bytes[e + 3]]);
        let data_len =
            u32::from_le_bytes([bytes[e + 4], bytes[e + 5], bytes[e + 6], bytes[e + 7]]) as usize;
        let data_off =
            u32::from_le_bytes([bytes[e + 12], bytes[e + 13], bytes[e + 14], bytes[e + 15]])
                as usize;
        let key_abs = match key_table_off.checked_add(key_off) {
            Some(v) => v,
            None => continue,
        };
        let data_abs = match data_table_off.checked_add(data_off) {
            Some(v) => v,
            None => continue,
        };
        let data_end = match data_abs.checked_add(data_len) {
            Some(v) => v,
            None => continue,
        };
        if data_end > bytes.len() || key_abs >= bytes.len() {
            continue;
        }
        let key_end = (key_abs..bytes.len())
            .find(|i| bytes[*i] == 0)
            .unwrap_or(bytes.len());
        let key = String::from_utf8_lossy(&bytes[key_abs..key_end]).into_owned();
        if format == 0x0004 || format == 0x0204 {
            let value = String::from_utf8_lossy(&bytes[data_abs..data_end])
                .trim_end_matches('\0')
                .to_string();
            out.insert(key, value);
        }
    }
    Ok(out)
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
    // Some third-party dump tools emit numeric param.json fields as quoted
    // strings (e.g. "0"). Accept both so the game/patch/voucher
    // classification doesn't silently fall back to "unknown".
    let application_category_type = v.get("applicationCategoryType").and_then(|x| {
        x.as_i64()
            .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
    });

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

    #[test]
    fn param_json_accepts_numeric_or_string_category() {
        // Numeric form (standard).
        let num = parse_param_json_bytes(br#"{"applicationCategoryType":0}"#).unwrap();
        assert_eq!(num.application_category_type, Some(0));
        // String form (some third-party dump tools) must parse, not drop.
        let strv = parse_param_json_bytes(br#"{"applicationCategoryType":"0"}"#).unwrap();
        assert_eq!(strv.application_category_type, Some(0));
        let strv2 = parse_param_json_bytes(br#"{"applicationCategoryType":" 5 "}"#).unwrap();
        assert_eq!(strv2.application_category_type, Some(5));
        // Garbage string → None, not a parse error for the whole file.
        let bad = parse_param_json_bytes(br#"{"applicationCategoryType":"abc"}"#).unwrap();
        assert_eq!(bad.application_category_type, None);
    }

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

    // ── SFO parser tests ────────────────────────────────────────────────

    /// Build a minimal valid PSF binary with the given key→string-value
    /// pairs. The layout matches what Sony's tools produce: 20-byte header,
    /// index entries (16 bytes each), key table (NUL-terminated), data
    /// table (NUL-padded to entry length).
    fn build_sfo(entries: &[(&str, &str)]) -> Vec<u8> {
        let header_len = 20usize;
        let index_len = entries.len() * 16;
        // Key table: all keys concatenated with trailing NUL.
        let key_table: Vec<u8> = {
            let mut kt = Vec::new();
            for (k, _) in entries {
                kt.extend_from_slice(k.as_bytes());
                kt.push(0);
            }
            kt
        };
        // Data offsets are relative to data_table start; each value is
        // padded to include its NUL terminator, matching Sony's format.
        let data_entries: Vec<(usize, usize)> = entries
            .iter()
            .scan(0usize, |off, (_, v)| {
                let start = *off;
                let len = v.len() + 1; // include NUL
                *off += len;
                Some((start, len))
            })
            .collect();
        let data_table_len: usize = data_entries.iter().map(|(_, l)| *l).sum();
        let key_table_off = header_len + index_len;
        let data_table_off = key_table_off + key_table.len();

        let mut buf = Vec::with_capacity(data_table_off + data_table_len);
        // Header.
        buf.extend_from_slice(b"\x00PSF"); // magic
        buf.extend_from_slice(&0x00000101u32.to_le_bytes()); // version 1.1 (4 bytes)
        buf.extend_from_slice(&(key_table_off as u32).to_le_bytes());
        buf.extend_from_slice(&(data_table_off as u32).to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        // Index entries.
        let mut key_off = 0u16;
        for (i, (k, v)) in entries.iter().enumerate() {
            let (data_off, data_len) = data_entries[i];
            buf.extend_from_slice(&key_off.to_le_bytes()); // key offset
            buf.extend_from_slice(&0x0204u16.to_le_bytes()); // format: utf-8 normal
            buf.extend_from_slice(&(data_len as u32).to_le_bytes()); // data length
            buf.extend_from_slice(&(data_len as u32).to_le_bytes()); // max length
            buf.extend_from_slice(&(data_off as u32).to_le_bytes()); // data offset
            key_off += k.len() as u16 + 1; // advance past key + NUL
        }
        // Key table.
        buf.extend_from_slice(&key_table);
        // Data table.
        for (_, v) in entries {
            buf.extend_from_slice(v.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn parse_sfo_extracts_title_and_title_id() {
        let sfo = build_sfo(&[
            ("TITLE", "Test Game"),
            ("TITLE_ID", "CUSA00001"),
            ("CONTENT_ID", "EP0000-CUSA00001_00-TESTGAME0000000"),
            ("APP_VER", "01.00"),
        ]);
        let kv = parse_sfo_string_keys(&sfo).unwrap();
        assert_eq!(kv.get("TITLE").unwrap(), "Test Game");
        assert_eq!(kv.get("TITLE_ID").unwrap(), "CUSA00001");
        assert_eq!(kv.get("APP_VER").unwrap(), "01.00");
    }

    #[test]
    fn parse_sfo_bytes_returns_folder_inspect_result() {
        let sfo = build_sfo(&[
            ("TITLE", "My PS4 Game"),
            ("TITLE_ID", "CUSA12345"),
            ("CONTENT_ID", "EP0000-CUSA12345_00-MYPS4GAME0000001"),
            ("APP_VER", "01.02"),
        ]);
        let r = parse_param_sfo_bytes(&sfo).unwrap();
        assert_eq!(r.meta_source, "param.sfo");
        assert_eq!(r.title.as_deref(), Some("My PS4 Game"));
        assert_eq!(r.title_id.as_deref(), Some("CUSA12345"));
        assert_eq!(
            r.content_id.as_deref(),
            Some("EP0000-CUSA12345_00-MYPS4GAME0000001")
        );
        assert_eq!(r.content_version.as_deref(), Some("01.02"));
    }

    #[test]
    fn parse_sfo_rejects_bad_magic() {
        let mut bad = build_sfo(&[("TITLE", "x")]);
        bad[0] = b'X';
        assert!(parse_sfo_string_keys(&bad).is_err());
    }

    #[test]
    fn parse_sfo_rejects_truncated_blob() {
        let sfo = build_sfo(&[("TITLE", "x")]);
        let truncated = &sfo[..10];
        assert!(parse_sfo_string_keys(truncated).is_err());
    }

    #[test]
    fn parse_sfo_skips_non_string_entries() {
        // PARENTAL_LEVEL is a uint32 (format 0x0404), not utf-8.
        // We build a hybrid SFO manually since build_sfo only does strings.
        let header_len = 20usize;
        let entries: [(u16, &[u8]); 2] = [
            (0x0204, b"Test\0"),                 // TITLE, utf-8 string
            (0x0404, &[0x01, 0x00, 0x00, 0x00]), // PARENTAL_LEVEL, uint32 = 1
        ];
        let index_len = entries.len() * 16;
        let key_table: Vec<u8> = {
            let mut kt = Vec::new();
            kt.extend_from_slice(b"TITLE\0");
            kt.extend_from_slice(b"PARENTAL_LEVEL\0");
            kt
        };
        let key_table_off = header_len + index_len;
        let data_table_off = key_table_off + key_table.len();
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x00PSF");
        buf.extend_from_slice(&0x00000101u32.to_le_bytes()); // version 1.1 (4 bytes)
        buf.extend_from_slice(&(key_table_off as u32).to_le_bytes());
        buf.extend_from_slice(&(data_table_off as u32).to_le_bytes());
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        // Index: TITLE at key_off 0, data_off 0, len 5
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0x0204u16.to_le_bytes());
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        // Index: PARENTAL_LEVEL at key_off 6, data_off 5, len 4
        buf.extend_from_slice(&6u16.to_le_bytes());
        buf.extend_from_slice(&0x0404u16.to_le_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(&key_table);
        for (_, data) in &entries {
            buf.extend_from_slice(data);
        }
        let kv = parse_sfo_string_keys(&buf).unwrap();
        assert_eq!(kv.get("TITLE").unwrap(), "Test");
        assert!(!kv.contains_key("PARENTAL_LEVEL"));
    }

    #[test]
    fn inspect_folder_with_param_sfo_extracts_title() {
        let dir = tmpdir("paramsfo");
        let sce_sys = dir.join("sce_sys");
        fs::create_dir_all(&sce_sys).unwrap();
        let sfo = build_sfo(&[("TITLE", "PS4 Homebrew Game"), ("TITLE_ID", "CUSA99999")]);
        fs::write(sce_sys.join("param.sfo"), &sfo).unwrap();
        fs::write(dir.join("eboot.bin"), vec![0u8; 512]).unwrap();

        let r = inspect_folder(&dir).unwrap();
        assert_eq!(r.meta_source, "param.sfo");
        assert_eq!(r.title.as_deref(), Some("PS4 Homebrew Game"));
        assert_eq!(r.title_id.as_deref(), Some("CUSA99999"));
        assert_eq!(r.file_count, 2); // param.sfo + eboot.bin
        assert_eq!(r.total_size, sfo.len() as u64 + 512);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_sfo_strips_trailing_nul_from_title() {
        // Sony tools always include the NUL in the data_len field.
        // The parser must strip it so titles don't render with a trailing
        // invisible byte.
        let sfo = build_sfo(&[("TITLE", "Hello")]);
        let kv = parse_sfo_string_keys(&sfo).unwrap();
        assert_eq!(kv.get("TITLE").unwrap(), "Hello");
        assert!(!kv.get("TITLE").unwrap().contains('\0'));
    }
}
