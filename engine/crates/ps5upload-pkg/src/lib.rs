//! PS4/PS5 PKG header parser.
//!
//! Parses the unencrypted header of a `.pkg` file to extract metadata
//! the install UI needs: content_id, title (from PARAM.SFO), category,
//! icon (from ICON0.PNG entry), and total size. We never touch the
//! encrypted body — Sony's BGFT installer on the PS5 owns decryption
//! using device keys.
//!
//! Layout reference: psdevwiki PS4 "Package files" page. The header
//! starts with magic `\x7FCNT` (`0x7F434E54`) for stock PSN packages.
//! Community formats (FPKG variants, license-only PKGs) sometimes use
//! different magics — those are surfaced as [`PkgKind::Unknown`] with
//! enough metadata for the UI to show a "format not recognized — install
//! at your own risk" prompt rather than reject outright.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Magic bytes of a stock PS4/PS5 .pkg file: `\x7FCNT`.
pub const PKG_MAGIC: u32 = 0x7F434E54;

/// PARAM.SFO entry id inside a PKG.
const ENTRY_PARAM_SFO: u32 = 0x1000;
/// ICON0.PNG entry id inside a PKG.
const ENTRY_ICON0_PNG: u32 = 0x1200;

/// Maximum bytes we'll read for ICON0.PNG. Stock PS5 icons are <500 KiB;
/// cap defensively against a malformed entry.
const MAX_ICON_BYTES: u32 = 4 * 1024 * 1024;

/// Maximum size of the PARAM.SFO blob we'll parse. Stock SFOs are <8 KiB.
const MAX_SFO_BYTES: u32 = 256 * 1024;

#[derive(Debug, Error)]
pub enum PkgError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("file too small to be a PKG: {0} bytes")]
    Truncated(u64),
    #[error("header parse: {0}")]
    Header(&'static str),
    #[error("invalid utf8 in {field}: {err}")]
    Utf8 { field: &'static str, err: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PkgKind {
    /// Stock PS4/PS5 PKG with `\x7FCNT` magic.
    Standard,
    /// Magic byte sequence we don't recognise. Could be an FPKG (fake
    /// PKG used by the homebrew/cracked-content community), a
    /// license-only DRM unlock file, or simply the wrong file. The
    /// install UI should warn the user but still permit an install
    /// attempt — Sony's BGFT will reject anything it doesn't accept,
    /// and surfacing the magic helps the user diagnose.
    Unknown { magic_hex: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgMetadata {
    /// Path of the PKG that was parsed. Single file, not the split set.
    pub path: PathBuf,
    /// Total bytes of the file on disk (sum across split parts when
    /// present — see [`SplitPkgMetadata`]).
    pub size: u64,
    /// Header magic + classification.
    pub kind: PkgKind,
    /// 36-char content id (e.g. `EP0006-CUSA45456_00-...`). Empty for
    /// `Unknown` kind. Trailing NULs trimmed.
    pub content_id: String,
    /// Title from PARAM.SFO key `TITLE`. Empty if SFO missing.
    pub title: String,
    /// Title id from PARAM.SFO key `TITLE_ID`. Often duplicated within
    /// `content_id`; we surface both because some non-standard PKGs
    /// have one without the other.
    pub title_id: String,
    /// PARAM.SFO `CATEGORY` field — `gd` (game), `gp` (patch),
    /// `ac` (DLC/add-on), `gde` (extra), etc.
    pub category: String,
    /// Mapping of `category` to BGFT's `package_type` string. None
    /// when the category is unknown / SFO missing — caller may still
    /// attempt install with `package_type=PS4GD` as a default.
    pub package_type: Option<String>,
    /// PNG bytes from ICON0.PNG entry, if present. None if the entry
    /// is missing or oversize. Base64-encoded for transport across
    /// the Tauri/HTTP boundary; the React side decodes for <img>.
    pub icon_png_base64: Option<String>,
    /// Non-fatal warnings raised during parse — surfaced in the UI as
    /// a yellow caution row. Empty for a clean stock PKG.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitPkgMetadata {
    /// Ordered list of part files, [0] = `<base>.pkg`,
    /// [1] = `<base>.pkg.0`, [2] = `<base>.pkg.1`, ...
    pub parts: Vec<PathBuf>,
    /// Per-part sizes in bytes (same order as `parts`).
    pub part_sizes: Vec<u64>,
    /// Sum of all `part_sizes`.
    pub total_size: u64,
    /// Metadata parsed from `parts[0]` (the only part with the PKG
    /// header). Fields like `title` / `icon` come from the lead file.
    pub head: PkgMetadata,
}

/// Parse the PKG header of a single `.pkg` file. Best-effort: a parse
/// failure on PARAM.SFO produces a warning but doesn't fail the call —
/// the user can still see the file and decide whether to install.
pub fn parse_pkg(path: &Path) -> Result<PkgMetadata, PkgError> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    if size < 0xA0 {
        // Header is ~160 bytes; anything smaller can't be a real PKG.
        return Err(PkgError::Truncated(size));
    }

    let mut head = [0u8; 0xA0];
    f.read_exact(&mut head)?;

    let magic = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let mut warnings = Vec::new();

    let mut meta = PkgMetadata {
        path: path.to_path_buf(),
        size,
        kind: PkgKind::Standard,
        content_id: String::new(),
        title: String::new(),
        title_id: String::new(),
        category: String::new(),
        package_type: None,
        icon_png_base64: None,
        warnings: Vec::new(),
    };

    if magic != PKG_MAGIC {
        meta.kind = PkgKind::Unknown {
            magic_hex: format!("{magic:08X}"),
        };
        // Soften the warning. On a jailbroken PS5 with kstuff loaded
        // (the only environment ps5upload runs in), Sony's
        // signature/DRM checks are bypassed at the kernel level —
        // fake-signed retail-format PKGs install fine, and that's
        // the common case. The pre-2.2.45 wording blamed BGFT
        // ("Sony BGFT will reject this") which was misleading on
        // two counts: (1) we now use AppInstUtil as the primary
        // install backend (2.2.44, etaHEN-style), not BGFT
        // directly; (2) most fake PKGs preserve the canonical
        // \x7FCNT magic anyway — a non-canonical magic typically
        // means a renamed non-PKG file or a tool-specific format
        // (FakePKG variants, .fpkg dumps, etc.) that Sony's
        // installer won't know how to parse. Tell the user that
        // honestly and let them proceed if they want to try.
        // 0x7F464948 = `\x7FFIH` is what newer PS5-native fakepkg
        // signing tools emit (observed on EP/PS5_/DLPSGAME-prefixed
        // pkgs). Sony's installer accepts both formats — only OUR
        // parser doesn't fully understand `\x7FFIH` yet, so the
        // header fields below stay empty (content_id, title, etc.).
        // Pre-2.2.52 the warning copy implied "your pkg is probably
        // broken / renamed" which scared users away from working
        // files; reframe as "we couldn't extract metadata" so they
        // know the install will still proceed and Sony's installer
        // is the source of truth on whether the bytes are valid.
        let magic_label = match magic {
            0x7F464948 => " (`\\x7FFIH` — used by recent PS5-native fakepkg signing tools)",
            _ => "",
        };
        meta.warnings.push(format!(
            "Couldn't extract pkg metadata: header magic is 0x{magic:08X}{magic_label}, \
             not the canonical PS4 fakepkg magic 0x7F434E54 (`\\x7FCNT`) that our parser knows. \
             Content ID, title, and category will be empty in the queue row. The install will \
             still proceed when you click Start — Sony's own installer is the source of truth \
             for whether the bytes are valid. If you've installed this pkg before via the PS5's \
             Settings → Debug Settings → Install Package menu, our path should also work \
             (2.2.52+ routes the install through ShellUI's process)."
        ));
        // Don't try to parse the rest — layout is unknown.
        return Ok(meta);
    }

    // Stock PKG layout — read offsets we care about.
    let entry_count = u32::from_be_bytes([head[0x10], head[0x11], head[0x12], head[0x13]]);
    let table_offset = u32::from_be_bytes([head[0x18], head[0x19], head[0x1A], head[0x1B]]);
    // content_id is at 0x40, 36 bytes ASCII with trailing NULs.
    let cid_raw = &head[0x40..0x40 + 36];
    let cid_end = cid_raw.iter().position(|&b| b == 0).unwrap_or(36);
    meta.content_id = String::from_utf8_lossy(&cid_raw[..cid_end])
        .trim()
        .to_string();

    if entry_count == 0 || entry_count > 1024 {
        warnings.push(format!(
            "PKG entry count {entry_count} out of expected range 1..1024 — header may be corrupt"
        ));
        meta.warnings = warnings;
        return Ok(meta);
    }

    // Walk the entry table. Each entry is 0x20 bytes.
    if let Err(e) = walk_entries(&mut f, table_offset, entry_count, &mut meta, &mut warnings) {
        warnings.push(format!("entry table walk failed: {e}"));
    }

    meta.package_type = derive_package_type(&meta.category);
    meta.warnings = warnings;
    Ok(meta)
}

/// Detect a split-pkg set rooted at `head_path`. Walks the parent
/// directory for siblings named `<base>.pkg.0`, `<base>.pkg.1`, etc.
/// in numeric order. Returns the lead file as a single-part metadata
/// if no split siblings are found.
pub fn parse_split_pkg(head_path: &Path) -> Result<SplitPkgMetadata, PkgError> {
    let head = parse_pkg(head_path)?;
    let stem = head_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let dir = head_path.parent().unwrap_or(Path::new("."));

    let mut parts = vec![head_path.to_path_buf()];
    let mut part_sizes = vec![head.size];

    // Look for <stem>.0, <stem>.1, ... in the same directory.
    let prefix = format!("{stem}.");
    let mut idx: u32 = 0;
    loop {
        let candidate = dir.join(format!("{prefix}{idx}"));
        if !candidate.is_file() {
            break;
        }
        let size = std::fs::metadata(&candidate)?.len();
        parts.push(candidate);
        part_sizes.push(size);
        idx = idx
            .checked_add(1)
            .ok_or(PkgError::Header("split-pkg index overflow"))?;
        if idx > 1024 {
            // Sony PKGs cap at ~16 splits in practice; 1024 is a sanity
            // ceiling, never expected to trigger.
            break;
        }
    }
    let total_size: u64 = part_sizes.iter().sum();

    Ok(SplitPkgMetadata {
        parts,
        part_sizes,
        total_size,
        head,
    })
}

fn walk_entries(
    f: &mut File,
    table_offset: u32,
    entry_count: u32,
    meta: &mut PkgMetadata,
    warnings: &mut Vec<String>,
) -> std::io::Result<()> {
    f.seek(SeekFrom::Start(table_offset as u64))?;
    let mut buf = vec![0u8; (entry_count as usize) * 0x20];
    f.read_exact(&mut buf)?;

    let mut sfo: Option<(u32, u32)> = None; // (offset, size)
    let mut icon: Option<(u32, u32)> = None;

    for i in 0..(entry_count as usize) {
        let e = &buf[i * 0x20..(i + 1) * 0x20];
        let id = u32::from_be_bytes([e[0], e[1], e[2], e[3]]);
        let data_off = u32::from_be_bytes([e[0x10], e[0x11], e[0x12], e[0x13]]);
        let data_sz = u32::from_be_bytes([e[0x14], e[0x15], e[0x16], e[0x17]]);
        match id {
            ENTRY_PARAM_SFO => sfo = Some((data_off, data_sz)),
            ENTRY_ICON0_PNG => icon = Some((data_off, data_sz)),
            _ => {}
        }
    }

    if let Some((off, sz)) = sfo {
        if sz == 0 || sz > MAX_SFO_BYTES {
            warnings.push(format!("PARAM.SFO size {sz} out of range, skipping"));
        } else {
            f.seek(SeekFrom::Start(off as u64))?;
            let mut sfo_buf = vec![0u8; sz as usize];
            f.read_exact(&mut sfo_buf)?;
            if let Err(e) = parse_sfo_into(&sfo_buf, meta) {
                warnings.push(format!("PARAM.SFO parse: {e}"));
            }
        }
    } else {
        warnings.push("PKG has no PARAM.SFO entry".to_string());
    }

    if let Some((off, sz)) = icon {
        if sz == 0 || sz > MAX_ICON_BYTES {
            warnings.push(format!("ICON0.PNG size {sz} out of range, skipping"));
        } else {
            f.seek(SeekFrom::Start(off as u64))?;
            let mut png = vec![0u8; sz as usize];
            f.read_exact(&mut png)?;
            meta.icon_png_base64 = Some(b64_encode(&png));
        }
    }

    Ok(())
}

fn parse_sfo_into(buf: &[u8], meta: &mut PkgMetadata) -> Result<(), &'static str> {
    if buf.len() < 0x14 {
        return Err("SFO too small");
    }
    let magic = &buf[..4];
    if magic != b"\0PSF" {
        return Err("SFO magic mismatch");
    }
    let keys_off = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
    let data_off = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
    let n_entries = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]) as usize;
    if keys_off > buf.len() || data_off > buf.len() || n_entries > 256 {
        return Err("SFO offsets / count out of range");
    }
    let entries_start = 0x14;
    if entries_start + n_entries * 0x10 > buf.len() {
        return Err("SFO entry table truncated");
    }
    for i in 0..n_entries {
        let e = &buf[entries_start + i * 0x10..entries_start + (i + 1) * 0x10];
        let key_off = u16::from_le_bytes([e[0], e[1]]) as usize;
        let data_len = u32::from_le_bytes([e[4], e[5], e[6], e[7]]) as usize;
        let d_off = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;

        let key_abs = keys_off + key_off;
        if key_abs >= buf.len() {
            continue;
        }
        let key_end = buf[key_abs..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| key_abs + p)
            .unwrap_or(buf.len());
        let key = std::str::from_utf8(&buf[key_abs..key_end]).unwrap_or("");

        let data_abs = data_off + d_off;
        if data_abs + data_len > buf.len() {
            continue;
        }
        // Most string entries are NUL-terminated within `data_len`.
        let trimmed = &buf[data_abs..data_abs + data_len];
        let trimmed_end = trimmed
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(trimmed.len());
        let val = String::from_utf8_lossy(&trimmed[..trimmed_end]).to_string();

        match key {
            "TITLE" => meta.title = val,
            "TITLE_ID" => meta.title_id = val,
            "CATEGORY" => meta.category = val,
            "CONTENT_ID" if meta.content_id.is_empty() => meta.content_id = val,
            _ => {}
        }
    }
    Ok(())
}

/// Map PARAM.SFO `CATEGORY` values to BGFT's `package_type` string.
/// Cross-referenced from psdevwiki + community PS4/PS5 references.
/// Returns None for unknown categories so the UI can default-or-warn.
fn derive_package_type(category: &str) -> Option<String> {
    match category {
        "gd" => Some("PS4GD".to_string()),   // game (full)
        "gp" => Some("PS4DP".to_string()),   // patch / DLC
        "ac" => Some("PS4AC".to_string()),   // add-on content / DLC
        "gde" => Some("PS4GDE".to_string()), // extra
        "la" => Some("PS4LA".to_string()),   // launcher (educated guess)
        _ => None,
    }
}

/// Tiny base64 encoder so we don't pull in a crate just for icon
/// transport. Standard alphabet, no line breaks. ~30 lines.
fn b64_encode(input: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHA[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_magic_classifies_as_unknown() {
        // Synthesise a 256-byte buffer with non-PKG magic. We can't
        // call parse_pkg without a real file; the magic-handling code
        // path is exercised via integration test only.
        let magic: u32 = 0x7F464948; // \x7FFIH (the user's anomalous file)
        let bytes = magic.to_be_bytes();
        assert_eq!(bytes, [0x7F, 0x46, 0x49, 0x48]);
        // Sanity: confirms our format string matches the PS4/PS5 expected magic.
        assert_eq!(PKG_MAGIC.to_be_bytes(), [0x7F, 0x43, 0x4E, 0x54]);
    }

    #[test]
    fn package_type_derivation() {
        assert_eq!(derive_package_type("gd"), Some("PS4GD".into()));
        assert_eq!(derive_package_type("ac"), Some("PS4AC".into()));
        assert_eq!(derive_package_type(""), None);
        assert_eq!(derive_package_type("zz"), None);
    }

    #[test]
    fn b64_round_trip_simple() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn truncated_file_errors_cleanly() {
        let dir = tempdir();
        let path = dir.join("tiny.pkg");
        std::fs::write(&path, b"\x7FCNT").unwrap();
        let err = parse_pkg(&path).unwrap_err();
        assert!(matches!(err, PkgError::Truncated(4)));
    }

    #[test]
    fn unknown_magic_synthetic_file() {
        let dir = tempdir();
        let path = dir.join("unknown.pkg");
        // 256-byte buffer with the user's anomalous magic.
        let mut buf = vec![0u8; 256];
        buf[0..4].copy_from_slice(&[0x7F, 0x46, 0x49, 0x48]);
        std::fs::write(&path, &buf).unwrap();
        let meta = parse_pkg(&path).unwrap();
        match meta.kind {
            PkgKind::Unknown { magic_hex } => assert_eq!(magic_hex, "7F464948"),
            _ => panic!("expected Unknown kind"),
        }
        assert!(!meta.warnings.is_empty());
    }

    fn tempdir() -> std::path::PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("ps5upload-pkg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }
}
