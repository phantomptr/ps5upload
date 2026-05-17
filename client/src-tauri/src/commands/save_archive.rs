//! Save-data backup/restore as .zip archives.
//!
//! The Saves screen wraps a save folder into `<title_id>.zip` so users
//! end up with a single portable file per backup instead of a directory
//! tree. The zip's canonical shape is:
//!
//! ```text
//! <title_id>.zip
//!  └─ <title_id>/
//!     ├─ savefile.bin
//!     └─ sce_sys/…
//! ```
//!
//! Backup flow (driven from the renderer):
//!   1. `save_archive_make_temp` — create a scratch dir under the system
//!      temp root.
//!   2. existing `transfer_download` — pulls the PS5 save folder into
//!      the scratch dir as `<scratch>/<title_id>/<files>` (the engine's
//!      download walker prefixes by basename).
//!   3. `save_archive_zip` — zip `<scratch>/<title_id>/` into the
//!      user-picked `.zip` path.
//!   4. `save_archive_cleanup_temp` — remove the scratch dir.
//!
//! Restore flow:
//!   1. `save_archive_make_temp` — scratch dir.
//!   2. `save_archive_unzip` — strict-validate the zip's layout, then
//!      extract into the scratch dir.
//!   3. existing `transfer_dir` from `<scratch>/<title_id>/` → PS5 path.
//!   4. `save_archive_cleanup_temp`.
//!
//! Strict validation rejects any zip whose top-level isn't exactly one
//! folder named `<title_id>`. Finder noise (`__MACOSX/`, `.DS_Store`) is
//! ignored so a macOS-Finder-zipped backup still works.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

// ── temp dir lifecycle ──────────────────────────────────────────────────

/// Monotonic counter so two `save_archive_make_temp` calls within the
/// same nanosecond (possible under heavy contention or coarse clock
/// resolution on some platforms) get distinct paths — without it, both
/// callers' downloads would land in the same scratch dir and clobber
/// each other since `create_dir_all` is idempotent.
static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[tauri::command]
pub async fn save_archive_make_temp(prefix: String) -> Result<String, String> {
    // Sync FS work on a Tauri command runs on the main thread —
    // create_dir is one syscall (usually <1 ms) so the prior sync
    // version was tolerable but inconsistent with the rest of the
    // file's spawn_blocking pattern. Routing through spawn_blocking
    // matches save_archive_zip and keeps the main thread responsive
    // even under FS contention.
    tokio::task::spawn_blocking(move || {
        let safe: String = prefix
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(40)
            .collect();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ps5upload-save-{safe}-{ts}-{n}"));
        // `create_dir` (not `create_dir_all`) so the rare case where
        // our path *does* somehow already exist surfaces as an error
        // instead of a silent shared-state hazard. Parent (the OS
        // temp root) is always present, so the recursive variant
        // isn't needed.
        std::fs::create_dir(&dir).map_err(|e| format!("create_dir {}: {e}", dir.display()))?;
        Ok(dir.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("save_archive_make_temp join: {e}"))?
}

#[tauri::command]
pub async fn save_archive_cleanup_temp(path: String) -> Result<(), String> {
    if path.is_empty() {
        return Ok(());
    }
    // remove_dir_all walks every entry under the scratch dir — for a
    // save with hundreds of files this is a meaningful syscall budget
    // (10s of ms minimum, more on slow disks). Sync command would
    // freeze the UI thread for the entire walk. spawn_blocking lets
    // it run on the blocking pool.
    tokio::task::spawn_blocking(move || {
        let p = Path::new(&path);
        // Safety net: only delete inside the system temp dir. If the
        // renderer ever passes a bad path (or this command is
        // misused), we won't recursively wipe an arbitrary directory.
        //
        // `Path::starts_with` is true when `p == tmp`, so a
        // `starts_with` check alone would happily `remove_dir_all`
        // the entire system temp root. Require that `p` is a *strict*
        // descendant AND that its final component carries the
        // `ps5upload-save-` prefix that `save_archive_make_temp`
        // always stamps — so cleanup can only ever touch a dir this
        // module created.
        let tmp = std::env::temp_dir();
        let named_ours = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("ps5upload-save-"));
        if !p.starts_with(&tmp) || p == tmp.as_path() || !named_ours {
            return Err(format!(
                "refusing to clean path outside our temp scratch dirs: {}",
                p.display()
            ));
        }
        // Best-effort: caller treats cleanup failure as non-fatal.
        let _ = std::fs::remove_dir_all(p);
        Ok(())
    })
    .await
    .map_err(|e| format!("save_archive_cleanup_temp join: {e}"))?
}

// ── zip a downloaded save folder ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SaveArchiveZipReq {
    /// Scratch dir that contains `<inner_root>/<files>` (the structure
    /// produced by `transfer_download` against a PS5 save folder).
    pub src_dir: String,
    /// Title ID — must match the folder name under `src_dir`.
    pub inner_root: String,
    /// Absolute path of the `.zip` file to write.
    pub dest_zip: String,
}

#[tauri::command]
pub async fn save_archive_zip(req: SaveArchiveZipReq) -> Result<(), String> {
    let dest = req.dest_zip.clone();
    let result = tokio::task::spawn_blocking(move || {
        zip_folder(
            Path::new(&req.src_dir),
            &req.inner_root,
            Path::new(&req.dest_zip),
        )
    })
    .await
    .map_err(|e| format!("zip task: {e}"))?;
    if let Err(e) = result {
        // On failure (typically ENOSPC mid-write), remove the
        // half-written .zip so the user doesn't see a corrupt file
        // at their picked path. Best-effort — if the unlink itself
        // fails, the original error is still what surfaces.
        let _ = std::fs::remove_file(&dest);
        return Err(format!("zip: {e:#}"));
    }
    Ok(())
}

fn zip_folder(src_dir: &Path, inner_root: &str, dest_zip: &Path) -> Result<()> {
    let src_root = src_dir.join(inner_root);
    if !src_root.is_dir() {
        return Err(anyhow!(
            "expected `{}/` under scratch dir, not found at {}",
            inner_root,
            src_root.display()
        ));
    }
    let out = File::create(dest_zip).with_context(|| format!("create {}", dest_zip.display()))?;
    let mut zip = ZipWriter::new(BufWriter::new(out));
    // Distinct file vs. directory modes. Unix dirs need the execute bit
    // to be traversable — using 0o644 for both (as we did originally)
    // produces `drw-r--r--` entries that reject `cd` after extraction.
    let file_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let dir_opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o755);

    let mut buf = vec![0u8; 64 * 1024];
    add_dir_recursive(
        &mut zip, &src_root, inner_root, &file_opts, &dir_opts, &mut buf,
    )?;
    zip.finish().context("finish zip")?;
    Ok(())
}

fn add_dir_recursive<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    abs_dir: &Path,
    rel_prefix: &str,
    file_opts: &SimpleFileOptions,
    dir_opts: &SimpleFileOptions,
    buf: &mut [u8],
) -> Result<()> {
    zip.add_directory(rel_prefix, *dir_opts)
        .with_context(|| format!("add_directory {rel_prefix}"))?;
    for entry in
        std::fs::read_dir(abs_dir).with_context(|| format!("read_dir {}", abs_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Zip entry paths are always forward-slash-separated, regardless
        // of host OS — Windows builds must not emit backslash entries.
        let zip_path = format!("{rel_prefix}/{name}");
        let kind = entry.file_type()?;
        if kind.is_dir() {
            add_dir_recursive(zip, &entry.path(), &zip_path, file_opts, dir_opts, buf)?;
        } else if kind.is_file() {
            zip.start_file(zip_path.clone(), *file_opts)
                .with_context(|| format!("start_file {zip_path}"))?;
            let f = File::open(entry.path())
                .with_context(|| format!("open {}", entry.path().display()))?;
            let mut r = BufReader::new(f);
            loop {
                let n = r.read(buf)?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buf[..n])?;
            }
        }
        // Symlinks/sockets/etc. are skipped — PS5 saves are plain files.
    }
    Ok(())
}

// ── format-aware backup pre-zip cleanup ─────────────────────────────────
//
// After `transfer_download` mirrors the PS5's `savedata_prospero/<title>/`
// tree into a scratch dir, the layout for a PS2-Classic title can include
// nested `<title>/<title>/<files>` subdirs that Sony stores for emulator
// bookkeeping (verified empirically — depth-1 vs depth-2 `sdimg_*` images
// have different CRCs, so they're real distinct files, not bind-mount
// duplicates). garlic-savemgr deliberately ignores those (its
// `scan_title_dir` at `src/main.c:228` filters to `S_ISREG` only —
// directories are silently skipped, no recursion). Mounting the depth-1
// image gives the user their working save.
//
// This finalize step prunes our scratch dir to the equivalent set:
//   1. Keep immediate files of the title folder.
//   2. Keep `sce_sys/` subdirectory contents (PS5-native saves store
//      `icon0.png` + `param.sfo` here outside the encrypted image).
//   3. Drop every other subdirectory.
//
// For each retained `sdimg_*` file, peek byte 0:
//   - `0x01` = PS4 format (PS2-Classics, PS4 backwards-compat saves).
//     Strip the `sdimg_` prefix so the zip names match the cross-tool
//     convention used by save-resigners (apollo-ps4 etc).
//   - `0x02` = PS5-native. Keep the `sdimg_` prefix verbatim — the
//     sealed key is embedded at offset 0x800 inside the image.
//
// Restore reverses the rename: `save_archive_restore_prepare` re-adds
// `sdimg_` to any bare top-level file (i.e. not `sdimg_*`, not `*.bin`,
// not under `sce_sys/`) before the upload step writes to PS5.

#[derive(Debug, Deserialize)]
pub struct SaveArchiveBackupFinalizeReq {
    /// Scratch dir that contains `<title_id>/<files>` from a fresh
    /// `transfer_download`. We mutate the contents in place.
    pub src_dir: String,
    /// Title ID (folder name under `src_dir`).
    pub title_id: String,
}

#[derive(Debug, Serialize)]
pub struct SaveArchiveBackupFinalizeResp {
    pub kept_files: u32,
    pub stripped_count: u32,
    pub dropped_dirs: u32,
}

#[tauri::command]
pub async fn save_archive_backup_finalize(
    req: SaveArchiveBackupFinalizeReq,
) -> Result<SaveArchiveBackupFinalizeResp, String> {
    tokio::task::spawn_blocking(move || {
        backup_finalize(&Path::new(&req.src_dir).join(&req.title_id))
    })
    .await
    .map_err(|e| format!("backup finalize task: {e}"))?
    .map_err(|e| format!("backup finalize: {e:#}"))
}

fn backup_finalize(title_dir: &Path) -> Result<SaveArchiveBackupFinalizeResp> {
    if !title_dir.is_dir() {
        return Err(anyhow!("title dir not found: {}", title_dir.display()));
    }
    // PS5 sometimes wraps the actual save inside one or more
    // `<title>/<title>/…` subdirectories (PS2-Classic emulator does
    // this — verified against a real backup of CUSA03474). If the
    // top-level scratch dir has no immediate files, descend through
    // wrappers until we find the layer that does, then move that
    // layer's contents up so the rest of finalize sees a flat
    // structure. Without this step the standard "drop non-sce_sys
    // subdirs" logic below would empty the entire backup because the
    // wrappers themselves are non-sce_sys dirs.
    flatten_wrapper_subdirs(title_dir)?;

    let mut kept_files: u32 = 0;
    let mut stripped_count: u32 = 0;
    let mut dropped_dirs: u32 = 0;

    let entries: Vec<_> = std::fs::read_dir(title_dir)
        .with_context(|| format!("read_dir {}", title_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        let path = entry.path();

        if kind.is_dir() {
            if name == "sce_sys" {
                // Keep — PS5-native saves store plaintext metadata here.
                // Count files inside it so the caller has a meaningful
                // tally back.
                kept_files += count_files_recursive(&path)?;
            } else {
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("drop {}", path.display()))?;
                dropped_dirs += 1;
            }
            continue;
        }
        if !kind.is_file() {
            // Skip symlinks/sockets/etc. — never expected in saves.
            continue;
        }

        if let Some(rest) = name.strip_prefix("sdimg_") {
            // Peek the format byte. PS4 = 0x01, PS5-native = 0x02.
            let mut f = File::open(&path).with_context(|| format!("open {}", path.display()))?;
            let mut byte0 = [0u8; 1];
            // A truncated image (less than 1 byte!) is malformed; treat
            // as unknown and don't rename, so the user can still see it
            // in the zip and we don't break the file.
            let read_ok = f.read_exact(&mut byte0).is_ok();
            drop(f);
            if read_ok && byte0[0] == 0x01 {
                // PS4 / PS2-Classic / PSP-Classic — strip the prefix.
                let new_path = title_dir.join(rest);
                // Don't let a prefix-strip silently clobber a sibling that
                // already occupies the unprefixed name (e.g. both
                // `sdimg_SLUS-20268` and a bare `SLUS-20268` present) —
                // `fs::rename` overwrites the destination on Unix.
                if new_path.exists() {
                    return Err(anyhow!(
                        "cannot strip sdimg_ prefix: {} already exists",
                        new_path.display()
                    ));
                }
                std::fs::rename(&path, &new_path).with_context(|| {
                    format!("rename {} -> {}", path.display(), new_path.display())
                })?;
                stripped_count += 1;
            }
            // 0x02 (PS5-native) or unknown: keep `sdimg_` prefix.
        }
        // `.bin` sealed keys + any other top-level files: keep as-is.
        kept_files += 1;
    }

    Ok(SaveArchiveBackupFinalizeResp {
        kept_files,
        stripped_count,
        dropped_dirs,
    })
}

/// Walk down the title scratch dir, peeling off `<title>/<title>/…`
/// wrapper subdirectories until we find a layer that has regular files.
/// Move that layer's contents up to `title_dir` and drop the wrapper
/// subtree.
///
/// Stops descending if:
///   - the current layer has any regular files (this is the data layer)
///   - the current layer has multiple non-`sce_sys` subdirs (ambiguous)
///   - the current layer is empty
///   - depth cap reached (8 is well above any real-world nesting)
///
/// Idempotent: calling on an already-flat tree is a no-op. Preserves
/// `sce_sys/` regardless of which layer it lives at.
///
/// Implementation note: we rename the wrapper into a `.tmp` staging
/// path inside `title_dir` BEFORE descending. That way, when we move
/// the data-layer's contents up to `title_dir`, none of those moves
/// can collide with the wrapper itself (which would otherwise share a
/// name with the data-layer's own subdirectories — verified empirically
/// against real CUSA03474 backups).
/// (2.9.0) Helper: read a DirEntry's file_type and propagate the
/// I/O error with path context. Replaces the `.file_type().map(|k|
/// k.is_file()).unwrap_or(false)` pattern that silently treated
/// I/O failures as "not a match" — when called inside the wrapper
/// flattener, that swallowed metadata-read failures on legitimate
/// save data files, causing the function to descend past them and
/// produce empty backups. Now any file_type failure aborts the
/// flatten with a clear `file_type <path>: <err>` message.
fn entry_is_file(e: &std::fs::DirEntry) -> Result<bool> {
    Ok(e.file_type()
        .with_context(|| format!("file_type {}", e.path().display()))?
        .is_file())
}
fn entry_is_dir(e: &std::fs::DirEntry) -> Result<bool> {
    Ok(e.file_type()
        .with_context(|| format!("file_type {}", e.path().display()))?
        .is_dir())
}

fn flatten_wrapper_subdirs(title_dir: &Path) -> Result<()> {
    const MAX_DESCEND: u32 = 8;
    const STAGING_NAME: &str = ".ps5upload_extract_tmp";

    // Top-level check: if there are immediate files at title_dir,
    // there's nothing to flatten. Same if there isn't exactly one
    // non-`sce_sys` subdir to descend into.
    let top_entries: Vec<_> = std::fs::read_dir(title_dir)
        .with_context(|| format!("read_dir {}", title_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut has_top_files = false;
    for e in &top_entries {
        if entry_is_file(e)? {
            has_top_files = true;
            break;
        }
    }
    if has_top_files {
        return Ok(());
    }
    let mut top_non_sce: Vec<&std::fs::DirEntry> = Vec::new();
    for e in &top_entries {
        if entry_is_dir(e)?
            && e.file_name() != std::ffi::OsStr::new("sce_sys")
            && e.file_name() != std::ffi::OsStr::new(STAGING_NAME)
        {
            top_non_sce.push(e);
        }
    }
    if top_non_sce.len() != 1 {
        return Ok(()); // already flat, empty, or ambiguous shape
    }

    // Rename the wrapper into a staging path inside title_dir. Now the
    // data-layer's contents — including any subdirectory named the same
    // as the wrapper — can be moved into title_dir without colliding.
    let wrapper_orig = top_non_sce[0].path();
    let staging = title_dir.join(STAGING_NAME);
    if staging.exists() {
        return Err(anyhow!(
            "flatten staging path already exists: {} (leftover from a previous failed run?)",
            staging.display()
        ));
    }
    std::fs::rename(&wrapper_orig, &staging).with_context(|| {
        format!(
            "stage rename {} -> {}",
            wrapper_orig.display(),
            staging.display()
        )
    })?;

    // Descend through the staging subtree to find the data layer.
    let mut cur = staging.clone();
    for _ in 0..MAX_DESCEND {
        let entries: Vec<_> = std::fs::read_dir(&cur)
            .with_context(|| format!("read_dir {}", cur.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        let mut has_files = false;
        for e in &entries {
            if entry_is_file(e)? {
                has_files = true;
                break;
            }
        }
        if has_files {
            break;
        }
        let mut non_sce_subdirs: Vec<&std::fs::DirEntry> = Vec::new();
        for e in &entries {
            if entry_is_dir(e)? && e.file_name() != std::ffi::OsStr::new("sce_sys") {
                non_sce_subdirs.push(e);
            }
        }
        if non_sce_subdirs.len() != 1 {
            break;
        }
        cur = non_sce_subdirs[0].path();
    }

    // Move every entry under `cur` up to `title_dir`. Staging path
    // makes these collision-free.
    let to_move: Vec<(PathBuf, std::ffi::OsString)> = std::fs::read_dir(&cur)
        .with_context(|| format!("read_dir {}", cur.display()))?
        .filter_map(|e| e.ok())
        .map(|e| (e.path(), e.file_name()))
        .collect();
    for (src, name) in to_move {
        let dst = title_dir.join(&name);
        if dst.exists() {
            return Err(anyhow!(
                "flatten collision: {} already exists (post-staging — shouldn't be possible)",
                dst.display()
            ));
        }
        std::fs::rename(&src, &dst)
            .with_context(|| format!("move {} -> {}", src.display(), dst.display()))?;
    }

    // Drop the now-mostly-empty staging subtree (anything left in it
    // is non-data — Sony's deeper bookkeeping wrappers).
    std::fs::remove_dir_all(&staging)
        .with_context(|| format!("drop staging {}", staging.display()))?;

    Ok(())
}

fn count_files_recursive(dir: &Path) -> Result<u32> {
    let mut n: u32 = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).with_context(|| format!("read_dir {}", d.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                n += 1;
            }
        }
    }
    Ok(n)
}

// ── format-aware restore prep ───────────────────────────────────────────
//
// After `unzip_strict` extracts a backup zip into a scratch dir, this
// step prepares the layout for upload to the PS5. It re-adds the
// `sdimg_` prefix to bare top-level files (i.e. PS4-style images whose
// prefix was stripped during backup_finalize) so the PS5 sees the
// filename it expects. Files that already start with `sdimg_` (PS5-
// native images), end in `.bin` (sealed keys), or live under `sce_sys/`
// (plaintext metadata) are left alone.
//
// This works for zips produced by either:
//   - The current backup flow (cleaned by `backup_finalize`).
//   - A user-edited / external zip following the same convention
//     (resigner output, manual edits, etc).
//   - Legacy ps5upload zips (pre-finalize) where every file already has
//     its `sdimg_` prefix — those pass through unchanged.

#[derive(Debug, Deserialize)]
pub struct SaveArchiveRestorePrepareReq {
    pub src_dir: String,
    pub title_id: String,
}

#[derive(Debug, Serialize)]
pub struct SaveArchiveRestorePrepareResp {
    pub renamed_count: u32,
}

#[tauri::command]
pub async fn save_archive_restore_prepare(
    req: SaveArchiveRestorePrepareReq,
) -> Result<SaveArchiveRestorePrepareResp, String> {
    tokio::task::spawn_blocking(move || {
        restore_prepare(&Path::new(&req.src_dir).join(&req.title_id))
    })
    .await
    .map_err(|e| format!("restore prepare task: {e}"))?
    .map_err(|e| format!("restore prepare: {e:#}"))
}

fn restore_prepare(title_dir: &Path) -> Result<SaveArchiveRestorePrepareResp> {
    if !title_dir.is_dir() {
        return Err(anyhow!("title dir not found: {}", title_dir.display()));
    }
    let mut renamed_count: u32 = 0;

    let entries: Vec<_> = std::fs::read_dir(title_dir)
        .with_context(|| format!("read_dir {}", title_dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        if !kind.is_file() {
            // sce_sys/ subdir + any other dirs: leave structure
            // alone, the uploader writes them verbatim.
            continue;
        }
        if name.starts_with("sdimg_") || name.ends_with(".bin") {
            continue; // already in PS5-correct form
        }
        // Skip files with any dot in the name — those are NOT what
        // `backup_finalize` produces. The strip-prefix step only fires
        // on `sdimg_<bare-disc-id>` files (PS2/PS4-emulator images
        // whose canonical names like `SLUS-20268` are extensionless).
        // A title-level file with a dot (`param.sfo`, `README.txt`,
        // user-added `notes.md`, etc.) sits there for some other
        // reason and must NOT be promoted to a fake `sdimg_*` image
        // on the PS5 — that would clutter savedata_prospero with
        // bogus encrypted-image entries.
        if name.contains('.') {
            continue;
        }
        // Bare extensionless top-level file: assumed to be a prefix-
        // stripped PS4 image. Re-add `sdimg_` so the PS5 sees the
        // expected filename in `savedata_prospero/<title>/`.
        let new_name = format!("sdimg_{name}");
        let new_path = title_dir.join(&new_name);
        std::fs::rename(entry.path(), &new_path).with_context(|| {
            format!(
                "rename {} -> {}",
                entry.path().display(),
                new_path.display()
            )
        })?;
        renamed_count += 1;
    }

    Ok(SaveArchiveRestorePrepareResp { renamed_count })
}

// ── strict unzip for restore ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SaveArchiveUnzipReq {
    /// Path to the user-picked `.zip`.
    pub zip_path: String,
    /// Empty scratch dir to extract into.
    pub dest_dir: String,
    /// Title ID we expect as the zip's single top-level folder.
    pub expected_inner: String,
}

#[derive(Debug, Serialize)]
pub struct SaveArchiveUnzipResp {
    pub inner_root: String,
    pub file_count: u32,
}

#[tauri::command]
pub async fn save_archive_unzip(req: SaveArchiveUnzipReq) -> Result<SaveArchiveUnzipResp, String> {
    tokio::task::spawn_blocking(move || {
        unzip_strict(
            Path::new(&req.zip_path),
            Path::new(&req.dest_dir),
            &req.expected_inner,
        )
    })
    .await
    .map_err(|e| format!("unzip task: {e}"))?
    .map_err(|e| format!("unzip: {e:#}"))
}

fn is_finder_noise(name: &str) -> bool {
    name.starts_with("__MACOSX/") || name == ".DS_Store" || name.ends_with("/.DS_Store")
}

fn unzip_strict(
    zip_path: &Path,
    dest_dir: &Path,
    expected_inner: &str,
) -> Result<SaveArchiveUnzipResp> {
    let f = File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(f)).context("read zip header")?;

    // Pass 1: validate layout before writing any bytes. Catching a bad
    // zip here means a malformed restore never half-overwrites the live
    // save (we also fsDelete first on the PS5 side, so a half-extract
    // followed by upload would be especially bad).
    let mut root_dirs: BTreeSet<String> = BTreeSet::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).context("read zip entry")?;
        let name = entry.name().to_string();
        if is_finder_noise(&name) {
            continue;
        }
        let first = name.split('/').next().unwrap_or("");
        if first.is_empty() {
            continue;
        }
        if !name.contains('/') {
            return Err(anyhow!(
                "zip has a file at its root (`{}`); expected a single folder named `{}/`",
                name,
                expected_inner
            ));
        }
        root_dirs.insert(first.to_string());
    }
    if root_dirs.len() != 1 {
        return Err(anyhow!(
            "zip must contain exactly one top-level folder; found {} ({:?})",
            root_dirs.len(),
            root_dirs
        ));
    }
    let only_root = root_dirs.into_iter().next().unwrap();
    if only_root != expected_inner {
        return Err(anyhow!(
            "zip's top-level folder is `{}`, expected `{}`",
            only_root,
            expected_inner
        ));
    }

    // Pass 2: extract. Sanitize each entry against zip-slip.
    let mut count = 0u32;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("read zip entry")?;
        let name = entry.name().to_string();
        if is_finder_noise(&name) {
            continue;
        }
        let safe = sanitize_entry(&name).ok_or_else(|| anyhow!("unsafe zip entry path: {name}"))?;
        let out_path = dest_dir.join(&safe);
        // Belt + suspenders: even if `sanitize_entry` missed something
        // (e.g. a Windows drive-letter segment like `C:foo` that
        // `PathBuf::push` treats as drive-rooted), reject any final
        // path that doesn't lexically stay under `dest_dir`. This
        // closes any platform-specific zip-slip vector that the
        // segment-level filter doesn't anticipate.
        if !out_path.starts_with(dest_dir) {
            return Err(anyhow!(
                "zip entry escapes extraction root: `{}` -> {}",
                name,
                out_path.display()
            ));
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .with_context(|| format!("mkdir {}", out_path.display()))?;
            continue;
        }
        if let Some(p) = out_path.parent() {
            std::fs::create_dir_all(p).with_context(|| format!("mkdir {}", p.display()))?;
        }
        let mut out =
            File::create(&out_path).with_context(|| format!("create {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("write {}", out_path.display()))?;
        count += 1;
    }
    if count == 0 {
        // A validation-clean zip that contains zero regular files (e.g.
        // just the `<title>/` dir entry) would pass Pass-1 layout
        // checks, but restoring it means wiping the live save and
        // uploading nothing — the user ends up with an empty save
        // folder, almost certainly not what they intended. Refuse
        // before the caller fsDeletes anything on the PS5.
        return Err(anyhow!(
            "zip contains no files under `{}/`; refusing to restore an empty save",
            expected_inner
        ));
    }
    Ok(SaveArchiveUnzipResp {
        inner_root: only_root,
        file_count: count,
    })
}

/// Turn a zip entry name into a relative `PathBuf` that's guaranteed to
/// stay under the extraction root. Returns `None` for traversal attempts
/// or otherwise suspicious shapes (absolute paths, embedded NULs, etc.).
fn sanitize_entry(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
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
        out.push(seg);
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// One isolated temp dir per test. Caller is responsible for the
    /// recursive remove on exit — we use a `static AtomicU32` to keep
    /// concurrent test invocations from colliding.
    fn fresh_tmp(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let id = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "ps5upload-test-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Parse a zip's central directory into a `{ path -> (is_dir, size) }`
    /// map. Lets each test assert on the exact set of entries produced.
    fn read_archive(zip: &Path) -> BTreeMap<String, (bool, u64)> {
        let mut a = ZipArchive::new(BufReader::new(File::open(zip).unwrap())).unwrap();
        let mut out = BTreeMap::new();
        for i in 0..a.len() {
            let e = a.by_index(i).unwrap();
            out.insert(e.name().to_string(), (e.is_dir(), e.size()));
        }
        out
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_entry("../etc/passwd").is_none());
        assert!(sanitize_entry("a/../../b").is_none());
        assert!(sanitize_entry("a/b\0c").is_none());
        assert!(sanitize_entry("").is_none());
    }

    #[test]
    fn sanitize_accepts_normal_paths() {
        assert_eq!(
            sanitize_entry("CUSA03474/sce_sys/icon0.png").unwrap(),
            PathBuf::from("CUSA03474").join("sce_sys").join("icon0.png")
        );
    }

    #[test]
    fn finder_noise_detection() {
        assert!(is_finder_noise("__MACOSX/foo"));
        assert!(is_finder_noise(".DS_Store"));
        assert!(is_finder_noise("CUSA03474/.DS_Store"));
        assert!(!is_finder_noise("CUSA03474/savefile.bin"));
    }

    /// Native PS5 save shape: a single title folder with `sce_sys/` +
    /// flat save files. The zip should mirror it 1:1 — no nesting
    /// surprises, no duplicate paths.
    #[test]
    fn zip_folder_native_ps5_layout_is_flat() {
        let tmp = fresh_tmp("native");
        let title = "CUSA12345";
        let root = tmp.join(title);
        touch(&root.join("savedata0"), b"\x01\x02\x03");
        touch(&root.join("sce_sys").join("icon0.png"), b"png");
        touch(&root.join("sce_sys").join("param.json"), b"{}");

        let zip = tmp.join("out.zip");
        zip_folder(&tmp, title, &zip).unwrap();

        let entries = read_archive(&zip);
        let names: Vec<&str> = entries.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "CUSA12345/",
                "CUSA12345/savedata0",
                "CUSA12345/sce_sys/",
                "CUSA12345/sce_sys/icon0.png",
                "CUSA12345/sce_sys/param.json",
            ],
            "native layout must be single-nested"
        );
        assert_eq!(entries["CUSA12345/savedata0"], (false, 3));
        assert_eq!(entries["CUSA12345/sce_sys/icon0.png"], (false, 3));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// PS2-Classic shape: PS5 stores the memcard image under an inner
    /// folder named the same as the outer title. The zip must
    /// faithfully reproduce that single level of nesting.
    #[test]
    fn zip_folder_ps2_classic_inner_nest() {
        let tmp = fresh_tmp("ps2classic");
        let title = "CUSA03474";
        let inner = tmp.join(title).join(title);
        touch(&inner.join("SLUS-20268.bin"), b"x".repeat(96).as_slice());
        touch(
            &inner.join("sdimg_SLUS-20268"),
            b"y".repeat(1024).as_slice(),
        );

        let zip = tmp.join("out.zip");
        zip_folder(&tmp, title, &zip).unwrap();

        let entries = read_archive(&zip);
        let names: Vec<&str> = entries.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "CUSA03474/",
                "CUSA03474/CUSA03474/",
                "CUSA03474/CUSA03474/SLUS-20268.bin",
                "CUSA03474/CUSA03474/sdimg_SLUS-20268",
            ],
            "PS2-classic must keep exactly one inner nest"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// User's actual `pro` zip shape: files exist at BOTH the 2-level
    /// and 3-level depths under the title folder, with different sizes
    /// at each depth (different memcard images). This pins the
    /// no-duplication-other-than-what-is-on-disk contract: every entry
    /// in the zip must correspond to a file/dir that exists in the
    /// source tree, nothing more, nothing less.
    #[test]
    fn zip_folder_preserves_multi_depth_without_duplicating() {
        let tmp = fresh_tmp("multidepth");
        let title = "CUSA03474";
        let outer = tmp.join(title).join(title);
        let inner = outer.join(title);
        touch(&outer.join("SLUS-20268.bin"), b"a".repeat(96).as_slice());
        touch(
            &outer.join("sdimg_SLUS-20268"),
            b"b".repeat(19_202_048).as_slice(),
        );
        touch(&inner.join("SLUS-20268.bin"), b"c".repeat(96).as_slice());
        touch(
            &inner.join("sdimg_SLUS-20268"),
            b"d".repeat(44_302_336).as_slice(),
        );

        let zip = tmp.join("out.zip");
        zip_folder(&tmp, title, &zip).unwrap();

        let entries = read_archive(&zip);
        let names: Vec<&str> = entries.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "CUSA03474/",
                "CUSA03474/CUSA03474/",
                "CUSA03474/CUSA03474/CUSA03474/",
                "CUSA03474/CUSA03474/CUSA03474/SLUS-20268.bin",
                "CUSA03474/CUSA03474/CUSA03474/sdimg_SLUS-20268",
                "CUSA03474/CUSA03474/SLUS-20268.bin",
                "CUSA03474/CUSA03474/sdimg_SLUS-20268",
            ],
            "expected 7 entries (3 dirs + 4 files) — one per real disk node"
        );
        assert_eq!(
            entries["CUSA03474/CUSA03474/sdimg_SLUS-20268"].1,
            19_202_048
        );
        assert_eq!(
            entries["CUSA03474/CUSA03474/CUSA03474/sdimg_SLUS-20268"].1,
            44_302_336
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Regression for the PS2-Classic "two sdimg files at different
    /// depths with the same basename" case the user encountered. Our
    /// zip writer + unzip_strict must round-trip both files with their
    /// distinct content intact. (macOS Finder Archive Utility has a
    /// bug where it zeros one of the two on extract — out of our
    /// control, but our tool's own restore path must not.)
    #[test]
    fn round_trip_same_basename_different_depths() {
        let src = fresh_tmp("rt-samename-src");
        let dst = fresh_tmp("rt-samename-dst");
        let title = "CUSA03474";
        let outer = src.join(title).join(title);
        let inner = outer.join(title);
        let outer_data = b"a".repeat(1024);
        let inner_data = b"b".repeat(1024);
        touch(&outer.join("sdimg_SLUS-20268"), &outer_data);
        touch(&inner.join("sdimg_SLUS-20268"), &inner_data);
        // Also include a sealed-key at each depth so the structure
        // matches what we see on a real PS2-Classic save.
        touch(&outer.join("SLUS-20268.bin"), &[0u8; 96]);
        touch(&inner.join("SLUS-20268.bin"), &[0xFFu8; 96]);

        let zip = src.join("rt.zip");
        zip_folder(&src, title, &zip).unwrap();
        unzip_strict(&zip, &dst, title).unwrap();

        let outer_restored = dst.join(title).join(title).join("sdimg_SLUS-20268");
        let inner_restored = dst
            .join(title)
            .join(title)
            .join(title)
            .join("sdimg_SLUS-20268");
        assert_eq!(std::fs::read(&outer_restored).unwrap(), outer_data);
        assert_eq!(std::fs::read(&inner_restored).unwrap(), inner_data);
        // Sealed keys too — both 96 bytes but distinct content.
        assert_eq!(
            std::fs::read(dst.join(title).join(title).join("SLUS-20268.bin")).unwrap(),
            vec![0u8; 96]
        );
        assert_eq!(
            std::fs::read(
                dst.join(title)
                    .join(title)
                    .join(title)
                    .join("SLUS-20268.bin")
            )
            .unwrap(),
            vec![0xFFu8; 96]
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    /// PS2-Classic / PS4-style backup flow: byte 0 == 0x01 image gets
    /// its `sdimg_` prefix stripped, sealed key kept, nested Sony
    /// bookkeeping dirs dropped.
    #[test]
    fn backup_finalize_ps4_style_strips_prefix_and_drops_nest() {
        let scratch = fresh_tmp("bf-ps4");
        let title = "CUSA03474";
        let title_dir = scratch.join(title);
        // PS4-style image: byte 0 = 0x01
        let mut img = vec![0x01u8];
        img.extend(b"a".repeat(1023));
        touch(&title_dir.join("sdimg_SLUS-20268"), &img);
        touch(&title_dir.join("SLUS-20268.bin"), &[0u8; 96]);
        // Nested Sony bookkeeping — drop on finalize.
        touch(
            &title_dir.join(title).join("sdimg_SLUS-20268"),
            &[0x01u8; 16],
        );
        touch(
            &title_dir.join(title).join(title).join("sdimg_SLUS-20268"),
            &[0x01u8; 16],
        );

        let resp = backup_finalize(&title_dir).unwrap();
        assert_eq!(resp.stripped_count, 1);
        assert!(resp.dropped_dirs >= 1);

        // Image was renamed.
        assert!(title_dir.join("SLUS-20268").is_file());
        assert!(!title_dir.join("sdimg_SLUS-20268").exists());
        // Sealed key untouched.
        assert!(title_dir.join("SLUS-20268.bin").is_file());
        // Nested dirs gone.
        assert!(!title_dir.join(title).exists());

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// PS5-native (byte 0 == 0x02): keep `sdimg_` prefix, preserve
    /// `sce_sys/` plaintext metadata.
    #[test]
    fn backup_finalize_ps5_native_keeps_prefix_and_sce_sys() {
        let scratch = fresh_tmp("bf-ps5");
        let title = "CUSA12345";
        let title_dir = scratch.join(title);
        let mut img = vec![0x02u8];
        img.extend(b"b".repeat(2047));
        touch(&title_dir.join("sdimg_savedata0"), &img);
        touch(&title_dir.join("sce_sys").join("icon0.png"), b"PNG");
        touch(&title_dir.join("sce_sys").join("param.sfo"), b"SFO");

        let resp = backup_finalize(&title_dir).unwrap();
        assert_eq!(resp.stripped_count, 0);
        assert_eq!(resp.dropped_dirs, 0);
        // 1 image + 2 metadata files
        assert_eq!(resp.kept_files, 3);

        // Image still prefixed.
        assert!(title_dir.join("sdimg_savedata0").is_file());
        // sce_sys preserved.
        assert!(title_dir.join("sce_sys").join("icon0.png").is_file());
        assert!(title_dir.join("sce_sys").join("param.sfo").is_file());

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Restore prep is the inverse of finalize for PS4-style: a bare
    /// image filename gets `sdimg_` re-added, sealed key + PS5-native
    /// `sdimg_*` + `sce_sys/*` pass through unchanged.
    #[test]
    fn restore_prepare_re_adds_sdimg_prefix_to_bare_images() {
        let scratch = fresh_tmp("rp-ps4");
        let title = "CUSA03474";
        let title_dir = scratch.join(title);
        // Resigner-format input: bare image + sealed key
        touch(&title_dir.join("SLUS-20268"), b"img");
        touch(&title_dir.join("SLUS-20268.bin"), b"key");
        // A PS5-native style item that should NOT be touched
        touch(&title_dir.join("sdimg_native"), b"x");
        // Metadata under sce_sys/
        touch(&title_dir.join("sce_sys").join("icon0.png"), b"png");

        let resp = restore_prepare(&title_dir).unwrap();
        assert_eq!(resp.renamed_count, 1);

        // Bare image got prefix re-added.
        assert!(title_dir.join("sdimg_SLUS-20268").is_file());
        assert!(!title_dir.join("SLUS-20268").exists());
        // Key untouched.
        assert!(title_dir.join("SLUS-20268.bin").is_file());
        // Pre-prefixed file untouched.
        assert!(title_dir.join("sdimg_native").is_file());
        // sce_sys untouched.
        assert!(title_dir.join("sce_sys").join("icon0.png").is_file());

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// User-reported regression (2026-05-13): PS2-Classic backup where
    /// the title folder on PS5 contains ONLY a nested wrapper subdir
    /// (no immediate files), e.g.:
    ///
    ///     CUSA03474/CUSA03474/SLUS-20268.bin
    ///     CUSA03474/CUSA03474/sdimg_SLUS-20268
    ///     CUSA03474/CUSA03474/CUSA03474/SLUS-20268.bin
    ///     CUSA03474/CUSA03474/CUSA03474/sdimg_SLUS-20268
    ///
    /// Earlier `backup_finalize` saw no immediate files at the title
    /// level and dropped everything as bookkeeping, leaving an empty
    /// folder in the zip. The flattener now descends through wrapper
    /// layers to find the real data layer.
    #[test]
    fn backup_finalize_flattens_nested_wrappers() {
        let scratch = fresh_tmp("bf-nested");
        let title = "CUSA03474";
        let title_dir = scratch.join(title);
        // Top-level has only a wrapper subdir.
        let depth1 = title_dir.join(title);
        let depth2 = depth1.join(title);
        let mut img1 = vec![0x01u8];
        img1.extend(b"slim-card-data".repeat(50));
        let mut img2 = vec![0x01u8];
        img2.extend(b"fat-card-data-different".repeat(40));
        touch(&depth1.join("SLUS-20268.bin"), &[0xAAu8; 96]);
        touch(&depth1.join("sdimg_SLUS-20268"), &img1);
        touch(&depth2.join("SLUS-20268.bin"), &[0xBBu8; 96]);
        touch(&depth2.join("sdimg_SLUS-20268"), &img2);

        let resp = backup_finalize(&title_dir).unwrap();
        assert_eq!(resp.stripped_count, 1, "PS4-style image should be renamed");

        // Title dir should now contain the flat data-layer files.
        let mut names: Vec<String> = std::fs::read_dir(&title_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["SLUS-20268".to_string(), "SLUS-20268.bin".to_string()],
            "title_dir must hold the depth-1 data layer's two files, stripped + flat"
        );
        // Content matches the depth-1 (shallowest) data layer.
        assert_eq!(std::fs::read(title_dir.join("SLUS-20268")).unwrap(), img1);
        assert_eq!(
            std::fs::read(title_dir.join("SLUS-20268.bin")).unwrap(),
            vec![0xAAu8; 96]
        );

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Defensive: backup_finalize on an empty scratch dir (e.g. the
    /// PS5 returned zero files for a stub save) must not panic. It
    /// should produce a no-op response — the standard "zip an empty
    /// title dir" outcome already exists upstream.
    #[test]
    fn backup_finalize_empty_title_dir_is_no_op() {
        let scratch = fresh_tmp("bf-empty");
        let title = "CUSAEMPTY";
        let title_dir = scratch.join(title);
        std::fs::create_dir_all(&title_dir).unwrap();
        let resp = backup_finalize(&title_dir).unwrap();
        assert_eq!(resp.kept_files, 0);
        assert_eq!(resp.stripped_count, 0);
        assert_eq!(resp.dropped_dirs, 0);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Defensive: backup_finalize where the title dir doesn't exist
    /// at all (caller passed a wrong path) errors cleanly rather than
    /// silently doing nothing.
    #[test]
    fn backup_finalize_missing_title_dir_errors() {
        let scratch = fresh_tmp("bf-missing");
        let title = "CUSANOTHERE";
        let missing = scratch.join(title); // not created
        let r = backup_finalize(&missing);
        assert!(r.is_err());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Regression: a leftover `.ps5upload_extract_tmp` from a previous
    /// crashed flatten run must surface as a clean error, not be
    /// silently re-used (which could mix old + new data).
    #[test]
    fn backup_finalize_leftover_staging_path_errors() {
        let scratch = fresh_tmp("bf-leftover");
        let title = "CUSALEFTOVER";
        let title_dir = scratch.join(title);
        // No top-level files → flatten kicks in.
        let wrapper = title_dir.join(title);
        touch(&wrapper.join("sdimg_SLUS-20268"), &[0x01u8; 100]);
        touch(&wrapper.join("SLUS-20268.bin"), &[0u8; 96]);
        // Plant a leftover staging dir from a prior crashed run.
        std::fs::create_dir(title_dir.join(".ps5upload_extract_tmp")).unwrap();
        let r = backup_finalize(&title_dir);
        assert!(r.is_err(), "should refuse to clobber leftover staging");
        assert!(r.unwrap_err().to_string().contains("staging"));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Defensive: a zip the user manually crafted (or a future
    /// payload variant) that includes a dot-extensioned file at the
    /// title level must NOT have `sdimg_` prepended on restore. Only
    /// extensionless bare files (PS2/PS4 disc-id images that lost
    /// their prefix during backup_finalize) are eligible for the
    /// re-prefix.
    #[test]
    fn restore_prepare_skips_dot_extensioned_files() {
        let scratch = fresh_tmp("rp-dotfiles");
        let title = "CUSA09999";
        let title_dir = scratch.join(title);
        // The canonical resigner-style pair: should get sdimg_ added.
        touch(&title_dir.join("SLUS-20268"), b"img");
        touch(&title_dir.join("SLUS-20268.bin"), b"key");
        // Files that look like metadata or user-added junk — must be
        // left alone, NOT renamed to sdimg_*.
        touch(&title_dir.join("README.txt"), b"hi");
        touch(&title_dir.join("notes.md"), b"todo");
        touch(&title_dir.join("param.sfo"), b"sfo");

        let resp = restore_prepare(&title_dir).unwrap();
        // Only the canonical extensionless bare file was renamed.
        assert_eq!(resp.renamed_count, 1);
        assert!(title_dir.join("sdimg_SLUS-20268").is_file());
        assert!(!title_dir.join("SLUS-20268").exists());
        // The dot-extensioned files stayed put with original names.
        assert!(title_dir.join("README.txt").is_file());
        assert!(title_dir.join("notes.md").is_file());
        assert!(title_dir.join("param.sfo").is_file());
        // Regression guard against accidental promotion.
        assert!(!title_dir.join("sdimg_README.txt").exists());
        assert!(!title_dir.join("sdimg_param.sfo").exists());

        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// End-to-end: real PS5 layout in scratch dir → finalize → zip →
    /// unzip → restore-prep → matches the original PS5 layout byte-
    /// for-byte after re-adding the `sdimg_` prefix.
    #[test]
    fn end_to_end_backup_then_restore_round_trips() {
        let backup_src = fresh_tmp("e2e-src");
        let backup_zip_dir = fresh_tmp("e2e-zip");
        let restore_dst = fresh_tmp("e2e-dst");
        let title = "CUSA03474";

        // Simulate what `transfer_download` lands on disk for a
        // PS2-Classic save (PS4-format image + sealed key + nested
        // Sony bookkeeping we want to drop).
        let title_dir = backup_src.join(title);
        let img_data = {
            let mut v = vec![0x01u8];
            v.extend(b"image-bytes-".repeat(100));
            v
        };
        let key_data = vec![0x42u8; 96];
        touch(&title_dir.join("sdimg_SLUS-20268"), &img_data);
        touch(&title_dir.join("SLUS-20268.bin"), &key_data);
        touch(
            &title_dir.join(title).join("sdimg_SLUS-20268"),
            b"nested-junk",
        );

        // Backup: finalize → zip.
        backup_finalize(&title_dir).unwrap();
        let zip = backup_zip_dir.join("CUSA03474.zip");
        zip_folder(&backup_src, title, &zip).unwrap();

        // Verify zip contents match the resigner-style two-file
        // pair (no nesting, no sdimg_ prefix).
        let entries = read_archive(&zip);
        let names: Vec<&str> = entries.keys().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "CUSA03474/",
                "CUSA03474/SLUS-20268",
                "CUSA03474/SLUS-20268.bin"
            ],
            "backup zip should be clean two-file pair, no nesting"
        );

        // Restore: unzip → prepare.
        unzip_strict(&zip, &restore_dst, title).unwrap();
        restore_prepare(&restore_dst.join(title)).unwrap();

        // After prep, the layout should match what we'd upload to PS5.
        let restored_img = std::fs::read(restore_dst.join(title).join("sdimg_SLUS-20268")).unwrap();
        let restored_key = std::fs::read(restore_dst.join(title).join("SLUS-20268.bin")).unwrap();
        assert_eq!(restored_img, img_data, "image bytes round-trip");
        assert_eq!(restored_key, key_data, "sealed key bytes round-trip");

        let _ = std::fs::remove_dir_all(&backup_src);
        let _ = std::fs::remove_dir_all(&backup_zip_dir);
        let _ = std::fs::remove_dir_all(&restore_dst);
    }

    /// Round-trip: zip a tree, unzip it back, byte-compare every file.
    /// Catches any silent corruption in the deflate path or any
    /// misordering between writer and reader.
    #[test]
    fn zip_then_unzip_is_byte_identical() {
        let src_tmp = fresh_tmp("rt-src");
        let dst_tmp = fresh_tmp("rt-dst");
        let title = "CUSA99999";
        let title_root = src_tmp.join(title);
        touch(&title_root.join("save.bin"), b"binary-data-\x00\x01\x02");
        touch(
            &title_root.join("sce_sys").join("icon0.png"),
            &vec![7u8; 4096],
        );
        touch(
            &title_root.join("inner").join("nested.dat"),
            &vec![0xAB; 1024],
        );

        let zip = src_tmp.join("rt.zip");
        zip_folder(&src_tmp, title, &zip).unwrap();
        unzip_strict(&zip, &dst_tmp, title).unwrap();

        // Compare every original file against the extracted copy.
        for rel in &["save.bin", "sce_sys/icon0.png", "inner/nested.dat"] {
            let original =
                std::fs::read(title_root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
                    .unwrap();
            let restored = std::fs::read(
                dst_tmp
                    .join(title)
                    .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
            )
            .unwrap();
            assert_eq!(original, restored, "byte mismatch on {rel}");
        }

        let _ = std::fs::remove_dir_all(&src_tmp);
        let _ = std::fs::remove_dir_all(&dst_tmp);
    }

    /// Strict-unzip rejects layouts that don't have a single
    /// top-level folder named `expected_inner` — verifies every
    /// branch of the validator.
    #[test]
    fn unzip_strict_rejects_bad_layouts() {
        let tmp = fresh_tmp("strict");

        // Build a zip with a flat file at the root → must reject.
        {
            let src = fresh_tmp("strict-flat");
            touch(&src.join("loose.bin"), b"x");
            let z = src.join("bad.zip");
            let out = File::create(&z).unwrap();
            let mut w = ZipWriter::new(BufWriter::new(out));
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            w.start_file("loose.bin", opts).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
            let r = unzip_strict(&z, &tmp, "CUSA00001");
            assert!(r.is_err(), "flat root must be rejected");
            assert!(r.unwrap_err().to_string().contains("root"));
            let _ = std::fs::remove_dir_all(&src);
        }

        // Wrong top-level folder name → must reject.
        {
            let src = fresh_tmp("strict-wrongname");
            let z = src.join("bad.zip");
            let out = File::create(&z).unwrap();
            let mut w = ZipWriter::new(BufWriter::new(out));
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            w.add_directory("OTHER/", opts).unwrap();
            w.start_file("OTHER/a.bin", opts).unwrap();
            w.write_all(b"y").unwrap();
            w.finish().unwrap();
            let r = unzip_strict(&z, &tmp, "CUSA00001");
            assert!(r.is_err(), "wrong top folder must be rejected");
            let _ = std::fs::remove_dir_all(&src);
        }

        // Dir-only zip (CUSA/ entry, no files) — must reject so we
        // don't blow away the live save with nothing.
        {
            let src = fresh_tmp("strict-empty");
            let z = src.join("bad.zip");
            let out = File::create(&z).unwrap();
            let mut w = ZipWriter::new(BufWriter::new(out));
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            w.add_directory("CUSA00001/", opts).unwrap();
            w.finish().unwrap();
            let r = unzip_strict(&z, &tmp, "CUSA00001");
            assert!(r.is_err(), "dir-only zip must be rejected");
            assert!(
                r.unwrap_err().to_string().contains("no files"),
                "error should mention empty contents"
            );
            let _ = std::fs::remove_dir_all(&src);
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Regression: zips produced by `zip_folder` must mark directory
    /// entries with the unix execute bit (0o755), not 0o644. Without
    /// it, external `unzip(1)` reproduces `drw-r--r--` dirs that the
    /// user can't `cd` into.
    #[test]
    fn zip_folder_sets_executable_dir_mode() {
        let tmp =
            std::env::temp_dir().join(format!("ps5upload-test-zipperms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let title = "CUSA03474";
        let inner = tmp.join(title);
        std::fs::create_dir_all(inner.join("sce_sys")).unwrap();
        std::fs::write(inner.join("savefile.bin"), b"hello").unwrap();
        std::fs::write(inner.join("sce_sys").join("icon0.png"), b"png").unwrap();

        let zip_path = tmp.join("out.zip");
        zip_folder(&tmp, title, &zip_path).expect("zip_folder ok");

        let mut archive = ZipArchive::new(BufReader::new(File::open(&zip_path).unwrap())).unwrap();
        let mut saw_dir = false;
        let mut saw_file = false;
        for i in 0..archive.len() {
            let entry = archive.by_index(i).unwrap();
            let mode = entry.unix_mode().unwrap_or(0) & 0o777;
            if entry.is_dir() {
                saw_dir = true;
                assert!(
                    mode & 0o111 != 0,
                    "directory `{}` has no execute bit (mode {:o})",
                    entry.name(),
                    mode
                );
                assert_eq!(mode, 0o755, "directory mode should be 0o755");
            } else {
                saw_file = true;
                assert_eq!(mode, 0o644, "file mode should be 0o644");
            }
        }
        assert!(saw_dir && saw_file, "test fixture must contain both kinds");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
