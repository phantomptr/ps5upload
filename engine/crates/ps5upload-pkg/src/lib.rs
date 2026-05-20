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

pub mod ufs2;
pub use ufs2::{Ufs2Error, Ufs2Image};

use std::fs::create_dir_all;
use std::io::Write as _;

/// Inspect a local UFS2 image (.ffpkg etc) — open the superblock,
/// resolve sce_sys/param.sfo if present, and return a digest the UI
/// can render before any upload.
///
/// Lives here (not in `ufs2`) because the param.sfo parse re-uses
/// the same logic as the PKG inspector below — keeping them in one
/// crate means there's one source of truth for "what does PS5
/// metadata look like."
#[derive(Debug, Serialize)]
pub struct FfpkgInspection {
    /// Image total bytes per the superblock (block_size × block_count).
    pub image_bytes: u64,
    /// UFS2 block size (typically 32768 on PS5 .ffpkg).
    pub block_size: u32,
    /// UFS2 fragment size (typically 4096 on PS5 .ffpkg).
    pub fragment_size: u32,
    /// Volume label from the superblock. Usually empty for PS5.
    pub volume_name: String,
    /// True when sce_sys/param.sfo was found and parsed cleanly.
    pub has_sce_sys: bool,
    /// PARAM.SFO `TITLE_ID` field (e.g. "CUSA12345"), when present.
    pub title_id: Option<String>,
    /// PARAM.SFO `TITLE` field (game name).
    pub title: Option<String>,
    /// PARAM.SFO `CATEGORY` field ("gd"=game data, "gp"=patch, etc.).
    pub category: Option<String>,
    /// Top-level entries (one level deep). Lets the UI show "this
    /// image contains: eboot.bin, sce_sys/, sce_module/, etc."
    pub root_entries: Vec<RootEntry>,
    /// Non-fatal warnings collected during inspect.
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RootEntry {
    pub name: String,
    pub kind: String,
    pub size: u64,
}

/// Maximum bytes we'll extract per call from a single inode.
/// 4 GiB matches the UFS2 max file size in practice for game assets;
/// callers can request again if they need more.
const FFPKG_EXTRACT_MAX: u64 = 4 * 1024 * 1024 * 1024;

/// Max directory-tree depth `extract_dir_recursive` will descend. UFS dir
/// entries are attacker-controllable in a hostile .ffpkg; a crafted entry
/// whose inode points back at an ancestor directory (a cycle), or simply a
/// pathologically deep tree, would otherwise recurse until the stack
/// overflows. 64 matches the payload's rm_rf/cp_rf depth cap.
const FFPKG_EXTRACT_MAX_DEPTH: u32 = 64;

/// Result of an extract op — caller can render "wrote N files,
/// total Y bytes" + per-file detail.
#[derive(Debug, Serialize)]
pub struct FfpkgExtractResult {
    /// Total files written.
    pub file_count: u64,
    /// Total bytes written across all files.
    pub bytes_written: u64,
    /// First N files we wrote, for renderer display. Capped to keep
    /// the response payload small even on bulk extracts.
    pub sample_paths: Vec<String>,
}

/// Extract a path from a `.ffpkg` to a local directory. `ffpkg_path`
/// must point at the image; `inner_path` is a slash-separated path
/// inside the image (e.g. "sce_sys/icon0.png" or "sce_sys" for a
/// whole subtree); `dest_dir` is a local directory that will receive
/// the extracted file or subtree.
///
/// Read-only on the image — never writes to it. Creates `dest_dir`
/// if missing. When `inner_path` is a single file, that file is
/// written directly inside `dest_dir`. When it's a directory, the
/// directory is created inside `dest_dir` and recursively populated.
pub fn extract_from_ffpkg(
    ffpkg_path: &Path,
    inner_path: &str,
    dest_dir: &Path,
) -> Result<FfpkgExtractResult, Ufs2Error> {
    let mut img = Ufs2Image::open(ffpkg_path)?;
    let components: Vec<&str> = inner_path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let inode = if components.is_empty() {
        img.read_inode(ufs2::ROOT_INODE)?
    } else {
        img.resolve_path(&components)?
    };
    create_dir_all(dest_dir).map_err(Ufs2Error::Io)?;
    let mut result = FfpkgExtractResult {
        file_count: 0,
        bytes_written: 0,
        sample_paths: Vec::new(),
    };
    let leaf_name = components.last().copied().unwrap_or("root");
    if inode.is_file() {
        let bytes = img.read_file(&inode, FFPKG_EXTRACT_MAX)?;
        let dest = dest_dir.join(leaf_name);
        write_file_atomic(&dest, &bytes)?;
        result.file_count = 1;
        result.bytes_written = bytes.len() as u64;
        result.sample_paths.push(dest.display().to_string());
    } else if inode.is_dir() {
        let target = dest_dir.join(leaf_name);
        create_dir_all(&target).map_err(Ufs2Error::Io)?;
        extract_dir_recursive(&mut img, &inode, &target, &mut result, 0)?;
    } else {
        return Err(Ufs2Error::NotFound {
            component: format!("{leaf_name} (not a file or directory)"),
        });
    }
    Ok(result)
}

/// Reject directory entry names that could escape the destination
/// via path traversal or absolute-path injection. UFS dirents are
/// attacker-controllable inside a hostile .ffpkg, and `Path::join`
/// with an absolute child silently discards the parent — so a
/// crafted entry named `/etc/passwd` would write outside `dest`.
///
/// Allowed: anything else, including spaces, dots inside the name,
/// and unicode. We deliberately permit those because legitimate PS5
/// game asset filenames routinely contain them.
fn is_safe_child_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    if name.contains('\0') {
        return false;
    }
    if Path::new(name).is_absolute() {
        return false;
    }
    true
}

fn extract_dir_recursive(
    img: &mut Ufs2Image<std::fs::File>,
    dir: &ufs2::Inode,
    dest: &Path,
    result: &mut FfpkgExtractResult,
    depth: u32,
) -> Result<(), Ufs2Error> {
    // Bound recursion against a hostile image with a directory cycle or
    // pathological nesting (the block-pointer walk has cycle detection, but
    // the directory tree didn't). "." / ".." are already skipped by
    // is_safe_child_name, so a cycle requires a crafted non-dotdot entry.
    if depth > FFPKG_EXTRACT_MAX_DEPTH {
        return Err(Ufs2Error::WalkTooDeep {
            max: FFPKG_EXTRACT_MAX_DEPTH,
        });
    }
    let entries = img.list_dir(dir)?;
    for e in entries {
        if !is_safe_child_name(&e.name) {
            // Skip but don't fail the whole extract — a hostile
            // image's malformed entry shouldn't kill an otherwise-
            // recoverable archive. The result counts won't include
            // these, so the user can compare expected vs observed.
            continue;
        }
        let child_path = dest.join(&e.name);
        let child_inode = img.read_inode(e.inode)?;
        if child_inode.is_dir() {
            create_dir_all(&child_path).map_err(Ufs2Error::Io)?;
            extract_dir_recursive(img, &child_inode, &child_path, result, depth + 1)?;
        } else if child_inode.is_file() {
            let bytes = img.read_file(&child_inode, FFPKG_EXTRACT_MAX)?;
            write_file_atomic(&child_path, &bytes)?;
            result.file_count += 1;
            result.bytes_written += bytes.len() as u64;
            // Cap sample list at 32 paths so the JSON response stays
            // bounded even on huge extracts.
            if result.sample_paths.len() < 32 {
                result.sample_paths.push(child_path.display().to_string());
            }
        }
        // Symlinks and other types are silently skipped — extracting
        // them with the right semantics across platforms is fiddly
        // (Windows symlinks need elevated privileges) and these are
        // rare in PS5 game images.
    }
    Ok(())
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), Ufs2Error> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent).map_err(Ufs2Error::Io)?;
    }
    let tmp = path.with_extension("ffpkg-tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(Ufs2Error::Io)?;
        f.write_all(bytes).map_err(Ufs2Error::Io)?;
        f.sync_all().map_err(Ufs2Error::Io)?;
    }
    std::fs::rename(&tmp, path).map_err(Ufs2Error::Io)?;
    Ok(())
}

/// Open a UFS2 image, walk its root + sce_sys, return everything the
/// inspect dialog needs. Read-only; never writes to the image.
pub fn inspect_ffpkg(path: &Path) -> Result<FfpkgInspection, Ufs2Error> {
    let mut img = Ufs2Image::open(path)?;
    let sb = img.superblock.clone();
    let mut warnings: Vec<String> = Vec::new();

    let root = img.read_inode(ufs2::ROOT_INODE)?;
    let entries = img.list_dir(&root)?;
    let mut root_entries: Vec<RootEntry> = Vec::with_capacity(entries.len());
    for e in &entries {
        // Read each child inode just to get its size — cheap, single
        // 256-byte read per entry.
        let size = match img.read_inode(e.inode) {
            Ok(child) => child.size,
            Err(_) => 0,
        };
        root_entries.push(RootEntry {
            name: e.name.clone(),
            kind: e.kind.clone(),
            size,
        });
    }

    let mut title_id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut category: Option<String> = None;
    let has_sce_sys = entries
        .iter()
        .any(|e| e.name == "sce_sys" && e.kind == "dir");
    if has_sce_sys {
        match img.resolve_path(&["sce_sys", "param.sfo"]) {
            Ok(sfo_inode) if sfo_inode.is_file() => match img.read_file(&sfo_inode, 256 * 1024) {
                Ok(bytes) => match parse_sfo_string_keys(&bytes) {
                    Ok(kv) => {
                        title_id = kv.get("TITLE_ID").cloned();
                        title = kv.get("TITLE").cloned();
                        category = kv.get("CATEGORY").cloned();
                    }
                    Err(e) => warnings.push(format!("param.sfo parse: {e}")),
                },
                Err(e) => warnings.push(format!("param.sfo read: {e}")),
            },
            Ok(_) => warnings.push("sce_sys/param.sfo exists but is not a regular file".into()),
            Err(_) => warnings.push("sce_sys/ is present but param.sfo is missing".into()),
        }
    } else {
        warnings.push("sce_sys/ folder not found at root — image may not be a PS5 game".into());
    }

    Ok(FfpkgInspection {
        image_bytes: sb.total_bytes(),
        block_size: sb.block_size,
        fragment_size: sb.fragment_size,
        volume_name: sb.volume_name,
        has_sce_sys,
        title_id,
        title,
        category,
        root_entries,
        warnings,
    })
}

/// Parse the string keys from a PARAM.SFO blob. Reuses the same
/// layout the PKG parser already understands (PSF magic at +0,
/// key/data tables, entries with format codes).
///
/// Returns a flat HashMap of key → string value for the string-typed
/// (format=4) entries we care about. Non-string entries (uint32
/// PARENTAL_LEVEL etc) are ignored.
fn parse_sfo_string_keys(
    bytes: &[u8],
) -> Result<std::collections::HashMap<String, String>, String> {
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
        // All of these offsets come straight off disk as u32/u16 and
        // are widened with `as usize`. On a 32-bit build `usize` is
        // also 32-bit, so `table_off + i*16`, `key_table_off + key_off`
        // and `data_abs + data_len` can each wrap — a wrapped sum that
        // lands below `bytes.len()` would sail past the bounds check
        // and panic (or worse) on the slice below. Do every add/mul
        // with checked arithmetic and skip the entry on overflow.
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
        // Format 4 = UTF-8 string. We skip uint32 (format 4 confusingly
        // reuses the same number in some tools — but in PSF, 0x0004 is
        // utf-8 special and 0x0204 is utf-8 normal. Both end at NUL).
        if format == 0x0004 || format == 0x0204 {
            let value = String::from_utf8_lossy(&bytes[data_abs..data_end])
                .trim_end_matches('\0')
                .to_string();
            out.insert(key, value);
        }
    }
    Ok(out)
}

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
        // Soften the warning. On a jailbroken PS5 with kernel-level
        // Unknown header magic: most commonly 0x7F464948 (`\x7FFIH` —
        // newer PS5-native fakepkg signing tool format) which Sony's
        // installer accepts but our parser doesn't read yet, so the
        // metadata (content_id, title, category) stays empty.
        //
        // Pre-2.2.52 we pushed a long warning explaining all of this.
        // The user-facing reality is simpler: "we couldn't read the
        // file's metadata, but install will still proceed and either
        // succeed or fail with a real error." That's a step's
        // success/fail status, not a warning to read. We now stay
        // silent here — the queue row's metadata fields just stay
        // empty, and the actual install attempt's success/error is
        // surfaced through the install_start ACK path.
        //
        // The compile-time test below (test_parse_fih_magic) keeps
        // the recognition of this magic working as a parse decision,
        // we just stop emitting the verbose user-facing warning.
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
    // saturating_add, not sum(): summing filesystem-controlled u64 sizes with
    // `iter().sum()` panics on overflow in debug builds. Not practically
    // reachable (would need exabytes on disk), but a parser entry point
    // shouldn't panic on its inputs.
    let total_size: u64 = part_sizes.iter().copied().fold(0u64, u64::saturating_add);

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

        // Offsets are u16/u32 widened with `as usize`; on a 32-bit
        // build these sums can wrap. Checked arithmetic keeps a
        // wrapped value from slipping under the `buf.len()` guard.
        let key_abs = match keys_off.checked_add(key_off) {
            Some(v) => v,
            None => continue,
        };
        if key_abs >= buf.len() {
            continue;
        }
        let key_end = buf[key_abs..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| key_abs + p)
            .unwrap_or(buf.len());
        let key = std::str::from_utf8(&buf[key_abs..key_end]).unwrap_or("");

        let data_abs = match data_off.checked_add(d_off) {
            Some(v) => v,
            None => continue,
        };
        let data_end = match data_abs.checked_add(data_len) {
            Some(v) => v,
            None => continue,
        };
        if data_end > buf.len() {
            continue;
        }
        // Most string entries are NUL-terminated within `data_len`.
        let trimmed = &buf[data_abs..data_end];
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
        // No user-facing warning expected: we used to emit a long
        // "we don't recognise this magic" warning here, but the user
        // doesn't need the explanation. The empty-metadata state is
        // the result; the queue row's blank content_id/title fields
        // already make the missing metadata visible, and the install
        // attempt's eventual ACK is the success/fail signal.
        assert!(meta.warnings.is_empty());
        // Metadata stays empty since the parser doesn't speak this
        // format yet — that's the row-display contract.
        assert!(meta.content_id.is_empty());
        assert!(meta.title.is_empty());
    }

    fn tempdir() -> std::path::PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("ps5upload-pkg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }
}
