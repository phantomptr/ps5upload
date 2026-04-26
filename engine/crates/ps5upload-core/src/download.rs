//! PS5 → host download helpers.
//!
//! Mirror image of the FTX2 upload path: walk a remote file or
//! directory, pull each file via chunked FS_READ on the management
//! port, write to a local destination directory. Built on top of the
//! existing `fs_ops::list_dir` + `fs_ops::fs_read` helpers — no new
//! payload-side protocol needed.
//!
//! Limitations vs upload:
//! - Single management port, single connection per FS_READ → no
//!   pipelining. Throughput is RTT × chunk-size / second; on LAN with
//!   2 MiB chunks, that's ~250-500 MiB/s realistic, much slower than
//!   the FTX2 upload path's pipelined 32 MiB shards.
//! - No resume / no per-shard digest. If the connection drops mid-
//!   download, the partial local file is retained but the next attempt
//!   starts over from offset 0.
//!
//! These limitations are acceptable for the Library + FileSystem
//! "save a copy locally" use case. A streaming/pipelined download
//! protocol is future work if multi-GB game-image downloads become a
//! routine workflow.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::fs_ops::{fs_read, list_dir, DirEntry, ListDirOptions};

/// Largest chunk we ask the payload for in one FS_READ. Matches the
/// payload's compile-time `FS_READ_MAX_BYTES` (2 MiB) — asking for
/// more is silently truncated, asking for less just makes more round
/// trips. Tracks the payload constant; bump together if the payload's
/// cap ever changes.
pub const DOWNLOAD_CHUNK_SIZE: u64 = 2 * 1024 * 1024;

/// One file the host needs to pull. `rel_path` is relative to the
/// download root (the source path the user picked). Built by
/// `enumerate_download_set` and consumed by the per-file copier.
#[derive(Debug, Clone)]
pub struct DownloadEntry {
    pub remote_path: String,
    pub rel_path: String,
    pub size: u64,
}

/// One non-file entry the walker skipped. Surfaced so the UI can
/// tell the user "we didn't pull these N symlinks / special files"
/// instead of silently producing a structurally-different local copy.
#[derive(Debug, Clone)]
pub struct SkippedEntry {
    pub remote_path: String,
    pub kind: String,
}

/// What `enumerate_download_set` returned: the manifest of files to
/// pull plus the set of entries the walker skipped (symlinks,
/// special files). The caller decides whether to surface skips as
/// warnings; the engine handler stitches them into the job's Done
/// state so the UI can render them.
#[derive(Debug, Clone)]
pub struct DownloadPlan {
    pub manifest: Vec<DownloadEntry>,
    pub skipped: Vec<SkippedEntry>,
}

/// What the user picked to download. Folder = walk; file = single
/// entry. The shape is decided by the caller (Library/FileSystem
/// already knows whether the row is a file or a directory) so we
/// avoid a redundant remote stat round-trip just to classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadKind {
    File,
    Folder,
}

/// Build the download manifest. For a file this is a single entry;
/// for a folder this walks the remote tree (depth-first) and returns
/// every regular file under it.
///
/// Errors propagate from the underlying `list_dir` calls — a single
/// permission denied surfaces here rather than in the middle of the
/// per-file loop, so the UI can render "couldn't enumerate
/// `<subdir>`" before any bytes have moved.
pub fn enumerate_download_set(
    addr: &str,
    src_path: &str,
    kind: DownloadKind,
) -> Result<DownloadPlan> {
    let basename = remote_basename(src_path);
    let mut manifest = Vec::new();
    let mut skipped = Vec::new();
    match kind {
        DownloadKind::File => {
            // For a single file the "rel_path" is just its basename
            // — that becomes `<dest_dir>/<basename>` on the host. We
            // need the size, which the parent's list_dir hands us
            // for free.
            let parent = remote_parent(src_path);
            let listing = list_dir(
                addr,
                &parent,
                ListDirOptions {
                    offset: 0,
                    limit: u64::MAX,
                },
            )
            .with_context(|| format!("list_dir {parent} (parent of {src_path})"))?;
            let entry = listing
                .entries
                .into_iter()
                .find(|e| e.name == basename)
                .ok_or_else(|| anyhow::anyhow!("source file not found: {src_path}"))?;
            if entry.kind != "file" {
                anyhow::bail!(
                    "source is not a regular file (kind={}): {src_path}",
                    entry.kind
                );
            }
            manifest.push(DownloadEntry {
                remote_path: src_path.to_string(),
                rel_path: basename,
                size: entry.size,
            });
        }
        DownloadKind::Folder => {
            walk_remote_dir(addr, src_path, &basename, &mut manifest, &mut skipped)?;
        }
    }
    Ok(DownloadPlan { manifest, skipped })
}

fn walk_remote_dir(
    addr: &str,
    remote_dir: &str,
    rel_prefix: &str,
    out: &mut Vec<DownloadEntry>,
    skipped: &mut Vec<SkippedEntry>,
) -> Result<()> {
    let listing = list_dir(
        addr,
        remote_dir,
        ListDirOptions {
            offset: 0,
            limit: u64::MAX,
        },
    )
    .with_context(|| format!("list_dir {remote_dir}"))?;
    // Sort for stable, predictable order — matches the engine's
    // upload `walk_plan` so users see the same ordering on both
    // directions of transfer.
    let mut entries: Vec<DirEntry> = listing.entries;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in entries {
        let child_rel = if rel_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{rel_prefix}/{}", entry.name)
        };
        let child_remote = format!("{}/{}", remote_dir.trim_end_matches('/'), entry.name);
        match entry.kind.as_str() {
            "dir" => walk_remote_dir(addr, &child_remote, &child_rel, out, skipped)?,
            "file" => out.push(DownloadEntry {
                remote_path: child_remote,
                rel_path: child_rel,
                size: entry.size,
            }),
            // Symlinks + special files: we only ship regular-file
            // bytes (no protocol for inode metadata round-trip), so
            // these get tracked as skipped entries the caller can
            // surface to the user. Caller decides whether to show a
            // warning or roll the count into a banner.
            other => skipped.push(SkippedEntry {
                remote_path: child_remote,
                kind: other.to_string(),
            }),
        }
    }
    Ok(())
}

/// Pull every entry in `manifest` into `dest_dir`. Creates intermediate
/// directories as needed. The progress counter (when supplied) is
/// `fetch_add`-ed by the caller's preferred unit (we add exact
/// byte-counts of each chunk just received, so the host-side UI can
/// drive a real-time speed/ETA readout).
///
/// Per-file flow:
///   1. Open `<dest_dir>/<rel_path>` for writing (truncating any
///      existing file — same overwrite semantics as upload).
///   2. Loop: fs_read(remote_path, offset, CHUNK) → write to file →
///      offset += received_bytes.
///   3. Stop when received_bytes < CHUNK (EOF, since the payload's
///      FS_READ returns short reads only at end-of-file or when the
///      cap is hit; we ask for exactly the cap so a short read
///      reliably means EOF).
///
/// Aborts on the first error. Per-file writes go to `<path>.part`
/// and only get renamed onto the final `<path>` after the size
/// verification passes — so a failed download leaves a `.part` file
/// behind for inspection but does NOT clobber whatever the user had
/// at the final destination from a previous run. Same atomic-rename
/// pattern persistence.rs uses for its JSON stores.
pub fn download_to_local(
    addr: &str,
    dest_dir: &Path,
    manifest: &[DownloadEntry],
    progress_bytes: Option<&Arc<AtomicU64>>,
) -> Result<u64> {
    use std::fs;
    use std::io::Write;

    let mut total_written = 0u64;
    for entry in manifest {
        let local_path = local_dest_for(dest_dir, &entry.rel_path);
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        // Write to a sibling `.part` file; rename onto the final
        // path only after the per-file size check below passes. Any
        // prior file at `local_path` is preserved unless this
        // download succeeds.
        //
        // Use `with_file_name` not `with_extension` — the latter
        // mishandles edge cases:
        //   foo            → with_extension(".part") = foo.part   ok
        //   foo.bin        → with_extension(".part") = foo.part   *bug:
        //                    overwrites the original suffix
        //   .gitignore     → with_extension(".part") = .part      *bug:
        //                    treats the dot as the extension boundary
        //   archive.tar.gz → with_extension(".part") = archive.tar.part
        //                    (truncates the multi-dot name)
        //
        // Appending `.part` to the full file_name keeps the name
        // identifiable in the user's file manager + means cleanup
        // tools that grep for `*.part` find every leftover.
        let part_path = match local_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => local_path.with_file_name(format!("{name}.part")),
            None => local_path.with_extension("part"),
        };
        // Best-effort sweep of any leftover `.part` orphan from a
        // prior failed attempt. Without this, a user retrying a
        // failed download accumulates one orphan per attempt next
        // to the real file. The sweep targets only the exact
        // part_path we're about to write, so unrelated `.part`
        // files in the same dir aren't touched.
        if part_path.exists() {
            let _ = fs::remove_file(&part_path);
        }
        let mut file = fs::File::create(&part_path)
            .with_context(|| format!("create {}", part_path.display()))?;

        let mut offset: u64 = 0;
        // Authoritative termination is by `entry.size` (pre-stat'd
        // from the parent's list_dir). Trusting "short read = EOF"
        // is fragile because the payload's FS_READ on PS5 makes one
        // syscall and returns whatever pread() returned — a single
        // short read mid-file (filesystem checkpoint, blocking I/O,
        // anything that returns less than requested) would silently
        // truncate the local copy and report Done. Using the known
        // size and verifying after-the-fact catches every
        // truncation case.
        let expected = entry.size;
        while offset < expected {
            let want = std::cmp::min(DOWNLOAD_CHUNK_SIZE, expected - offset);
            let bytes = fs_read(addr, &entry.remote_path, offset, want)
                .with_context(|| format!("fs_read {} @ {offset}", entry.remote_path))?;
            if bytes.is_empty() {
                // The remote file shrank under us, or fs_read failed
                // silently — either way the local file is now shorter
                // than the manifest promised. Bail with an explicit
                // error rather than reporting Done with a truncated
                // copy.
                anyhow::bail!(
                    "fs_read returned 0 bytes at offset {offset} of {} (expected {expected} total) — \
                     remote file may have changed or been deleted mid-download",
                    entry.remote_path
                );
            }
            file.write_all(&bytes)
                .with_context(|| format!("write {}", part_path.display()))?;
            let n = bytes.len() as u64;
            offset += n;
            total_written += n;
            if let Some(counter) = progress_bytes {
                counter.fetch_add(n, Ordering::Relaxed);
            }
        }
        // Defensive: fsync via drop, then verify the on-disk size
        // matches expected. Caught here, the user sees a clear
        // mismatch error; missed, they'd discover the truncation
        // only when trying to use the downloaded file.
        drop(file);
        let written = std::fs::metadata(&part_path)
            .with_context(|| format!("stat {} after write", part_path.display()))?
            .len();
        if written != expected {
            anyhow::bail!(
                "downloaded {} bytes but expected {} for {} — possible truncation (partial at {})",
                written,
                expected,
                entry.remote_path,
                part_path.display()
            );
        }
        // Atomic-ish promotion: rename .part onto the final path.
        // On Unix this is atomic across the same filesystem. On
        // Windows, fs::rename fails when the destination exists,
        // so we remove first — but a Windows file-lock (media
        // player, indexer, antivirus quarantine) makes the remove
        // itself fail, and the subsequent rename then surfaces a
        // generic "rename failed" that hides the real cause.
        // Handle the cases distinctly:
        //   - remove fails → bail with the remove error so the
        //     user sees "couldn't replace existing file (locked
        //     by another process)" instead of a misleading
        //     post-hoc "rename failed".
        //   - rename fails with EXDEV → the .part lives on a
        //     different filesystem than local_path (Windows
        //     reparse point, mac bind-mount). Surface a distinct
        //     message explaining that the bytes ARE on disk at
        //     the .part path so the user knows the download
        //     itself succeeded.
        if local_path.exists() {
            if let Err(e) = fs::remove_file(&local_path) {
                anyhow::bail!(
                    "downloaded {} bytes successfully to {}, but couldn't replace the existing file at {}: {e} (file may be open in another program)",
                    expected,
                    part_path.display(),
                    local_path.display()
                );
            }
        }
        if let Err(e) = fs::rename(&part_path, &local_path) {
            // EXDEV / cross-device — bytes ARE on disk, just at
            // the wrong path. Tell the user where they landed.
            let cross_device = matches!(
                e.raw_os_error(),
                Some(n) if n == 18 /* EXDEV on Linux+macOS */
            );
            if cross_device {
                anyhow::bail!(
                    "downloaded {} bytes to {}, but couldn't promote to {} (cross-filesystem rename refused) — the file IS on disk at the .part path; rename it manually",
                    expected,
                    part_path.display(),
                    local_path.display()
                );
            }
            return Err(anyhow::anyhow!(
                "downloaded {} bytes to {}, but couldn't promote to {}: {e}",
                expected,
                part_path.display(),
                local_path.display()
            ));
        }
    }
    Ok(total_written)
}

fn remote_basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

fn remote_parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent.to_string(),
        // Root file (`/foo`) → parent is "/"; the payload accepts
        // FS_LIST_DIR("/") so this is fine.
        _ => "/".to_string(),
    }
}

fn local_dest_for(dest_dir: &Path, rel_path: &str) -> PathBuf {
    let mut out = PathBuf::from(dest_dir);
    // Forward-slash relpaths from PS5; convert to host separators
    // when pushing components so Windows produces `\` paths.
    for part in rel_path.split('/') {
        if !part.is_empty() {
            out.push(part);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_basename_handles_trailing_slash() {
        assert_eq!(remote_basename("/data/foo/bar"), "bar");
        assert_eq!(remote_basename("/data/foo/bar/"), "bar");
        assert_eq!(remote_basename("standalone"), "standalone");
    }

    #[test]
    fn remote_parent_walks_up_one() {
        assert_eq!(remote_parent("/data/foo/bar"), "/data/foo");
        assert_eq!(remote_parent("/data/foo/bar/"), "/data/foo");
        assert_eq!(remote_parent("/foo"), "/");
    }

    #[test]
    fn local_dest_uses_host_separators() {
        let base = PathBuf::from("/host/dest");
        let resolved = local_dest_for(&base, "sub/dir/file.bin");
        // Use components rather than string compare so this passes
        // identically on Windows + Unix.
        let parts: Vec<_> = resolved
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(parts.last().unwrap(), "file.bin");
        assert!(parts.contains(&"sub".to_string()));
        assert!(parts.contains(&"dir".to_string()));
    }
}
