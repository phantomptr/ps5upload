//! PS5 → host download helpers.
//!
//! Walk a remote file or directory, pull each file via chunked FS_READ on
//! the management port, write to a local destination directory.
//!
//! Throughput + robustness (2026-06, rewrite): one connection is reused
//! across every chunk AND every file (the old path opened a fresh TCP
//! connection per 2 MiB chunk — the dominant cost, worst on folder dumps),
//! and `DOWNLOAD_PIPELINE_DEPTH` FS_READ requests are kept in flight so the
//! request RTT overlaps the payload's disk read. A mid-download connection
//! drop reconnects and resumes from the last durably-written byte (FS_READ
//! is stateless by offset), the same resume-on-drop robustness the upload
//! queue has. Hardware-measured on a Fat PS5: single 1 GiB file ~40-50 →
//! ~96 MiB/s (≈ wire speed), real game folders ~58 MiB/s. Many-tiny-file
//! folders stay round-trip-bound (one request per file); multi-stream
//! download (split the manifest across connections) addresses that.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;

use crate::connection::Connection;
use crate::fs_ops::{list_dir, DirEntry, ListDirOptions};
use crate::transfer::is_retryable_transfer_error;

/// Largest chunk we ask the payload for in one FS_READ. Matches the
/// payload's compile-time `FS_READ_MAX_BYTES` (2 MiB) — asking for
/// more is silently truncated, asking for less just makes more round
/// trips. Tracks the payload constant; bump together if the payload's
/// cap ever changes.
pub const DOWNLOAD_CHUNK_SIZE: u64 = 2 * 1024 * 1024;

/// How many FS_READ requests we keep in flight on the single reused
/// connection. The old path opened a NEW TCP connection per 2 MiB chunk and
/// waited for each response before asking for the next — so the wire sat idle
/// during every connect + request round-trip (measured ~40-50 MiB/s single
/// file, 3-13 MiB/s on many-small-file folders vs ftpsrv's ~90-100). Reusing
/// one connection kills the per-chunk connect; pipelining `DEPTH` requests
/// ahead hides the request RTT so recv overlaps the payload's disk read.
/// 4 × 2 MiB = 8 MiB of outstanding requests — enough to saturate a gigabit
/// LAN without unbounded host buffering.
const DOWNLOAD_PIPELINE_DEPTH: usize = 4;

/// Per-read I/O deadline on the reused connection. Matches the transfer
/// path's default; a stalled payload surfaces instead of hanging forever.
const DOWNLOAD_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Reconnect attempts before a download gives up. FS_READ is stateless
/// (every request carries its own offset), so a dropped connection just
/// reconnects and resumes from the last byte written — the same
/// resume-on-drop robustness the upload queue has.
const DOWNLOAD_MAX_RECONNECTS: u32 = 6;

/// Send one FS_READ request on an existing connection WITHOUT reading the
/// response — the producer half of the pipeline.
fn fs_read_send(conn: &mut Connection, path: &str, offset: u64, limit: u64) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "offset": offset,
        "limit": limit,
    }))
    .context("serialize fs_read body")?;
    conn.send_frame(FrameType::FsRead, &body)
        .with_context(|| format!("send FS_READ {path} @ {offset}"))
}

/// Read one FS_READ_ACK response into `buf`, returning the body length.
/// Bails on an ERROR frame (carries the payload's reason) or a wrong frame
/// type, draining the body first to keep the stream framed.
fn fs_read_recv_into(conn: &mut Connection, buf: &mut [u8], path: &str) -> Result<usize> {
    let hdr = conn.recv_header().context("recv FS_READ_ACK header")?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    let len = hdr.body_len as usize;
    if ft == FrameType::Error {
        let mut ebuf = vec![0u8; len];
        if len > 0 {
            conn.recv_body_exact(&mut ebuf)?;
        }
        bail!(
            "payload rejected FS_READ({path}): {}",
            String::from_utf8_lossy(&ebuf)
        );
    }
    if ft != FrameType::FsReadAck {
        conn.drain_body(hdr.body_len)?;
        bail!("expected FS_READ_ACK for {path}, got {ft:?}");
    }
    if len > buf.len() {
        conn.drain_body(hdr.body_len)?;
        bail!("FS_READ_ACK body {len} exceeds chunk buffer {}", buf.len());
    }
    if len > 0 {
        conn.recv_body_exact(&mut buf[..len])?;
    }
    Ok(len)
}

/// Pull one file's `[*offset, expected)` range over `conn`, writing
/// sequentially to `file` (positioned at `*offset`). Pipelines up to
/// `DOWNLOAD_PIPELINE_DEPTH` FS_READ requests to hide the request RTT, and
/// advances `*offset` as bytes are durably written so a caller that
/// reconnects resumes exactly where this left off — no double-write, no gap.
///
/// A short read (body shorter than requested) means the remote file shrank
/// mid-pull: the outstanding pipelined requests are now misaligned, so we
/// bail. That's a non-retryable protocol error (re-reading gets the same
/// short file), distinct from a connection drop the caller retries.
fn download_file_range(
    conn: &mut Connection,
    path: &str,
    expected: u64,
    file: &mut std::fs::File,
    offset: &mut u64,
    total_written: &mut u64,
    progress: Option<&Arc<AtomicU64>>,
) -> Result<()> {
    use std::io::Write;
    let mut next_req = *offset;
    // Requested lengths, in send order — responses arrive in the same order
    // on a single connection.
    let mut inflight: VecDeque<u64> = VecDeque::new();
    let mut buf = vec![0u8; DOWNLOAD_CHUNK_SIZE as usize];
    loop {
        // Top up the in-flight window.
        while inflight.len() < DOWNLOAD_PIPELINE_DEPTH && next_req < expected {
            let want = std::cmp::min(DOWNLOAD_CHUNK_SIZE, expected - next_req);
            fs_read_send(conn, path, next_req, want)?;
            inflight.push_back(want);
            next_req += want;
        }
        let Some(want) = inflight.pop_front() else {
            break; // window empty and nothing left to request → done
        };
        let n = fs_read_recv_into(conn, &mut buf[..want as usize], path)? as u64;
        if n == 0 {
            bail!(
                "fs_read returned 0 bytes at offset {} of {path} (expected {expected}) — \
                 remote file may have changed or been deleted mid-download",
                *offset
            );
        }
        file.write_all(&buf[..n as usize])
            .context("write downloaded chunk")?;
        *offset += n;
        *total_written += n;
        if let Some(c) = progress {
            c.fetch_add(n, Ordering::Relaxed);
        }
        if n < want {
            bail!(
                "short fs_read ({n} < {want}) at offset {} of {path} — file changed mid-download",
                *offset
            );
        }
    }
    Ok(())
}

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
            // for free. Paginate: the payload caps FS_LIST_DIR at 256
            // entries/page, so a one-shot listing would miss a target
            // file that sorts beyond entry 256 and report it "not found".
            let parent = remote_parent(src_path);
            let entry = list_dir_all(addr, &parent)
                .with_context(|| format!("list_dir {parent} (parent of {src_path})"))?
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
            walk_remote_dir(addr, src_path, &basename, &mut manifest, &mut skipped, 0)?;
        }
    }
    Ok(DownloadPlan { manifest, skipped })
}

/// Maximum recursion depth for `walk_remote_dir`. Matches the payload-side
/// `rm_rf_op` / `chmod_rf` cap of 64. Bounds stack usage against:
///   - genuinely deeply-nested directories (Unreal asset trees, etc.);
///   - PS5-side filesystem cycles (rare but possible with bind-mounts);
///   - hostile or buggy modified payloads returning crafted FS_LIST_DIR
///     entries that loop back on themselves.
///
/// Without this, recursion would stack-overflow the engine process
/// before any of those cases produced a clean error.
const WALK_REMOTE_MAX_DEPTH: u32 = 64;

/// The payload clamps every FS_LIST_DIR response to this many entries
/// (`runtime.c` page cap), setting `truncated` only when its response
/// buffer fills. A directory with more children than this must be read
/// in pages — see `list_dir_all`.
const REMOTE_LIST_PAGE: u64 = 256;

/// List *every* entry in a remote directory, paging in `REMOTE_LIST_PAGE`
/// chunks until exhausted. The download path previously passed
/// `limit: u64::MAX` and read a single page, silently dropping every
/// entry past the payload's 256-cap — truncating downloads of any folder
/// with >256 children, and making single-file downloads spuriously fail
/// for files beyond entry 256. The break condition mirrors
/// `fs_ops::list_remote_scoped`: stop on an empty page or a *short*
/// (< page) page that wasn't buffer-truncated, so an exact multiple of
/// 256 still triggers one more (empty) request rather than stopping early.
fn list_dir_all(addr: &str, dir: &str) -> Result<Vec<DirEntry>> {
    paginate_entries(|offset| {
        let listing = list_dir(
            addr,
            dir,
            ListDirOptions {
                offset,
                limit: REMOTE_LIST_PAGE,
            },
        )
        .with_context(|| format!("list_dir {dir} (offset {offset})"))?;
        Ok((listing.entries, listing.truncated))
    })
}

/// Pure pagination loop, factored out of `list_dir_all` so the
/// break-condition (the exact spot the truncation bug lived) is
/// unit-testable without a live payload. `fetch(offset)` returns one
/// `REMOTE_LIST_PAGE`-sized page as `(entries, truncated)`.
fn paginate_entries<F>(mut fetch: F) -> Result<Vec<DirEntry>>
where
    F: FnMut(u64) -> Result<(Vec<DirEntry>, bool)>,
{
    let mut all = Vec::new();
    let mut offset = 0u64;
    loop {
        let (entries, truncated) = fetch(offset)?;
        let n = entries.len() as u64;
        all.extend(entries);
        offset += n;
        // Natural end of a listing is an empty page or a *short* page
        // (< REMOTE_LIST_PAGE) that wasn't buffer-truncated. A full page
        // (truncated or not) means "ask again" — an exact multiple of
        // 256 must page once more (and get an empty page) rather than
        // stop early.
        if n == 0 || (!truncated && n < REMOTE_LIST_PAGE) {
            break;
        }
    }
    Ok(all)
}

fn walk_remote_dir(
    addr: &str,
    remote_dir: &str,
    rel_prefix: &str,
    out: &mut Vec<DownloadEntry>,
    skipped: &mut Vec<SkippedEntry>,
    depth: u32,
) -> Result<()> {
    if depth > WALK_REMOTE_MAX_DEPTH {
        anyhow::bail!(
            "remote walk depth cap {} exceeded at {}; refusing to recurse further \
             (possible PS5 directory cycle or pathological nesting)",
            WALK_REMOTE_MAX_DEPTH,
            remote_dir
        );
    }
    // Paginated — a one-shot `limit: u64::MAX` listing is clamped to 256
    // by the payload and would silently drop every entry past 256.
    let mut entries: Vec<DirEntry> =
        list_dir_all(addr, remote_dir).with_context(|| format!("list_dir {remote_dir}"))?;
    // Sort for stable, predictable order — matches the engine's
    // upload `walk_plan` so users see the same ordering on both
    // directions of transfer.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in entries {
        let child_rel = if rel_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{rel_prefix}/{}", entry.name)
        };
        let child_remote = format!("{}/{}", remote_dir.trim_end_matches('/'), entry.name);
        match entry.kind.as_str() {
            "dir" => walk_remote_dir(addr, &child_remote, &child_rel, out, skipped, depth + 1)?,
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
    use std::io::Seek;

    let mut total_written = 0u64;
    // ONE connection reused across every chunk AND every file — the single
    // biggest win over the old connect-per-2-MiB path (worst on folder
    // dumps, where per-file reconnects dominated). Lazily (re)opened; a drop
    // mid-download reconnects and resumes from the last byte written.
    let mut conn: Option<Connection> = None;
    for entry in manifest {
        let local_path = local_dest_for(dest_dir, &entry.rel_path).with_context(|| {
            format!(
                "rejecting download of {} — unsafe rel_path {:?}",
                entry.remote_path, entry.rel_path
            )
        })?;
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
            // Don't silently swallow — if the existing entry is a dir
            // (e.g. a symlink-attack edge case) or locked by another
            // process (Windows file lock from a previous attempt that
            // hasn't released yet), the caller needs a clear error
            // rather than a cryptic "create failed" further down.
            if let Err(e) = fs::remove_file(&part_path) {
                eprintln!(
                    "[download] WARN: failed to clean up orphan {}: {}",
                    part_path.display(),
                    e
                );
            }
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
        // Pull this file over the reused connection, pipelined. On a
        // connection-level error (TCP drop, timeout), reconnect and resume
        // from `offset` — FS_READ is stateless so the new connection just
        // re-requests the remaining range. `offset` only advances after a
        // durable write, so resume never double-writes or gaps. Protocol
        // errors (short read = file changed, payload ERROR frame) are NOT
        // retried — re-reading can't fix them.
        let mut reconnects = 0u32;
        loop {
            if conn.is_none() {
                let c = Connection::connect(addr)
                    .with_context(|| format!("connect {addr} for download"))?;
                let _ = c.set_io_timeout(DOWNLOAD_IO_TIMEOUT);
                conn = Some(c);
            }
            // Position the local file at the resume point (no-op on the
            // first pass; after a reconnect this rewinds writes that never
            // landed because `offset` only counts durable bytes).
            file.seek(std::io::SeekFrom::Start(offset))
                .with_context(|| format!("seek {} to {offset}", part_path.display()))?;
            let c = conn.as_mut().expect("just set");
            match download_file_range(
                c,
                &entry.remote_path,
                expected,
                &mut file,
                &mut offset,
                &mut total_written,
                progress_bytes,
            ) {
                Ok(()) => break,
                Err(e) => {
                    // Drop the (possibly half-framed) connection.
                    conn = None;
                    if reconnects < DOWNLOAD_MAX_RECONNECTS && is_retryable_transfer_error(&e) {
                        reconnects += 1;
                        eprintln!(
                            "[download] {} @ {offset}: {e:#}; reconnect {reconnects}/{DOWNLOAD_MAX_RECONNECTS}",
                            entry.remote_path
                        );
                        continue;
                    }
                    return Err(e).with_context(|| {
                        format!("download {} @ {offset}", entry.remote_path)
                    });
                }
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
            // The errno differs per OS: EXDEV is 18 on Linux/macOS, but
            // Windows' MoveFileExW reports a cross-volume move as
            // ERROR_NOT_SAME_DEVICE (17). cfg-gate so the diagnostic
            // fires on every host (and so we don't collide with Unix
            // EEXIST=17).
            #[cfg(windows)]
            const CROSS_DEVICE_ERRNO: i32 = 17; // ERROR_NOT_SAME_DEVICE
            #[cfg(not(windows))]
            const CROSS_DEVICE_ERRNO: i32 = 18; // EXDEV
            let cross_device = e.raw_os_error() == Some(CROSS_DEVICE_ERRNO);
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

/// Build a local destination path from `dest_dir` + a forward-slash-separated
/// `rel_path` from the PS5. Each component is validated to ensure the
/// returned path stays under `dest_dir` — a malicious or buggy modified
/// payload could otherwise return `FS_LIST_DIR` entries with names like
/// `..` or `C:` and write outside the user-chosen destination.
///
/// Rejected component shapes:
///   - `.` or `..` — directory traversal
///   - any `/` or `\\` — embedded separator (already split, but the
///     backslash check defends against PS5-side names like
///     `foo\\bar` slipping through)
///   - any `:` (Windows) — drive-letter or NTFS alternate-data-stream
///   - leading `\\\\` — UNC path on Windows
///   - any embedded NUL — C-string terminator
///
/// Returns `Err` with the offending component on rejection so the caller
/// can surface a clear "the PS5 sent us an invalid name" error rather than
/// silently writing to a wrong path.
fn local_dest_for(dest_dir: &Path, rel_path: &str) -> Result<PathBuf> {
    let mut out = PathBuf::from(dest_dir);
    for part in rel_path.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            anyhow::bail!(
                "rejecting download path with '{}' component: rel_path={:?}",
                part,
                rel_path
            );
        }
        if part.contains('\\')
            || part.contains(':')
            || part.contains('\0')
            || part.starts_with('\\')
        {
            anyhow::bail!(
                "rejecting download path with unsafe component {:?}: rel_path={:?}",
                part,
                rel_path
            );
        }
        out.push(part);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake pager over `total` synthetic file entries that mimics
    /// the payload: each call returns up to `REMOTE_LIST_PAGE` entries
    /// starting at `offset`, with `truncated=false` (the payload only sets
    /// truncated when its response buffer fills, which a clean page-sized
    /// reply does not).
    fn fake_pager(total: u64) -> impl FnMut(u64) -> Result<(Vec<DirEntry>, bool)> {
        move |offset: u64| {
            let end = (offset + REMOTE_LIST_PAGE).min(total);
            let entries = (offset..end)
                .map(|i| DirEntry {
                    name: format!("f{i:05}.bin"),
                    kind: "file".to_string(),
                    size: 1,
                })
                .collect();
            Ok((entries, false))
        }
    }

    #[test]
    fn paginate_reads_every_entry_past_the_256_cap() {
        // The truncation bug: a one-shot listing stopped at 256. These
        // sizes straddle the page boundary, including the exact-multiple
        // case that naively breaking on a short page would mishandle.
        for total in [0u64, 1, 255, 256, 257, 300, 512, 513, 1000] {
            let all = paginate_entries(fake_pager(total)).unwrap();
            assert_eq!(all.len() as u64, total, "lost entries at total={total}");
            // No duplicates / gaps: names are unique and complete.
            let mut names: Vec<_> = all.iter().map(|e| e.name.clone()).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len() as u64, total, "dup/gap at total={total}");
        }
    }

    #[test]
    fn paginate_stops_on_truncated_short_page() {
        // A buffer-truncated page (truncated=true) shorter than the cap
        // still means "ask again", but once the source is exhausted the
        // next page is empty and we stop — no infinite loop.
        let mut pages = vec![
            (vec![ent("a"), ent("b")], true), // truncated short page → continue
            (vec![ent("c")], false),          // short, not truncated → stop after
        ]
        .into_iter();
        let all = paginate_entries(|_offset| Ok(pages.next().unwrap())).unwrap();
        assert_eq!(all.len(), 3);
    }

    fn ent(name: &str) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            kind: "file".to_string(),
            size: 0,
        }
    }

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
        let resolved = local_dest_for(&base, "sub/dir/file.bin").expect("clean rel_path");
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

    #[test]
    fn local_dest_rejects_dotdot_component() {
        let base = PathBuf::from("/host/dest");
        let err = local_dest_for(&base, "../escape").unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn local_dest_rejects_dot_component() {
        let base = PathBuf::from("/host/dest");
        assert!(local_dest_for(&base, "./hidden").is_err());
    }

    #[test]
    fn local_dest_rejects_backslash_component() {
        let base = PathBuf::from("/host/dest");
        // PS5 paths use forward slashes, but a malicious entry name
        // containing a literal backslash should still be refused so
        // it can't bypass the split-on-/ scoping on Windows.
        assert!(local_dest_for(&base, r"sub/foo\bar").is_err());
    }

    #[test]
    fn local_dest_rejects_drive_letter_component() {
        let base = PathBuf::from("/host/dest");
        // A `:` in a component would let a malicious name like `C:` or
        // `foo:bar` either replace the path on Windows or land in an
        // NTFS alternate data stream. Either is a traversal.
        assert!(local_dest_for(&base, "sub/C:").is_err());
    }

    #[test]
    fn local_dest_accepts_dotted_filename() {
        // Defense against false positives: a filename that *contains*
        // dots (e.g. `archive.tar.gz`) or starts with a dot (e.g.
        // `.gitignore`) is legitimate. Only the exact `.` and `..`
        // components should be rejected.
        let base = PathBuf::from("/host/dest");
        assert!(local_dest_for(&base, "archive.tar.gz").is_ok());
        assert!(local_dest_for(&base, ".gitignore").is_ok());
        assert!(local_dest_for(&base, "sub/..foo").is_ok()); // contains, not equals
    }
}

#[cfg(test)]
mod pipeline_tests {
    //! End-to-end tests for the pipelined, connection-reusing download
    //! against a fake FS_READ payload over loopback — no PS5 needed. Pins
    //! (a) byte-correctness across the multi-chunk pipeline and (b) that a
    //! mid-stream connection drop reconnects and resumes from the exact
    //! last-written byte (the upload-grade robustness this rewrite adds).
    use super::*;
    use ftx2_proto::{FrameHeader, FrameType, FRAME_HEADER_LEN};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Deterministic file contents: byte at offset `o` is `(o % 251)`.
    fn pattern_byte(o: u64) -> u8 {
        (o % 251) as u8
    }

    /// Spawn a fake payload that answers FS_READ(offset, limit) with the
    /// pattern bytes for `[offset, offset+limit)`. If `drop_first_conn` is
    /// true, the FIRST accepted connection is closed right after its first
    /// response — forcing the downloader to reconnect and resume. Later
    /// connections serve normally. Returns the bound `ip:port`.
    fn spawn_fake_payload(drop_first_conn: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let mut conn_idx = 0usize;
            for stream in listener.incoming() {
                let mut s = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let drop_after_one = drop_first_conn && conn_idx == 0;
                conn_idx += 1;
                let mut answered = 0usize;
                loop {
                    let mut hbuf = [0u8; FRAME_HEADER_LEN];
                    if s.read_exact(&mut hbuf).is_err() {
                        break;
                    }
                    let hdr = FrameHeader::decode(&hbuf).unwrap();
                    let mut body = vec![0u8; hdr.body_len as usize];
                    if hdr.body_len > 0 {
                        s.read_exact(&mut body).unwrap();
                    }
                    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let offset = v["offset"].as_u64().unwrap();
                    let limit = v["limit"].as_u64().unwrap();
                    let data: Vec<u8> = (offset..offset + limit).map(pattern_byte).collect();
                    let rh = FrameHeader::new(FrameType::FsReadAck, 0, limit, 0);
                    if s.write_all(&rh.encode()).is_err() || s.write_all(&data).is_err() {
                        break;
                    }
                    answered += 1;
                    if drop_after_one && answered == 1 {
                        // Model a real mid-stream drop: shut the socket both
                        // ways so the downloader's next pipelined read gets a
                        // prompt EOF (FIN) — like a PS5 closing the connection
                        // — and must reconnect + resume. (A plain `break` here
                        // leaves the unread pipelined requests in the kernel
                        // buffer, and macOS then stalls the peer's read until
                        // SO_RCVTIMEO instead of delivering the close.)
                        let _ = s.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                }
            }
        });
        addr
    }

    fn run_download(addr: &str, size: u64) -> (PathBuf, Result<u64>) {
        let dir = std::env::temp_dir().join(format!(
            "ps5dl-test-{}-{}",
            std::process::id(),
            size
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = vec![DownloadEntry {
            remote_path: "/data/x.bin".into(),
            rel_path: "x.bin".into(),
            size,
        }];
        let res = download_to_local(addr, &dir, &manifest, None);
        (dir.join("x.bin"), res)
    }

    fn assert_pattern(path: &PathBuf, size: u64) {
        let got = std::fs::read(path).unwrap();
        assert_eq!(got.len() as u64, size, "downloaded size mismatch");
        for (i, b) in got.iter().enumerate() {
            assert_eq!(*b, pattern_byte(i as u64), "byte {i} differs");
        }
    }

    #[test]
    fn pipelined_download_is_byte_correct_across_chunks() {
        // 2 full chunks + a partial tail exercises the pipeline + the
        // min() boundary on the last request.
        let size = DOWNLOAD_CHUNK_SIZE * 2 + 12_345;
        let addr = spawn_fake_payload(false);
        let (path, res) = run_download(&addr, size);
        assert!(res.is_ok(), "download failed: {:?}", res.err());
        assert_eq!(res.unwrap(), size);
        assert_pattern(&path, size);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn download_resumes_after_midstream_drop() {
        // First connection dies after one response; the downloader must
        // reconnect and resume from the last durable byte — final file
        // must still be perfect.
        let size = DOWNLOAD_CHUNK_SIZE * 3 + 7;
        let addr = spawn_fake_payload(true);
        let (path, res) = run_download(&addr, size);
        assert!(res.is_ok(), "resume download failed: {:?}", res.err());
        assert_eq!(res.unwrap(), size);
        assert_pattern(&path, size);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
