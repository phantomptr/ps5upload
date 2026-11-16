//! Archive entry names made safe, and the `.7z` checks and decode threads, shared by the
//! engine's transfer and unpack code (`ps5upload-core::transfer` re-exports them) and the
//! in-place reader ([`crate::stream`]).

use anyhow::{bail, Result};

/// An entry's name, reading UTF-8 even when the zip doesn't say so.
///
/// The spec says a name without the UTF-8 flag (general-purpose bit 11) is
/// CP437, and the `zip` crate decodes it that way. But zips made by macOS,
/// Linux and most tools write UTF-8 without setting the flag, so a file
/// called `名前 é.txt` landed on the PS5 as `σÉìσëì ├⌐.txt`. Like Info-ZIP
/// and 7-Zip, use the raw bytes when they are valid UTF-8 and fall back to
/// the crate's CP437 decoding only when they are not.
pub fn zip_entry_name(raw: &[u8], decoded: &str) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => decoded.to_string(),
    }
}

/// Sanitize a zip entry name into a safe POSIX-relative path (forward
/// slashes for the PS5). Rejects traversal (`..`), NUL, backslash segments,
/// and absolute/empty paths — the same zip-slip defense as the client's
/// `save_archive::sanitize_entry`, returning a string instead of a host
/// `PathBuf`. Returns `None` for directory-only or unsafe names.
pub fn sanitize_zip_entry(name: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return None;
        }
        if seg.contains('\\') || seg.contains('\0') {
            return None;
        }
        parts.push(seg);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Normalise + validate a 7z entry path. Same zip-slip rules as
/// `sanitize_zip_entry`, except 7z archives created on Windows legitimately use
/// '\\' as the path separator (zip always uses '/'), so backslashes are
/// translated to forward slashes rather than rejected.
pub fn sanitize_7z_entry(name: &str) -> Option<String> {
    sanitize_zip_entry(&name.replace('\\', "/"))
}

/// The refusal for a 7z whose solid block has stream-less entries (directories, empty
/// files) between its streamed files. The `sevenz-rust2` per-block walk covers fewer
/// entries than such a block spans and would silently drop the last files, so the
/// archive is refused rather than partly uploaded. Shared by the AVA1 sources and the plan previews.
pub const SEVENZ_LAYOUT_UNSUPPORTED: &str =
    "this 7z archive's layout is not supported (a solid block has directories or empty files between its files); re-pack it with 7-Zip or extract it first";

/// `Err` with [`SEVENZ_LAYOUT_UNSUPPORTED`] when any stream-less entry sits inside a
/// block (before the block's last streamed file). Trailing and leading stream-less
/// entries are not in a block and are fine.
pub fn sevenz_check_layout(archive: &sevenz_rust2::Archive) -> Result<()> {
    let fbi = &archive.stream_map.file_block_index;
    let bad = archive
        .files
        .iter()
        .enumerate()
        .any(|(i, e)| !e.has_stream() && fbi.get(i).copied().flatten().is_some());
    if bad {
        bail!("{SEVENZ_LAYOUT_UNSUPPORTED}");
    }
    Ok(())
}

/// Pick the LZMA2 worker-thread count. Defaults to 1 — multi-threaded decode
/// is opt-in via `PS5UPLOAD_7Z_THREADS` because it trades a bounded streaming
/// footprint for one proportional to the whole archive.
///
/// Measured on a 2.5 GB corpus (sevenz-rust2 0.22.2 / lzma-rust2 0.20.1),
/// peak RSS for the decode alone:
///
/// | archive packed with | threads=1 | threads>1        |
/// |---------------------|-----------|------------------|
/// | `-mmt=8` (MT LZMA2) | 74 MiB    | 2.3 GiB, +24%    |
/// | `-mmt=1` (solid)    | 74 MiB    | 4.4 GiB, +0%     |
///
/// The reason is structural: `Lzma2ReaderMt` splits work on LZMA2
/// dictionary-reset chunks (control byte `>= 0xE0` or `== 0x01`) and buffers
/// each unit's *decompressed* output in a `Vec<u8>`. A solid stream contains a
/// single dict reset, so the one work unit is the entire archive — the reader
/// stops streaming and materialises everything in RAM. Halving a 205 GB game
/// dump's transfer time is worthless if the engine is OOM-killed first, and
/// Docker hosts and Android have the tightest budgets of anyone.
///
/// So: honour an explicit opt-in (clamped to a sane range), otherwise stay on
/// the single-threaded path that keeps peak RAM at one LZMA2 window.
pub fn select_sevenz_decode_threads(configured: Option<&str>) -> u32 {
    const HARD_MAX: usize = 16;

    configured
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, HARD_MAX))
        .unwrap_or(1) as u32
}

pub fn sevenz_decode_threads() -> u32 {
    select_sevenz_decode_threads(std::env::var("PS5UPLOAD_7Z_THREADS").ok().as_deref())
}
