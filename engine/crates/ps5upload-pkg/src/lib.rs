//! PS4/PS5 PKG header parser.
//!
//! Parses the unencrypted header of a `.pkg` file to extract metadata
//! the install UI needs: content_id, title (from PARAM.SFO), category,
//! icon (from ICON0.PNG entry), and total size. We never touch the
//! encrypted body — Sony's BGFT installer on the PS5 owns decryption
//! using device keys.
//!
//! PS4 packages are `\x7FCNT` containers. PS5 installable packages are
//! finalized `\x7FFIH` images which point to an embedded `\x7FCNT` metadata
//! container. Container shape is deliberately kept separate from signing:
//! a CNT magic alone does not prove that a package is Sony-retail signed.

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
    // Windows drive-relative names like "C:evil.txt" are NOT absolute
    // (Path::is_absolute() is false — there's a drive prefix but no root),
    // yet PathBuf::join with a drive-prefixed component *replaces* the
    // whole path, so dest.join("C:evil.txt") escapes `dest` on Windows.
    // A colon never legitimately appears in a PS5/UFS asset filename.
    if name.contains(':') {
        return false;
    }
    // Belt-and-suspenders, cross-platform: the name must resolve to
    // exactly one ordinary path component. This rejects any Prefix,
    // RootDir, CurDir, or ParentDir component on every host, not just
    // the specific cases enumerated above.
    let mut comps = Path::new(name).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
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

/// Magic bytes of a CNT package/container: `\x7FCNT`.
pub const PKG_MAGIC: u32 = 0x7F434E54;

/// Magic bytes of a PS5 finalized install image (`\x7FFIH`). Its envelope
/// points to an embedded CNT carrying the package metadata.
pub const PKG_MAGIC_FIH: u32 = 0x7F464948;

/// Classify a package's target platform for UI badging. `\x7FFIH` is
/// PS5-native. For stock `\x7FCNT` packages the title-id prefix is the
/// practical discriminator (CUSA = PS4, PPSA = PS5); falls back to the
/// content-id's middle token when no SFO title-id was parsed. Returns
/// `"ps4"`, `"ps5"`, or `""` (unknown — don't badge).
pub fn derive_platform(magic: u32, content_id: &str, title_id: &str) -> String {
    if magic == PKG_MAGIC_FIH {
        return "ps5".to_string();
    }
    // Prefer the SFO title-id; else the middle token of the content-id
    // (`EP4293-CUSA32097_00-…` → `CUSA32097`).
    let token = if !title_id.is_empty() {
        title_id
    } else {
        content_id.split('-').nth(1).unwrap_or("")
    };
    let prefix: String = token.chars().take(4).collect();
    match prefix.as_str() {
        "CUSA" => "ps4".to_string(),
        "PPSA" => "ps5".to_string(),
        _ => String::new(),
    }
}

/// Recover a PS4/PS5 title id (`CUSA#####` / `PPSA#####`) embedded in a
/// package's filename, e.g. `[SPSX]-Bloodborne…-CUSA00900-USA-Game-PS4.pkg`
/// → `CUSA00900`. Scene release names almost always carry the id, so this
/// lets a drive scan classify a package's platform from the name alone —
/// no per-file header read over the (slow) console RPC. Returns the first
/// token shaped `AAAA#####` (4 uppercase letters + 5 digits).
pub fn title_id_from_filename(name: &str) -> Option<String> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|tok| {
            tok.len() == 9
                && tok.as_bytes()[..4].iter().all(u8::is_ascii_uppercase)
                && tok.as_bytes()[4..].iter().all(u8::is_ascii_digit)
        })
        .map(|s| s.to_string())
}

/// PARAM.SFO entry id inside a PKG.
const ENTRY_PARAM_SFO: u32 = 0x1000;
/// PARAM.JSON entry id inside a PS5 CNT metadata container.
const ENTRY_PARAM_JSON: u32 = 0x2000;
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
    /// A `\x7FCNT` metadata container. This describes the container layout,
    /// not whether its cryptographic material is retail or fake/debug.
    #[serde(alias = "standard")]
    CntContainer,
    /// A complete PS5 finalized image (`\x7FFIH`) with an embedded CNT.
    Ps5Finalized {
        signed_byte: u8,
        format_version: u16,
    },
    /// Magic byte sequence we don't recognise. It may be a license-only
    /// artifact, an unsupported package variant, or simply the wrong file.
    /// The install UI may still permit an attempt; Sony's installer remains
    /// the final authority on formats it accepts.
    Unknown { magic_hex: String },
}

/// What the unencrypted package envelope can prove about signing. PS4 CNT
/// packages need a deeper cryptographic probe, so they intentionally remain
/// unknown instead of being mislabeled "retail" merely from `\x7FCNT`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PkgAuthenticity {
    FakeDebug,
    Retail,
    #[default]
    Unknown,
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
    /// Signing class proven by the package envelope.
    pub authenticity: PkgAuthenticity,
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
    /// PARAM.SFO `APP_VER` field — the application version this package
    /// brings, e.g. `01.04`. For a patch (`gp`) this is the authoritative
    /// "which update is this" answer (a patch shares the base game's
    /// content_id and TITLE, so nothing else distinguishes versions). Empty
    /// when the SFO is missing or has no APP_VER key.
    pub app_ver: String,
    /// Stable, inexpensive identity for this exact package variant. This is a
    /// BLAKE3 digest over the file size plus bounded samples from the beginning
    /// and end of the pkg. It deliberately differs for two patches that share
    /// the same ContentID/category/APP_VER (for example an optional fix and a
    /// backport), without making the UI hash a multi-gigabyte pkg before an
    /// upload can start.
    pub fingerprint: String,
    /// Mapping of category/platform to BGFT's package_type string.
    pub package_type: Option<String>,
    /// Target platform for UI badging: `"ps4"`, `"ps5"`, or `""` (unknown).
    /// Derived from the header magic (`\x7FFIH` = PS5) and the title-id
    /// prefix (CUSA = PS4, PPSA = PS5). See [`derive_platform`].
    pub platform: String,
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

    let fingerprint = package_fingerprint_from_reader(size, |offset, len| {
        let mut buf = vec![0u8; len as usize];
        f.seek(SeekFrom::Start(offset)).ok()?;
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    })
    .unwrap_or_default();

    f.seek(SeekFrom::Start(0))?;
    let mut head = [0u8; 0xA0];
    f.read_exact(&mut head)?;

    let magic = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let mut warnings = Vec::new();

    let mut meta = PkgMetadata {
        path: path.to_path_buf(),
        size,
        kind: PkgKind::CntContainer,
        authenticity: PkgAuthenticity::Unknown,
        content_id: String::new(),
        title: String::new(),
        title_id: String::new(),
        category: String::new(),
        app_ver: String::new(),
        fingerprint,
        package_type: None,
        platform: String::new(),
        icon_png_base64: None,
        warnings: Vec::new(),
    };

    let (container_base, container_head) = if magic == PKG_MAGIC_FIH {
        // Finalized PS5 image fields are little-endian. The embedded CNT
        // offset lives at +0x58 and carries the normal big-endian metadata
        // header/table. A signed byte of 0x00 is debug/fake; 0x80 is retail.
        let signed_byte = head[0x05];
        let format_version = u16::from_le_bytes([head[0x06], head[0x07]]);
        let cnt_base = u64::from_le_bytes([
            head[0x58], head[0x59], head[0x5A], head[0x5B], head[0x5C], head[0x5D], head[0x5E],
            head[0x5F],
        ]);
        meta.kind = PkgKind::Ps5Finalized {
            signed_byte,
            format_version,
        };
        meta.authenticity = match signed_byte {
            0x00 => PkgAuthenticity::FakeDebug,
            0x80 => PkgAuthenticity::Retail,
            _ => {
                warnings.push(format!(
                    "PS5 FIH signed byte 0x{signed_byte:02X} is not a recognised debug/retail value"
                ));
                PkgAuthenticity::Unknown
            }
        };
        meta.platform = "ps5".to_string();
        if format_version != 3 {
            warnings.push(format!(
                "PS5 FIH format version {format_version} is not the supported version 3"
            ));
        }
        if cnt_base == 0 || cnt_base.checked_add(0xA0).is_none_or(|end| end > size) {
            return Err(PkgError::Header("PS5 FIH embedded CNT offset out of range"));
        }
        f.seek(SeekFrom::Start(cnt_base))?;
        let mut cnt = [0u8; 0xA0];
        f.read_exact(&mut cnt)?;
        if u32::from_be_bytes([cnt[0], cnt[1], cnt[2], cnt[3]]) != PKG_MAGIC {
            return Err(PkgError::Header("PS5 FIH embedded CNT magic mismatch"));
        }
        (cnt_base, cnt)
    } else if magic == PKG_MAGIC {
        (0, head)
    } else {
        meta.kind = PkgKind::Unknown {
            magic_hex: format!("{magic:08X}"),
        };
        meta.platform = derive_platform(magic, "", "");
        warnings.push(format!("unrecognized package magic {magic:08X}"));
        meta.warnings = warnings;
        return Ok(meta);
    };

    let entry_count = u32::from_be_bytes([
        container_head[0x10],
        container_head[0x11],
        container_head[0x12],
        container_head[0x13],
    ]);
    let table_offset = u32::from_be_bytes([
        container_head[0x18],
        container_head[0x19],
        container_head[0x1A],
        container_head[0x1B],
    ]);
    let content_flags = u32::from_be_bytes([
        container_head[0x78],
        container_head[0x79],
        container_head[0x7A],
        container_head[0x7B],
    ]);
    // content_id is at 0x40, 36 bytes ASCII with trailing NULs.
    let cid_raw = &container_head[0x40..0x40 + 36];
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
    if let Err(e) = walk_entries(
        &mut f,
        container_base,
        table_offset,
        entry_count,
        &mut meta,
        &mut warnings,
    ) {
        warnings.push(format!("entry table walk failed: {e}"));
    }

    if magic == PKG_MAGIC_FIH {
        // PS5 patch kind is encoded in CNT content flags even if param.json is
        // absent/encrypted. Preserve the familiar gd/gp category contract for
        // the rest of the app and never default a PS5 package to PS4GD.
        let is_patch = (content_flags & 0x0010_0000) != 0 || (content_flags & 0x4000_0000) != 0;
        if meta.category.is_empty() {
            meta.category = if is_patch { "gp" } else { "gd" }.to_string();
        }
        meta.package_type = Some(if is_patch { "PS5DP" } else { "PS5GD" }.to_string());
    } else {
        meta.package_type = derive_package_type(&meta.category);
        meta.platform = derive_platform(magic, &meta.content_id, &meta.title_id);
    }
    meta.warnings = warnings;
    Ok(meta)
}

/// Bytes sampled at each end of a pkg for [`package_fingerprint_from_reader`].
/// 64 KiB covers the complete package header/entry table in normal packages,
/// while keeping an on-console verification to two small ranged reads.
pub const PACKAGE_FINGERPRINT_SAMPLE_BYTES: u64 = 64 * 1024;

/// Compute a stable package-variant identity using bounded ranged reads.
///
/// ContentID, CATEGORY and APP_VER are not unique: repacks/backports can share
/// all three. The size plus first/last 64 KiB is enough to distinguish those
/// artifacts in practice and is cryptographically bound with BLAKE3. This is
/// an identity/deduplication fingerprint, not a promise that every byte in the
/// middle was integrity-checked; callers needing a full-file audit should use
/// the existing FS_HASH/BLAKE3 operation.
pub fn package_fingerprint_from_reader<F>(size: u64, mut read_at: F) -> Option<String>
where
    F: FnMut(u64, u64) -> Option<Vec<u8>>,
{
    if size == 0 {
        return None;
    }
    let prefix_len = size.min(PACKAGE_FINGERPRINT_SAMPLE_BYTES);
    let prefix = read_at(0, prefix_len)?;
    if prefix.len() as u64 != prefix_len {
        return None;
    }

    let suffix_len = size.min(PACKAGE_FINGERPRINT_SAMPLE_BYTES);
    let suffix_offset = size.saturating_sub(suffix_len);
    // Small packages have one overlapping sample. Avoid reading and hashing it
    // twice, while retaining the offset/length domain separators below.
    let suffix = if suffix_offset == 0 {
        Vec::new()
    } else {
        let bytes = read_at(suffix_offset, suffix_len)?;
        if bytes.len() as u64 != suffix_len {
            return None;
        }
        bytes
    };

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ps5upload-pkg-fingerprint-v1\0");
    hasher.update(&size.to_le_bytes());
    hasher.update(&prefix_len.to_le_bytes());
    hasher.update(&prefix);
    hasher.update(&suffix_offset.to_le_bytes());
    hasher.update(&(suffix.len() as u64).to_le_bytes());
    hasher.update(&suffix);
    Some(hasher.finalize().to_hex().to_string())
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
    container_base: u64,
    table_offset: u32,
    entry_count: u32,
    meta: &mut PkgMetadata,
    warnings: &mut Vec<String>,
) -> std::io::Result<()> {
    f.seek(SeekFrom::Start(container_base + table_offset as u64))?;
    let mut buf = vec![0u8; (entry_count as usize) * 0x20];
    f.read_exact(&mut buf)?;

    let mut sfo: Option<(u32, u32)> = None; // (offset, size)
    let mut param_json: Option<(u32, u32)> = None;
    let mut icon: Option<(u32, u32)> = None;

    for i in 0..(entry_count as usize) {
        let e = &buf[i * 0x20..(i + 1) * 0x20];
        let id = u32::from_be_bytes([e[0], e[1], e[2], e[3]]);
        let data_off = u32::from_be_bytes([e[0x10], e[0x11], e[0x12], e[0x13]]);
        let data_sz = u32::from_be_bytes([e[0x14], e[0x15], e[0x16], e[0x17]]);
        match id {
            ENTRY_PARAM_SFO => sfo = Some((data_off, data_sz)),
            ENTRY_PARAM_JSON => param_json = Some((data_off, data_sz)),
            ENTRY_ICON0_PNG => icon = Some((data_off, data_sz)),
            _ => {}
        }
    }

    if let Some((off, sz)) = sfo {
        if sz == 0 || sz > MAX_SFO_BYTES {
            warnings.push(format!("PARAM.SFO size {sz} out of range, skipping"));
        } else {
            f.seek(SeekFrom::Start(container_base + off as u64))?;
            let mut sfo_buf = vec![0u8; sz as usize];
            f.read_exact(&mut sfo_buf)?;
            if let Err(e) = parse_sfo_into(&sfo_buf, meta) {
                warnings.push(format!("PARAM.SFO parse: {e}"));
            }
        }
    } else if param_json.is_none() {
        warnings.push("PKG has no PARAM.SFO entry".to_string());
    }

    if let Some((off, sz)) = param_json {
        if sz == 0 || sz > MAX_SFO_BYTES {
            warnings.push(format!("PARAM.JSON size {sz} out of range, skipping"));
        } else {
            f.seek(SeekFrom::Start(container_base + off as u64))?;
            let mut json = vec![0u8; sz as usize];
            f.read_exact(&mut json)?;
            if let Err(e) = parse_param_json_into(&json, meta) {
                warnings.push(format!("PARAM.JSON parse: {e}"));
            }
        }
    }

    if let Some((off, sz)) = icon {
        if sz == 0 || sz > MAX_ICON_BYTES {
            warnings.push(format!("ICON0.PNG size {sz} out of range, skipping"));
        } else {
            f.seek(SeekFrom::Start(container_base + off as u64))?;
            let mut png = vec![0u8; sz as usize];
            f.read_exact(&mut png)?;
            meta.icon_png_base64 = Some(b64_encode(&png));
        }
    }

    Ok(())
}

fn parse_param_json_into(buf: &[u8], meta: &mut PkgMetadata) -> Result<(), &'static str> {
    let value = parse_param_json_value(buf)?;
    apply_param_json(
        &value,
        &mut meta.content_id,
        &mut meta.title,
        &mut meta.title_id,
        &mut meta.app_ver,
    );
    Ok(())
}

fn parse_param_json_value(buf: &[u8]) -> Result<serde_json::Value, &'static str> {
    // Package metadata entries are commonly NUL-padded to an alignment
    // boundary. serde_json correctly rejects that padding, so trim only the
    // trailing zero bytes before parsing the otherwise exact entry payload.
    let end = buf.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    serde_json::from_slice(&buf[..end]).map_err(|_| "invalid JSON")
}

fn apply_param_json(
    value: &serde_json::Value,
    content_id: &mut String,
    title: &mut String,
    title_id: &mut String,
    app_ver: &mut String,
) {
    let string = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
    if let Some(v) = string("contentId") {
        *content_id = v.to_string();
    }
    if let Some(v) = string("titleId") {
        *title_id = v.to_string();
    }
    if let Some(v) = string("contentVersion").or_else(|| string("masterVersion")) {
        *app_ver = v.to_string();
    }
    if let Some(localized) = value
        .get("localizedParameters")
        .and_then(serde_json::Value::as_object)
    {
        let preferred = localized
            .get("defaultLanguage")
            .and_then(serde_json::Value::as_str)
            .and_then(|lang| localized.get(lang));
        let title_name = preferred
            .and_then(|v| v.get("titleName"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                localized
                    .values()
                    .find_map(|v| v.get("titleName").and_then(serde_json::Value::as_str))
            });
        if let Some(v) = title_name {
            *title = v.to_string();
        }
    }
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
            "APP_VER" => meta.app_ver = val,
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

/// Public wrapper over the category → BGFT `package_type` map.
pub fn package_type_for_category(category: &str) -> Option<String> {
    package_type_for_category_and_platform(category, "ps4")
}

/// Map a metadata category to the platform-specific BGFT package type.
/// Unknown platforms retain the established PS4 default for compatibility.
pub fn package_type_for_category_and_platform(category: &str, platform: &str) -> Option<String> {
    let prefix = if platform == "ps5" { "PS5" } else { "PS4" };
    match category {
        "gd" => Some(format!("{prefix}GD")),
        "gp" => Some(format!("{prefix}DP")),
        "ac" => Some(format!("{prefix}AC")),
        "gde" => Some(format!("{prefix}GDE")),
        "la" => Some(format!("{prefix}LA")),
        _ => None,
    }
}

/// Extract the PARAM.SFO `CATEGORY` from a `.pkg` using a ranged-read closure
/// instead of a local `File`. `read_at(offset, len)` returns the bytes (or
/// `None` on failure), so this works against a pkg that lives on the PS5 (via
/// `fs_read`) — letting the engine learn whether a STAGED pkg is a patch
/// (`gp`) or a full game (`gd`) for the data-loss guard, without re-parsing the
/// whole file. Only three small reads: header (0x40), the entry table, and the
/// SFO itself. For a PS5 FIH image, patch/base comes from the embedded CNT's
/// content flags, so the destructive-patch guard also works for staged FPKGs.
pub fn category_from_reader<F>(read_at: F) -> Option<String>
where
    F: Fn(u64, u64) -> Option<Vec<u8>>,
{
    let outer = read_at(0, 0x60)?;
    if outer.len() < 0x60 {
        return None;
    }
    let outer_magic = u32::from_be_bytes([outer[0], outer[1], outer[2], outer[3]]);
    let container_base = if outer_magic == PKG_MAGIC_FIH {
        if u16::from_le_bytes([outer[6], outer[7]]) != 3 {
            return None;
        }
        u64::from_le_bytes(outer[0x58..0x60].try_into().ok()?)
    } else if outer_magic == PKG_MAGIC {
        0
    } else {
        return None;
    };
    let head = if container_base == 0 {
        outer
    } else {
        read_at(container_base, 0xA0)?
    };
    if head.len() < 0x1C {
        return None;
    }
    let magic = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    if magic != PKG_MAGIC {
        return None;
    }
    if outer_magic == PKG_MAGIC_FIH {
        if head.len() < 0x7C {
            return None;
        }
        let flags = u32::from_be_bytes(head[0x78..0x7C].try_into().ok()?);
        let is_patch = (flags & 0x0010_0000) != 0 || (flags & 0x4000_0000) != 0;
        return Some(if is_patch { "gp" } else { "gd" }.to_string());
    }
    let entry_count = u32::from_be_bytes([head[0x10], head[0x11], head[0x12], head[0x13]]);
    let table_offset = u32::from_be_bytes([head[0x18], head[0x19], head[0x1A], head[0x1B]]);
    if entry_count == 0 || entry_count > 1024 {
        return None;
    }
    let table = read_at(
        container_base + table_offset as u64,
        entry_count as u64 * 0x20,
    )?;
    let mut sfo: Option<(u32, u32)> = None;
    for i in 0..entry_count as usize {
        let e = table.get(i * 0x20..(i + 1) * 0x20)?;
        if u32::from_be_bytes([e[0], e[1], e[2], e[3]]) == ENTRY_PARAM_SFO {
            let off = u32::from_be_bytes([e[0x10], e[0x11], e[0x12], e[0x13]]);
            let sz = u32::from_be_bytes([e[0x14], e[0x15], e[0x16], e[0x17]]);
            sfo = Some((off, sz));
            break;
        }
    }
    let (off, sz) = sfo?;
    if sz == 0 || sz > MAX_SFO_BYTES {
        return None;
    }
    let sfo_bytes = read_at(container_base + off as u64, sz as u64)?;
    parse_sfo_string_keys(&sfo_bytes)
        .ok()?
        .get("CATEGORY")
        .cloned()
}

/// The subset of [`PkgMetadata`] recoverable from ranged reads — enough to
/// enrich a listing row without parsing the whole file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReaderMetadata {
    pub content_id: String,
    pub title: String,
    pub title_id: String,
    /// PARAM.SFO CATEGORY (`gd`/`gp`/`ac`…).
    pub category: String,
    /// PARAM.SFO APP_VER (`01.04`).
    pub app_ver: String,
    /// `"ps4"` | `"ps5"` | `""`.
    pub platform: String,
    /// Signing class proven by the outer envelope.
    pub authenticity: PkgAuthenticity,
    /// Optional sampled artifact identity. `metadata_from_reader` leaves this
    /// empty because it does not know the total file size; remote callers that
    /// do know it can populate the field with
    /// [`package_fingerprint_from_reader`].
    pub fingerprint: String,
}

/// Title id from a content id's second dash segment, e.g.
/// `UP9000-CUSA07842_00-…` → `CUSA07842`. Self-contained fallback for when the
/// SFO has no `TITLE_ID` (kept here so the pkg crate needn't depend on core).
fn title_id_from_content_id_str(content_id: &str) -> String {
    content_id
        .split('-')
        .nth(1)
        .and_then(|seg| seg.split('_').next())
        .filter(|id| {
            id.len() == 9
                && id[..4].bytes().all(|b| b.is_ascii_uppercase())
                && id[4..].bytes().all(|b| b.is_ascii_digit())
        })
        .unwrap_or("")
        .to_string()
}

/// Parse a pkg's content id + PARAM.SFO fields (title, category, APP_VER) from
/// ranged reads. Like [`category_from_reader`] this works against a pkg that
/// lives on the PS5 (via `fs_read`) — used to lazily enrich the External
/// Packages listing with the authoritative title/version/category the fast,
/// filename-based scan deliberately skips. Supports both top-level CNT and a
/// PS5 FIH image's embedded CNT. SFO/param.json fields are best-effort.
pub fn metadata_from_reader<F>(read_at: F) -> Option<ReaderMetadata>
where
    F: Fn(u64, u64) -> Option<Vec<u8>>,
{
    let outer = read_at(0, 0xA0)?;
    if outer.len() < 0x60 {
        return None;
    }
    let outer_magic = u32::from_be_bytes([outer[0], outer[1], outer[2], outer[3]]);
    let outer_signed_byte = outer[0x05];
    let container_base = if outer_magic == PKG_MAGIC_FIH {
        if u16::from_le_bytes([outer[6], outer[7]]) != 3 {
            return None;
        }
        u64::from_le_bytes(outer[0x58..0x60].try_into().ok()?)
    } else if outer_magic == PKG_MAGIC {
        0
    } else {
        return None;
    };
    let head = if container_base == 0 {
        outer
    } else {
        read_at(container_base, 0xA0)?
    };
    if head.len() < 0x1C {
        return None;
    }
    let magic = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    if magic != PKG_MAGIC {
        return None;
    }
    let mut content_id = if head.len() >= 0x40 + 36 {
        let raw = &head[0x40..0x40 + 36];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(36);
        String::from_utf8_lossy(&raw[..end]).trim().to_string()
    } else {
        String::new()
    };

    let entry_count = u32::from_be_bytes([head[0x10], head[0x11], head[0x12], head[0x13]]);
    let table_offset = u32::from_be_bytes([head[0x18], head[0x19], head[0x1A], head[0x1B]]);
    let (mut title, mut title_id, mut category, mut app_ver) =
        (String::new(), String::new(), String::new(), String::new());
    if entry_count > 0 && entry_count <= 1024 {
        if let Some(table) = read_at(
            container_base + table_offset as u64,
            entry_count as u64 * 0x20,
        ) {
            let mut sfo: Option<(u32, u32)> = None;
            let mut param_json: Option<(u32, u32)> = None;
            for i in 0..entry_count as usize {
                if let Some(e) = table.get(i * 0x20..(i + 1) * 0x20) {
                    if u32::from_be_bytes([e[0], e[1], e[2], e[3]]) == ENTRY_PARAM_SFO {
                        let off = u32::from_be_bytes([e[0x10], e[0x11], e[0x12], e[0x13]]);
                        let sz = u32::from_be_bytes([e[0x14], e[0x15], e[0x16], e[0x17]]);
                        sfo = Some((off, sz));
                    } else if u32::from_be_bytes([e[0], e[1], e[2], e[3]]) == ENTRY_PARAM_JSON {
                        let off = u32::from_be_bytes([e[0x10], e[0x11], e[0x12], e[0x13]]);
                        let sz = u32::from_be_bytes([e[0x14], e[0x15], e[0x16], e[0x17]]);
                        param_json = Some((off, sz));
                    }
                }
            }
            if let Some((off, sz)) = sfo {
                if sz > 0 && sz <= MAX_SFO_BYTES {
                    if let Some(sfo_bytes) = read_at(container_base + off as u64, sz as u64) {
                        if let Ok(kv) = parse_sfo_string_keys(&sfo_bytes) {
                            title = kv.get("TITLE").cloned().unwrap_or_default();
                            title_id = kv.get("TITLE_ID").cloned().unwrap_or_default();
                            category = kv.get("CATEGORY").cloned().unwrap_or_default();
                            app_ver = kv.get("APP_VER").cloned().unwrap_or_default();
                        }
                    }
                }
            }
            if let Some((off, sz)) = param_json {
                if sz > 0 && sz <= MAX_SFO_BYTES {
                    if let Some(bytes) = read_at(container_base + off as u64, sz as u64) {
                        if let Ok(v) = parse_param_json_value(&bytes) {
                            apply_param_json(
                                &v,
                                &mut content_id,
                                &mut title,
                                &mut title_id,
                                &mut app_ver,
                            );
                        }
                    }
                }
            }
        }
    }
    if outer_magic == PKG_MAGIC_FIH && category.is_empty() && head.len() >= 0x7C {
        let flags = u32::from_be_bytes(head[0x78..0x7C].try_into().ok()?);
        category = if (flags & 0x0010_0000) != 0 || (flags & 0x4000_0000) != 0 {
            "gp"
        } else {
            "gd"
        }
        .to_string();
    }
    if title_id.is_empty() {
        title_id = title_id_from_content_id_str(&content_id);
    }
    let platform = derive_platform(outer_magic, &content_id, &title_id);
    let authenticity = if outer_magic == PKG_MAGIC_FIH {
        match outer_signed_byte {
            0x00 => PkgAuthenticity::FakeDebug,
            0x80 => PkgAuthenticity::Retail,
            _ => PkgAuthenticity::Unknown,
        }
    } else {
        PkgAuthenticity::Unknown
    };
    Some(ReaderMetadata {
        content_id,
        title,
        title_id,
        category,
        app_ver,
        platform,
        authenticity,
        fingerprint: String::new(),
    })
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
    fn platform_from_magic_and_title_id() {
        // \x7FFIH magic is always PS5-native, even with no ids.
        assert_eq!(derive_platform(PKG_MAGIC_FIH, "", ""), "ps5");
        // Stock \x7FCNT: title-id prefix decides.
        assert_eq!(derive_platform(PKG_MAGIC, "", "CUSA32097"), "ps4");
        assert_eq!(derive_platform(PKG_MAGIC, "", "PPSA01650"), "ps5");
        // Falls back to the content-id middle token when no title-id.
        assert_eq!(
            derive_platform(PKG_MAGIC, "EP4293-CUSA32097_00-ASTNCPS4SIEE0000", ""),
            "ps4"
        );
        assert_eq!(
            derive_platform(PKG_MAGIC, "UP0000-PPSA01650_00-YOUTUBE000000000", ""),
            "ps5"
        );
        // Unknown prefixes (NPXS system, homebrew) → no badge.
        assert_eq!(derive_platform(PKG_MAGIC, "", "NPXS40047"), "");
        assert_eq!(derive_platform(PKG_MAGIC, "", ""), "");
    }

    // Build a minimal PARAM.SFO carrying one CATEGORY string key.
    fn build_sfo(category: &str) -> Vec<u8> {
        let key = b"CATEGORY\0";
        let val = format!("{category}\0");
        let key_table_off = 0x14usize + 16; // header + 1 entry
        let data_table_off = key_table_off + key.len();
        let mut sfo = Vec::new();
        sfo.extend_from_slice(b"\x00PSF");
        sfo.extend_from_slice(&[1, 1, 0, 0]); // version
        sfo.extend_from_slice(&(key_table_off as u32).to_le_bytes()); // 0x08
        sfo.extend_from_slice(&(data_table_off as u32).to_le_bytes()); // 0x0C
        sfo.extend_from_slice(&1u32.to_le_bytes()); // 0x10 entry count
        sfo.extend_from_slice(&0u16.to_le_bytes()); // key_off
        sfo.extend_from_slice(&0x0204u16.to_le_bytes()); // fmt = utf8
        sfo.extend_from_slice(&(val.len() as u32).to_le_bytes()); // len
        sfo.extend_from_slice(&(val.len() as u32).to_le_bytes()); // max
        sfo.extend_from_slice(&0u32.to_le_bytes()); // data_off
        sfo.extend_from_slice(key);
        sfo.extend_from_slice(val.as_bytes());
        sfo
    }

    // Build a minimal \x7FCNT pkg whose only entry is PARAM.SFO.
    fn build_pkg(category: &str) -> Vec<u8> {
        let sfo = build_sfo(category);
        let table_offset = 0x40u32;
        let entry_count = 1u32;
        let sfo_off = table_offset + entry_count * 0x20;
        let mut pkg = vec![0u8; sfo_off as usize];
        pkg[0..4].copy_from_slice(&PKG_MAGIC.to_be_bytes());
        pkg[0x10..0x14].copy_from_slice(&entry_count.to_be_bytes());
        pkg[0x18..0x1C].copy_from_slice(&table_offset.to_be_bytes());
        let e = table_offset as usize;
        pkg[e..e + 4].copy_from_slice(&ENTRY_PARAM_SFO.to_be_bytes());
        pkg[e + 0x10..e + 0x14].copy_from_slice(&sfo_off.to_be_bytes());
        pkg[e + 0x14..e + 0x18].copy_from_slice(&(sfo.len() as u32).to_be_bytes());
        pkg.extend_from_slice(&sfo);
        pkg
    }

    // Build a PARAM.SFO carrying several string keys (CATEGORY, APP_VER, …).
    // Matches the layout `parse_sfo_into` reads: 0x14 header, 0x10-byte index
    // entries (key_off u16, fmt u16, len u32, max u32, data_off u32), then the
    // key table, then the data table.
    fn build_sfo_multi(pairs: &[(&str, &str)]) -> Vec<u8> {
        let n = pairs.len();
        let key_table_off = 0x14 + n * 0x10;
        let mut keys = Vec::new();
        let mut key_offs = Vec::new();
        for (k, _) in pairs {
            key_offs.push(keys.len() as u16);
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);
        }
        let data_table_off = key_table_off + keys.len();
        let mut data = Vec::new();
        let mut data_offs = Vec::new();
        let mut lens = Vec::new();
        for (_, v) in pairs {
            data_offs.push(data.len() as u32);
            let val = format!("{v}\0");
            lens.push(val.len() as u32);
            data.extend_from_slice(val.as_bytes());
        }
        let mut sfo = Vec::new();
        sfo.extend_from_slice(b"\x00PSF");
        sfo.extend_from_slice(&[1, 1, 0, 0]);
        sfo.extend_from_slice(&(key_table_off as u32).to_le_bytes());
        sfo.extend_from_slice(&(data_table_off as u32).to_le_bytes());
        sfo.extend_from_slice(&(n as u32).to_le_bytes());
        for i in 0..n {
            sfo.extend_from_slice(&key_offs[i].to_le_bytes()); // key_off
            sfo.extend_from_slice(&0x0204u16.to_le_bytes()); // fmt = utf8
            sfo.extend_from_slice(&lens[i].to_le_bytes()); // len
            sfo.extend_from_slice(&lens[i].to_le_bytes()); // max
            sfo.extend_from_slice(&data_offs[i].to_le_bytes()); // data_off
        }
        sfo.extend_from_slice(&keys);
        sfo.extend_from_slice(&data);
        sfo
    }

    // A \x7FCNT pkg carrying a content id in the header and a multi-key SFO.
    fn build_pkg_multi(content_id: &str, pairs: &[(&str, &str)]) -> Vec<u8> {
        let sfo = build_sfo_multi(pairs);
        let table_offset = 0x80u32; // past the 0x40-byte content_id field
        let entry_count = 1u32;
        let sfo_off = table_offset + entry_count * 0x20;
        let mut pkg = vec![0u8; sfo_off as usize];
        pkg[0..4].copy_from_slice(&PKG_MAGIC.to_be_bytes());
        pkg[0x10..0x14].copy_from_slice(&entry_count.to_be_bytes());
        pkg[0x18..0x1C].copy_from_slice(&table_offset.to_be_bytes());
        let cid = content_id.as_bytes();
        let n = cid.len().min(36);
        pkg[0x40..0x40 + n].copy_from_slice(&cid[..n]);
        let e = table_offset as usize;
        pkg[e..e + 4].copy_from_slice(&ENTRY_PARAM_SFO.to_be_bytes());
        pkg[e + 0x10..e + 0x14].copy_from_slice(&sfo_off.to_be_bytes());
        pkg[e + 0x14..e + 0x18].copy_from_slice(&(sfo.len() as u32).to_be_bytes());
        pkg.extend_from_slice(&sfo);
        pkg
    }

    #[test]
    fn metadata_from_reader_recovers_title_category_version() {
        let pkg = build_pkg_multi(
            "UP9000-CUSA07842_00-SCUS974290000001",
            &[
                ("APP_VER", "01.04"),
                ("CATEGORY", "gp"),
                ("TITLE", "Jak X: Combat Racing"),
                ("TITLE_ID", "CUSA07842"),
            ],
        );
        let read = move |off: u64, len: u64| {
            let (o, l) = (off as usize, len as usize);
            pkg.get(o..(o + l).min(pkg.len())).map(|s| s.to_vec())
        };
        let m = metadata_from_reader(read).expect("should parse");
        assert_eq!(m.content_id, "UP9000-CUSA07842_00-SCUS974290000001");
        assert_eq!(m.title, "Jak X: Combat Racing");
        assert_eq!(m.title_id, "CUSA07842");
        assert_eq!(m.category, "gp");
        assert_eq!(m.app_ver, "01.04");
        assert_eq!(m.platform, "ps4");
    }

    #[test]
    fn metadata_from_reader_derives_title_id_from_content_when_sfo_lacks_it() {
        // No TITLE_ID key — must fall back to the content id's second segment.
        let pkg = build_pkg_multi(
            "EP4293-CUSA32097_00-ASTNCPS4SIEE0000",
            &[("CATEGORY", "gd")],
        );
        let read = move |off: u64, len: u64| {
            let (o, l) = (off as usize, len as usize);
            pkg.get(o..(o + l).min(pkg.len())).map(|s| s.to_vec())
        };
        let m = metadata_from_reader(read).expect("should parse");
        assert_eq!(m.title_id, "CUSA32097");
        assert_eq!(m.platform, "ps4");
    }

    #[test]
    fn metadata_from_reader_routes_embedded_fih_patch_as_ps5dp() {
        let cnt_base = 0x10000usize;
        let mut cnt = build_pkg_multi(
            "UP0000-PPSA01234_00-TESTGAME00000000",
            &[("TITLE", "Test Game")],
        );
        cnt[0x78..0x7c].copy_from_slice(&0x6000_0000u32.to_be_bytes());
        let mut pkg = vec![0u8; cnt_base];
        pkg[0..4].copy_from_slice(&PKG_MAGIC_FIH.to_be_bytes());
        pkg[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        pkg[0x58..0x60].copy_from_slice(&(cnt_base as u64).to_le_bytes());
        pkg.extend_from_slice(&cnt);

        let read = move |off: u64, len: u64| {
            let (o, l) = (off as usize, len as usize);
            pkg.get(o..(o + l).min(pkg.len())).map(|s| s.to_vec())
        };
        let m = metadata_from_reader(read).expect("should parse embedded CNT");
        assert_eq!(m.platform, "ps5");
        assert_eq!(m.authenticity, PkgAuthenticity::FakeDebug);
        assert_eq!(m.category, "gp");
        assert_eq!(m.title_id, "PPSA01234");
        assert_eq!(
            package_type_for_category_and_platform(&m.category, &m.platform).as_deref(),
            Some("PS5DP")
        );
    }

    #[test]
    fn package_type_mapping_is_platform_specific() {
        assert_eq!(
            package_type_for_category_and_platform("gd", "ps5").as_deref(),
            Some("PS5GD")
        );
        assert_eq!(
            package_type_for_category_and_platform("gp", "ps5").as_deref(),
            Some("PS5DP")
        );
        assert_eq!(package_type_for_category("gp").as_deref(), Some("PS4DP"));
    }

    #[test]
    fn parse_sfo_into_reads_app_ver() {
        // A patch's APP_VER is the authoritative "which update is this" — it
        // shares the base game's content_id and TITLE, so nothing else tells
        // versions apart. Parse it alongside CATEGORY from a realistic SFO.
        let sfo = build_sfo_multi(&[("APP_VER", "01.04"), ("CATEGORY", "gp")]);
        let mut meta = PkgMetadata {
            path: PathBuf::new(),
            size: 0,
            kind: PkgKind::CntContainer,
            authenticity: PkgAuthenticity::Unknown,
            content_id: String::new(),
            title: String::new(),
            title_id: String::new(),
            category: String::new(),
            app_ver: String::new(),
            fingerprint: String::new(),
            package_type: None,
            platform: String::new(),
            icon_png_base64: None,
            warnings: Vec::new(),
        };
        parse_sfo_into(&sfo, &mut meta).expect("SFO should parse");
        assert_eq!(meta.app_ver, "01.04");
        assert_eq!(meta.category, "gp");
    }

    #[test]
    fn param_json_parser_accepts_nul_padding() {
        let mut meta = PkgMetadata {
            path: PathBuf::new(),
            size: 0,
            kind: PkgKind::CntContainer,
            authenticity: PkgAuthenticity::Unknown,
            content_id: String::new(),
            title: String::new(),
            title_id: String::new(),
            category: String::new(),
            app_ver: String::new(),
            fingerprint: String::new(),
            package_type: None,
            platform: String::new(),
            icon_png_base64: None,
            warnings: Vec::new(),
        };
        let mut json = br#"{"contentId":"UP0000-PPSA01234_00-TESTGAME00000000","titleId":"PPSA01234","contentVersion":"01.002.000","localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"Test Game"}}}"#.to_vec();
        json.extend_from_slice(&[0, 0]);
        parse_param_json_into(&json, &mut meta).expect("NUL-padded JSON should parse");
        assert_eq!(meta.content_id, "UP0000-PPSA01234_00-TESTGAME00000000");
        assert_eq!(meta.title, "Test Game");
        assert_eq!(meta.title_id, "PPSA01234");
        assert_eq!(meta.app_ver, "01.002.000");
    }

    #[test]
    fn category_from_reader_distinguishes_patch_from_game() {
        let read = |pkg: Vec<u8>| {
            move |off: u64, len: u64| {
                let (o, l) = (off as usize, len as usize);
                pkg.get(o..(o + l).min(pkg.len())).map(|s| s.to_vec())
            }
        };
        // The load-bearing case: a patch ("gp") must derive a "…DP" type so the
        // payload guard fires and can't wipe the base; a full game ("gd") must
        // NOT, so a legitimate re-install still uses the normal cascade.
        for (cat, want) in [("gp", "PS4DP"), ("gd", "PS4GD"), ("ac", "PS4AC")] {
            let got = category_from_reader(read(build_pkg(cat)));
            assert_eq!(got.as_deref(), Some(cat), "category for {cat}");
            assert_eq!(
                package_type_for_category(got.as_deref().unwrap()).as_deref(),
                Some(want)
            );
        }
        // Corrupt/non-\x7FCNT magic ⇒ None (caller keeps its default).
        let mut bad = build_pkg("gp");
        bad[0] = 0;
        assert_eq!(category_from_reader(read(bad)), None);
    }

    #[test]
    fn title_id_recovered_from_scene_filenames() {
        // The exact PS4 case a user hit — a Bloodborne scene release whose
        // platform must be classifiable from the name alone (no header read).
        assert_eq!(
            title_id_from_filename("[SPSX]-Bloodborne.Complete.Edition-CUSA00900-USA-Game-PS4.pkg")
                .as_deref(),
            Some("CUSA00900")
        );
        assert_eq!(
            title_id_from_filename("PS5_PPSA01650_v1.03.pkg").as_deref(),
            Some("PPSA01650")
        );
        // …and that derived id badges the right platform end-to-end.
        assert_eq!(derive_platform(PKG_MAGIC, "", "CUSA00900"), "ps4");
        // No id in the name → None (caller falls back to a header read).
        assert_eq!(title_id_from_filename("update.pkg"), None);
        assert_eq!(title_id_from_filename("game-v1.02.pkg"), None);
        // Must be exactly 4 letters + 5 digits — near-misses are rejected.
        assert_eq!(title_id_from_filename("CUSA0090.pkg"), None);
        assert_eq!(title_id_from_filename("CUSA009000.pkg"), None);
    }

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
    fn package_fingerprint_is_stable_and_variant_sensitive() {
        let mut a = vec![0x11u8; (PACKAGE_FINGERPRINT_SAMPLE_BYTES * 3) as usize];
        let mut b = a.clone();
        // A tail-only difference must distinguish two same-size variants.
        *b.last_mut().unwrap() = 0x22;
        let read = |bytes: Vec<u8>| {
            move |offset: u64, len: u64| {
                let start = offset as usize;
                let end = start.checked_add(len as usize)?;
                bytes.get(start..end).map(|s| s.to_vec())
            }
        };
        let a1 = package_fingerprint_from_reader(a.len() as u64, read(a.clone())).unwrap();
        let a2 = package_fingerprint_from_reader(a.len() as u64, read(a.clone())).unwrap();
        let b1 = package_fingerprint_from_reader(b.len() as u64, read(b)).unwrap();
        assert_eq!(a1, a2);
        assert_ne!(a1, b1);

        // A middle-only change is intentionally outside this bounded identity
        // sample; full-file integrity remains the FS_HASH operation's job.
        a[PACKAGE_FINGERPRINT_SAMPLE_BYTES as usize + 7] ^= 0xff;
        let a_middle = package_fingerprint_from_reader(a.len() as u64, read(a)).unwrap();
        assert_eq!(a1, a_middle);
    }

    #[test]
    fn package_fingerprint_rejects_zero_or_short_reads() {
        assert_eq!(
            package_fingerprint_from_reader(0, |_o, _l| Some(vec![])),
            None
        );
        assert_eq!(
            package_fingerprint_from_reader(100, |_o, _l| Some(vec![0; 1])),
            None
        );
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
    fn ps5_fih_parses_embedded_cnt_and_uses_ps5_package_type() {
        let dir = tempdir();
        let path = dir.join("PPSA01234.fpkg");
        let cnt_base = 0x10000usize;
        let mut cnt = build_pkg_multi(
            "UP0000-PPSA01234_00-TESTGAME00000000",
            &[("CATEGORY", "gd")],
        );
        // A PS5 CNT normally has param.json; the embedded content id and
        // content flags are sufficient to prove platform/base routing here.
        cnt[0x78..0x7c].copy_from_slice(&0u32.to_be_bytes());
        let mut buf = vec![0u8; cnt_base];
        buf[0..4].copy_from_slice(&PKG_MAGIC_FIH.to_be_bytes());
        buf[0x05] = 0x00; // debug/fake finalized image
        buf[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        buf[0x58..0x60].copy_from_slice(&(cnt_base as u64).to_le_bytes());
        buf.extend_from_slice(&cnt);
        std::fs::write(&path, &buf).unwrap();
        let meta = parse_pkg(&path).unwrap();
        match meta.kind {
            PkgKind::Ps5Finalized {
                signed_byte,
                format_version,
            } => {
                assert_eq!(signed_byte, 0);
                assert_eq!(format_version, 3);
            }
            _ => panic!("expected PS5 finalized kind"),
        }
        assert_eq!(meta.authenticity, PkgAuthenticity::FakeDebug);
        assert_eq!(meta.content_id, "UP0000-PPSA01234_00-TESTGAME00000000");
        assert_eq!(meta.platform, "ps5");
        assert_eq!(meta.category, "gd");
        assert_eq!(meta.package_type.as_deref(), Some("PS5GD"));
    }

    #[test]
    fn ps5_fih_patch_flags_route_as_ps5_patch() {
        let dir = tempdir();
        let path = dir.join("PPSA01234-update.pkg");
        let cnt_base = 0x10000usize;
        let mut cnt = build_pkg_multi("UP0000-PPSA01234_00-TESTGAME00000000", &[]);
        cnt[0x78..0x7c].copy_from_slice(&0x6000_0000u32.to_be_bytes());
        let mut buf = vec![0u8; cnt_base];
        buf[0..4].copy_from_slice(&PKG_MAGIC_FIH.to_be_bytes());
        buf[0x05] = 0;
        buf[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        buf[0x58..0x60].copy_from_slice(&(cnt_base as u64).to_le_bytes());
        buf.extend_from_slice(&cnt);
        std::fs::write(&path, &buf).unwrap();
        let meta = parse_pkg(&path).unwrap();
        assert_eq!(meta.category, "gp");
        assert_eq!(meta.package_type.as_deref(), Some("PS5DP"));
    }

    /// Real debug FPKGs (FIH, signed byte 0x00). 5.26 read these as PS4GD with a
    /// filename-derived content id, so staged installs could never verify (#319).
    #[test]
    fn real_debug_fpkg_samples_parse_content_id() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let samples = [
            ("webbrowser.pkg", "IV9999-WEBB00002_00-XXXXXXXXXXXXXXXX"),
            (
                "EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg",
                "EP7579-PPSA17599_00-EXP33DLC10000PS5",
            ),
        ];
        let mut checked = 0;
        for (name, cid) in samples {
            let path = std::path::Path::new(&dir).join(name);
            let Ok(bytes) = std::fs::read(&path) else {
                eprintln!("skip: {} not present", path.display());
                continue;
            };
            let meta = parse_pkg(&path).unwrap();
            assert_eq!(meta.content_id, cid, "{name} via parse_pkg");
            assert_eq!(meta.authenticity, PkgAuthenticity::FakeDebug, "{name}");

            let rm = metadata_from_reader(|off, len| {
                let start = off as usize;
                let end = (off + len).min(bytes.len() as u64) as usize;
                (start < end).then(|| bytes[start..end].to_vec())
            })
            .expect("reader metadata");
            assert_eq!(rm.content_id, cid, "{name} via metadata_from_reader");
            assert_eq!(rm.platform, "ps5", "{name}");
            checked += 1;
        }
        eprintln!("checked {checked} real sample(s)");
    }

    fn tempdir() -> std::path::PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("ps5upload-pkg-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }
}
