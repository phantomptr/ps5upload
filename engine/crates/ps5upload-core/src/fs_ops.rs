//! File-system ops over AVA1 management.
//!
//! This module is the home for the non-transfer file RPCs the UI needs: list_dir, stat,
//! mkdir, move, read_file, query_hashes and the rest. Each helper makes one management
//! call (`crate::mgmt`) and returns the parsed body. Payload-side refusals surface as
//! anyhow errors holding a `MgmtError`, with the payload's error string verbatim, so the
//! UI can switch on the vocabulary the payload defines.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

// ─── FS_LIST_DIR ─────────────────────────────────────────────────────────────

/// One directory entry returned by `list_dir`.
///
/// `kind` is one of `"file"`, `"dir"`, `"link"`, `"other"`, or `"unknown"`
/// (for entries whose `lstat` on the payload side failed). Size is 0 for
/// non-regular-file kinds. `mtime` is Unix seconds and defaults to 0 when an
/// older payload omits it or `lstat` failed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: String,
    pub size: u64,
    #[serde(default)]
    pub mtime: i64,
}

/// A paginated directory listing response.
///
/// `total_scanned` is the number of entries `readdir` returned (before
/// slicing by `offset`/`limit`); clients use this to detect the natural
/// end of the directory vs being paginated into submission by a small
/// `limit`. `truncated` is set when the response buffer filled up before
/// the limit was reached — uncommon in practice (response body fits 256
/// entries), but covers the pathological "many very long filenames" case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirListing {
    pub path: String,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
    #[serde(default)]
    pub total_scanned: u64,
    #[serde(default)]
    pub returned: u64,
}

/// Options for `list_dir`. Defaults to offset 0, limit 256 (the payload's
/// own ceiling). Pass `offset` to paginate; bigger `limit` values are
/// silently clamped by the payload.
#[derive(Debug, Clone, Copy)]
pub struct ListDirOptions {
    pub offset: u64,
    pub limit: u64,
}

impl Default for ListDirOptions {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 256,
        }
    }
}

/// List immediate children of a directory on the PS5.
///
/// `path` must be absolute and must not contain `..`. Request shape:
/// `{"path":"...","offset":N,"limit":N}`. On error the payload returns
/// an error frame; the error string is surfaced verbatim.
pub fn list_dir(addr: &str, path: &str, opts: ListDirOptions) -> Result<DirListing> {
    list_dir_with_timeout(addr, path, opts, None)
}

/// Same as [`list_dir`] but with a caller-provided per-socket I/O
/// timeout. Reconcile uses this with a short (few-second) deadline —
/// listing one directory on the PS5 should return in well under a
/// second, so hanging 30 s (the default) on a payload that's busy or
/// crashed just keeps the user staring at "checking what's already on
/// your PS5…" for no reason.
pub fn list_dir_with_timeout(
    addr: &str,
    path: &str,
    opts: ListDirOptions,
    io_timeout: Option<std::time::Duration>,
) -> Result<DirListing> {
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "offset": opts.offset,
        "limit": opts.limit,
    }))
    .context("serialize list_dir body")?;
    let resp = mgmt::call_with(
        addr,
        m::FS_LIST,
        &format!("FS_LIST_DIR({path})"),
        &body,
        io_timeout,
    )?;
    let parsed: DirListing =
        serde_json::from_slice(&resp).context("decode FS_LIST_DIR_ACK body as JSON")?;
    Ok(parsed)
}

// ─── fs.stat ────────────────────────────────────────────────────────────────

/// What `fs.stat` says about a path (a symbolic link is followed; a dangling one is `link`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathStat {
    /// `file`, `dir`, `link`, `other` or `unknown`.
    pub kind: String,
    pub size: u64,
    pub mtime: u64,
    pub mode: u32,
    /// The device id (`st_dev`): two paths with the same value share a mount.
    #[serde(default)]
    pub dev: u64,
}

/// `fs.stat`: metadata of one path. An absent path is an error whose text carries
/// `fs_stat_failed_errno_2` (see [`is_not_found`]).
pub fn fs_stat(addr: &str, path: &str) -> Result<PathStat> {
    let body = serde_json::to_vec(&serde_json::json!({ "path": path }))
        .context("serialize fs_stat body")?;
    match mgmt::call_as(addr, m::FS_STAT, &format!("FS_STAT({path})"), &body) {
        Ok(resp) => serde_json::from_slice(&resp).context("decode FS_STAT reply as JSON"),
        Err(e) => Err(e),
    }
}

/// [`fs_stat`] with its own deadline, for background readers that must not wait out the default.
pub fn fs_stat_with_timeout(
    addr: &str,
    path: &str,
    timeout: std::time::Duration,
) -> Result<PathStat> {
    let body = serde_json::to_vec(&serde_json::json!({ "path": path }))
        .context("serialize fs_stat body")?;
    let resp = mgmt::call_with(
        addr,
        m::FS_STAT,
        &format!("FS_STAT({path})"),
        &body,
        Some(timeout),
    )?;
    serde_json::from_slice(&resp).context("decode FS_STAT reply as JSON")
}

/// True when an error from [`fs_stat`], [`fs_read`] or [`list_dir`] means "no such path".
pub fn is_not_found(message: &str) -> bool {
    message.contains("ENOENT")
        || message.contains("No such file")
        || message.split("_errno_").skip(1).any(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                == "2"
        })
        || message.contains("fs_read_stat_failed")
}

/// Whether `path` exists. `Ok(false)` only for a definite "no such path"; any other failure (a
/// busy port, a timeout) is an error the caller decides about.
pub fn fs_exists(addr: &str, path: &str) -> Result<bool> {
    match fs_stat(addr, path) {
        Ok(_) => Ok(true),
        Err(e) if is_not_found(&format!("{e:#}")) => Ok(false),
        Err(e) => Err(e),
    }
}

// ─── FS_HASH ────────────────────────────────────────────────────────────────

/// Response body from FS_HASH_ACK. `hash` is 64 lowercase hex characters
/// (32-byte BLAKE3 digest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashResult {
    pub path: String,
    pub size: u64,
    pub hash: String,
}

/// Ask the payload to BLAKE3-hash a single file on the PS5. Used by Safe
/// reconcile mode to verify remote content matches local content when
/// size equality alone isn't enough guarantee. Streams in 64 KiB chunks
/// on the payload side — ~2-3 s per GiB on PS5 UFS, so judicious use only.
pub fn fs_hash(addr: &str, path: &str) -> Result<HashResult> {
    fs_hash_with_timeout(addr, path, None)
}

/// Like [`fs_hash`] but with a caller-supplied per-socket I/O timeout.
/// Reconcile loops over many files; without an explicit override each
/// call inherits the 30 s default and a single hung PS5 stalls the
/// whole reconcile linearly. Pass a few-second deadline so a stuck
/// FS_HASH fails fast and the loop continues.
pub fn fs_hash_with_timeout(
    addr: &str,
    path: &str,
    io_timeout: Option<std::time::Duration>,
) -> Result<HashResult> {
    let body = serde_json::to_vec(&serde_json::json!({ "path": path }))
        .context("serialize fs_hash body")?;
    // A job over AVA1 (the hash of a multi-GiB file outlives any request deadline); the
    // caller's timeout is now the whole wait.
    let resp = mgmt::run_op(
        addr,
        mgmt::ops::HASH,
        &format!("FS_HASH({path})"),
        &body,
        &mgmt::JobCall {
            op_id: 0,
            subject: path,
            deadline: io_timeout.unwrap_or(mgmt::DEFAULT_TIMEOUT),
        },
    )?;
    let parsed: HashResult =
        serde_json::from_slice(&resp).context("decode FS_HASH_ACK body as JSON")?;
    Ok(parsed)
}

/// What [`ffpfsc_unpack`] wrote: the image beside the `.ffpfsc`, and its size.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UnpackResult {
    pub path: String,
    pub size: u64,
}

/// Decompress a `.ffpfsc` on the console (#453): the helper writes the image it holds beside it
/// under the image's own name (a `.part` first, renamed once its size and exFAT boot sector
/// check out) and keeps the source. `op_id` keys `/api/ps5/fs/op-status` and `op-cancel`.
/// Refused: a destination that exists, a drive without room for the image plus 1 GiB.
pub fn ffpfsc_unpack(
    addr: &str,
    path: &str,
    op_id: u64,
    io_timeout: std::time::Duration,
) -> Result<UnpackResult> {
    let body = serde_json::to_vec(&serde_json::json!({ "path": path }))
        .context("serialize ffpfsc_unpack")?;
    match mgmt::run_op(
        addr,
        mgmt::ops::FFPFSC_UNPACK,
        &format!("FFPFSC_UNPACK({path})"),
        &body,
        &mgmt::JobCall {
            op_id,
            subject: path,
            deadline: io_timeout,
        },
    ) {
        Ok(resp) => serde_json::from_slice(&resp).context("decode FFPFSC_UNPACK result"),
        Err(e) if is_cancel(&e, "fs_unpack_cancelled") => bail!("cancelled"),
        Err(e) => Err(e),
    }
}

/// What [`archive_extract`] asks for: the archive (or its first volume) and the folder its entries
/// are written into, with the password and how an existing folder is treated.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ExtractRequest {
    pub path: String,
    pub dest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// The folder may exist: write into it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub merge: bool,
    /// An existing file is replaced (else kept and counted in `skipped`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// What [`archive_extract`] wrote. `unsafe` counts entries refused for escaping the folder (an
/// absolute name, `..`, a link on the way), `links` the links and special files never created.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtractResult {
    pub path: String,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub dirs: u64,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub skipped: u64,
    #[serde(default, rename = "unsafe")]
    pub unsafe_entries: u64,
    #[serde(default)]
    pub links: u64,
}

/// Extract an archive on the console into a folder there (.zip, .7z, .rar, .tar and .tar.gz/.bz2/.xz/
/// .zst, numbered and RAR volume sets). The helper runs its extractor payload for it, so nothing
/// crosses the network. `op_id` keys `/api/ps5/fs/op-status` and `op-cancel`. Refused: a folder
/// that exists without `merge`, a drive without room. A folder the run made is removed again when
/// it fails or is stopped.
pub fn archive_extract(
    addr: &str,
    req: &ExtractRequest,
    op_id: u64,
    io_timeout: std::time::Duration,
) -> Result<ExtractResult> {
    let body = serde_json::to_vec(req).context("serialize archive_extract")?;
    match mgmt::run_op(
        addr,
        mgmt::ops::ARCHIVE_EXTRACT,
        &format!("ARCHIVE_EXTRACT({})", req.path),
        &body,
        &mgmt::JobCall {
            op_id,
            subject: &req.path,
            deadline: io_timeout,
        },
    ) {
        Ok(resp) => serde_json::from_slice(&resp).context("decode ARCHIVE_EXTRACT result"),
        Err(e) if is_cancel(&e, "fs_extract_cancelled") => bail!("cancelled"),
        Err(e) => Err(e),
    }
}

/// What [`app_dump`] asks for: the title (its `<ID>-app0` must be mounted, i.e. the game is
/// running) and the folder the dump is written under (`<dest>/<ID>-app`, DLC folders beside it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDumpRequest {
    pub title_id: String,
    pub dest: String,
    /// Decrypt the SELFs (.elf/.self/.prx/.sprx/.bin) of the dump on the console.
    #[serde(default)]
    pub decrypt: bool,
    /// With `decrypt`: rewrite each decrypted ELF as a fake-signed SELF (the plain ELF is kept
    /// under `<out>/decrypted/`).
    #[serde(default)]
    pub fself: bool,
    /// Also dump the title's mounted DLC (`-ac` folders).
    #[serde(default = "default_true")]
    pub dlc: bool,
}

fn default_true() -> bool {
    true
}

/// What [`app_dump`] wrote. `decrypt_skipped` is empty, or why no file was decrypted (the
/// payload's token, e.g. `dump_decrypt_unsupported_fw`); `fw` is the console's firmware as
/// hex (`0x13600000`), empty when it could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDumpResult {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub decrypted: u64,
    #[serde(default)]
    pub decrypt_failed: u64,
    #[serde(default)]
    pub decrypt_skipped: String,
    #[serde(default)]
    pub dlc: Vec<String>,
    #[serde(default)]
    pub fw: String,
}

/// Dump an installed, running title to a drive on the console (`JOB_OP_APP_DUMP`): its files,
/// the sce_sys extras of its package and appmeta, its DLC, optionally decrypted. Refused (the
/// payload's token as cause): `dump_not_running` (not mounted: launch it first), `dump_exists`,
/// `dump_no_space`. `op_id` keys `/api/ps5/fs/op-status` and `op-cancel`; a stopped dump leaves
/// its partial folder with a `dump-incomplete.txt` in it.
pub fn app_dump(
    addr: &str,
    req: &AppDumpRequest,
    op_id: u64,
    io_timeout: std::time::Duration,
) -> Result<AppDumpResult> {
    let body = serde_json::to_vec(req).context("serialize app_dump")?;
    match mgmt::run_op(
        addr,
        mgmt::ops::APP_DUMP,
        &format!("APP_DUMP({})", req.title_id),
        &body,
        &mgmt::JobCall {
            op_id,
            subject: &req.title_id,
            deadline: io_timeout,
        },
    ) {
        Ok(resp) => serde_json::from_slice(&resp).context("decode APP_DUMP result"),
        Err(e) if is_cancel(&e, "dump_cancelled") => bail!("cancelled"),
        Err(e) => Err(e),
    }
}

// ─── FS_READ ────────────────────────────────────────────────────────────────

/// Ask the payload for up to `limit` bytes of a file on the PS5 starting at
/// `offset`. Used to pull small metadata blobs (`param.json`, `icon0.png`)
/// out of a game folder so the UI can render covers + titles. The payload
/// caps the response at `FS_READ_MAX_BYTES` regardless of `limit`, so large
/// requests are silently truncated — callers that need exact-size reads
/// should chunk with updated `offset` values.
pub fn fs_read(addr: &str, path: &str, offset: u64, limit: u64) -> Result<Vec<u8>> {
    fs_read_with_timeout(addr, path, offset, limit, None, false)
}

/// Like [`fs_read`] but with a caller-supplied per-socket I/O timeout.
/// Same rationale as `fs_hash_with_timeout` — read loops over many
/// metadata blobs need to fail fast on a stuck PS5 instead of inheriting
/// the 30 s default.
pub fn fs_read_with_timeout(
    addr: &str,
    path: &str,
    offset: u64,
    limit: u64,
    io_timeout: Option<std::time::Duration>,
    unsafe_read: bool,
) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "offset": offset,
        "limit": limit,
        "unsafe": unsafe_read,
    }))
    .context("serialize fs_read body")?;
    // The AVA1 transport loops `fs.read` on `eof` until `limit` bytes (at most the legacy
    // ceiling) have arrived, so the caller sees one call as before.
    let resp = mgmt::call_with(
        addr,
        m::FS_READ,
        &format!("FS_READ({path})"),
        &body,
        io_timeout,
    )?;
    Ok(resp)
}

/// One read through the helper. It caps a single read (2 MiB), so a whole file takes a loop.
const FS_READ_WHOLE_CHUNK: u64 = 1 << 20;

/// The most [`fs_read_whole`] reads: it holds the file in memory, and what it is used for (a
/// library, a payload, a small record) is a few tens of MiB at most. `size` is whatever the
/// console's listing said, so it is checked rather than believed.
pub const FS_READ_WHOLE_MAX: u64 = 1 << 30;

/// The most reserved ahead of the bytes arriving; the rest grows as they do.
const FS_READ_WHOLE_PREALLOC: u64 = 16 << 20;

/// A whole `size`-byte file from the console. An error when it ends early: nothing that wants the
/// whole file can use part of one. An error too when `size` is over [`FS_READ_WHOLE_MAX`].
pub fn fs_read_whole(addr: &str, path: &str, size: u64) -> Result<Vec<u8>> {
    read_whole_with(path, size, |at, want| fs_read(addr, path, at, want))
}

fn read_whole_with(
    path: &str,
    size: u64,
    mut read: impl FnMut(u64, u64) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    if size > FS_READ_WHOLE_MAX {
        anyhow::bail!(
            "{path} is {size} bytes, more than the {FS_READ_WHOLE_MAX} that can be read whole"
        );
    }
    let mut out = Vec::with_capacity(size.min(FS_READ_WHOLE_PREALLOC) as usize);
    while (out.len() as u64) < size {
        let at = out.len() as u64;
        let piece = read(at, FS_READ_WHOLE_CHUNK.min(size - at))?;
        if piece.is_empty() {
            anyhow::bail!("{path} ended at {at} of {size} bytes");
        }
        // A reply longer than what was asked for is not kept past the file's size.
        let room = (size - at) as usize;
        out.extend_from_slice(&piece[..piece.len().min(room)]);
    }
    Ok(out)
}

// ─── Destructive ops (delete / move / chmod / mkdir) ────────────────────────

/// Delete a file or directory recursively on the PS5. Path must be under
/// the payload's writable-root allowlist (/data, /user, /mnt/ext*, /mnt/usb*).
///
/// Convenience form using the default 30 s socket timeout — appropriate
/// for single-file unlinks. For large directory trees (game folders with
/// 200k+ files) callers must use [`fs_delete_with_timeout`] with a
/// generous deadline; the payload's recursive walk is single-threaded and
/// can take minutes on PS5 UFS.
pub fn fs_delete(addr: &str, path: &str) -> Result<()> {
    fs_delete_with_timeout(addr, path, None)
}

/// Like [`fs_delete`] but with a caller-supplied per-socket I/O timeout.
/// Same single-shot RPC as `fs_copy`/`fs_move`: the payload performs the
/// entire recursive `rm -rf` and only sends FS_DELETE_ACK at the end.
/// With the default 30 s socket timeout, deleting a small-file-heavy
/// game folder (≈220k files, 19k dirs — 240k+ unlink/rmdir syscalls)
/// fires the timeout mid-walk and surfaces to the user as the cryptic
/// "read frame header: Resource temporarily unavailable" 502, while the
/// payload happily keeps deleting in the background. The HTTP handler
/// passes a 1-hour deadline so the operation can complete naturally.
pub fn fs_delete_with_timeout(
    addr: &str,
    path: &str,
    io_timeout: Option<std::time::Duration>,
) -> Result<()> {
    fs_delete_with_op_id(addr, path, 0, io_timeout)
}

/// Like [`fs_delete_with_timeout`] but stamps a caller-chosen op_id
/// into the FS_DELETE frame's trace_id. The payload uses that as the
/// key into its in-flight ops table — pass the same op_id to
/// [`fs_op_status`] / [`fs_op_cancel`] from a separate connection to
/// observe progress (bytes-freed / total) or cancel mid-flight. Pass
/// 0 if you don't need progress/cancel; the payload skips the slot
/// registration in that case so single-file unlinks don't burn a
/// MAX_FS_OPS slot.
pub fn fs_delete_with_op_id(
    addr: &str,
    path: &str,
    op_id: u64,
    io_timeout: Option<std::time::Duration>,
) -> Result<()> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "path": path })).context("serialize fs_delete")?;
    match mgmt::run_op(
        addr,
        mgmt::ops::DELETE,
        "FS_DELETE",
        &body,
        &mgmt::JobCall {
            op_id,
            subject: path,
            deadline: io_timeout.unwrap_or(mgmt::DEFAULT_TIMEOUT),
        },
    ) {
        Ok(_) => Ok(()),
        // Cancellation is a non-error outcome from the user's POV (they hit Stop): surface
        // it distinctly so the engine HTTP layer can return 409 instead of 502, mirroring
        // fs_copy.
        Err(e) if is_cancel(&e, "fs_delete_cancelled") => bail!("cancelled"),
        Err(e) => Err(e),
    }
}

/// True when `e` is the payload's own cancel outcome `token`.
fn is_cancel(e: &anyhow::Error, token: &str) -> bool {
    e.downcast_ref::<mgmt::MgmtError>()
        .is_some_and(|m| m.cause == token)
}

/// Snapshot of a running job. `found = false` means the
/// op_id is not currently registered (either finished or never
/// started). Caller should stop polling on `found = false`.
#[derive(Debug, Clone, Deserialize)]
pub struct FsOpSnapshot {
    pub found: bool,
    #[serde(default)]
    pub op_id: u64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub bytes_copied: u64,
    #[serde(default)]
    pub cancel_requested: bool,
}

/// Ask the console for the current state of an in-flight job (a delete, a checksum, ...).
pub fn fs_op_status(addr: &str, op_id: u64) -> Result<FsOpSnapshot> {
    // An operation the AVA1 transport runs as a job (delete, ...): its progress is the job's.
    if let Some(found) = mgmt::op_progress(addr, op_id)? {
        return Ok(match found {
            Some(p) => FsOpSnapshot {
                found: true,
                op_id,
                kind: p.kind,
                from: p.subject,
                to: String::new(),
                total_bytes: p.bytes_total,
                bytes_copied: p.bytes_done,
                cancel_requested: p.cancel_requested,
            },
            None => FsOpSnapshot {
                found: false,
                op_id: 0,
                kind: String::new(),
                from: String::new(),
                to: String::new(),
                total_bytes: 0,
                bytes_copied: 0,
                cancel_requested: false,
            },
        });
    }
    // No registered transport: nothing is running under `op_id` that this process could ask about.
    Err(mgmt::helper_not_ava1("FS_OP_STATUS"))
}

/// Send FS_OP_CANCEL to the payload. Returns true if the op was
/// found and the cancel flag was set; false if the op_id wasn't
/// recognized (already finished or never registered).
pub fn fs_op_cancel(addr: &str, op_id: u64) -> Result<bool> {
    if let Some(found) = mgmt::op_cancel(addr, op_id)? {
        return Ok(found);
    }
    Err(mgmt::helper_not_ava1("FS_OP_CANCEL"))
}

// ─── FS_MOUNT / FS_UNMOUNT ─────────────────────────────────────────────────

/// Return shape from FS_MOUNT_ACK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResult {
    pub mount_point: String,
    pub dev_node: String,
    pub fstype: String,
    /// True if `<mount_point>/sce_sys/param.json` exists immediately
    /// after the mount. False when the user built an image with an
    /// extra top-level folder (game files at
    /// `<mount>/MyGame/sce_sys/...` instead of `<mount>/sce_sys/...`)
    /// or when the image isn't a game (e.g. an arbitrary disk image).
    /// The mount itself succeeds either way; this flag lets the UI
    /// warn that Register/Launch will fail without re-building the
    /// image. Added in 2.2.32. Older payloads omit the field — serde
    /// defaults to `true` so a missing field doesn't trigger a false
    /// warning on pre-2.2.32 payloads.
    #[serde(default = "default_layout_valid")]
    pub layout_valid: bool,
    /// statfs-reported block size at the resolved mount point. Added
    /// in 2.2.52 to help diagnose UFS images that mount at the kernel
    /// level but read as empty (typically a sector-size/cluster-size
    /// mismatch between the image and the LVD-exposed device). Older
    /// payloads omit the field; serde-default to 0 so a missing value
    /// doesn't read as a fake validation failure on pre-2.2.52 mounts.
    #[serde(default)]
    pub f_bsize: u64,
    /// statfs-reported preferred I/O block size. Same diagnostics
    /// purpose as `f_bsize` — useful when the kernel reports a
    /// fragment size for `f_bsize` and the actual I/O block size lives
    /// here instead. Added in 2.2.52.
    #[serde(default)]
    pub f_iosize: u64,
    /// True if the kernel reports `MNT_RDONLY` on the resolved mount
    /// point — even when the caller passed `read_only=false`. PS5's
    /// UFS_DOWNLOAD_DATA image_type forces RO on some firmwares and
    /// surfacing this lets the UI explain to the user why writes
    /// won't land. Added in 2.2.52.
    #[serde(default)]
    pub kernel_ro: bool,
}

fn default_layout_valid() -> bool {
    true
}

/// Mount a disk image on the PS5. `image_path` must be an absolute path
/// under the payload's writable-root allowlist and have a `.exfat`,
/// `.ffpkg`, or `.ffpfs` extension.
///
/// Mount-location resolution, in priority:
/// - `mount_point: Some(path)` — full path. Must be allowlisted on the
///   payload (`/data`, `/mnt/ext*`, `/mnt/usb*`, `/mnt/ps5upload/*`).
///   Added in 2.2.25.
/// - `mount_name: Some(name)` (no slashes) — mounts at
///   `/mnt/ps5upload/<name>/`. Backward-compat with 2.2.24-and-earlier
///   callers.
/// - both `None` — payload derives a filesystem-safe name from the
///   image basename and mounts at `/mnt/ps5upload/<derived>/`.
///
/// `read_only=true` selects the read-only LVD attach flag and the
/// matching read-only nmount flag (UFS magic `0x10000001` for
/// `.ffpkg`, `MNT_RDONLY` for exfatfs and pfs). Added in 2.2.26.
pub fn fs_mount(
    addr: &str,
    image_path: &str,
    mount_name: Option<&str>,
    mount_point: Option<&str>,
    read_only: bool,
) -> Result<MountResult> {
    let body = serde_json::to_vec(&serde_json::json!({
        "image_path": image_path,
        "mount_name": mount_name,
        "mount_point": mount_point,
        "read_only": if read_only { 1 } else { 0 },
    }))
    .context("serialize fs_mount body")?;
    let resp = mgmt::call_as(addr, m::FS_MOUNT, &format!("FS_MOUNT({image_path})"), &body)?;
    let parsed: MountResult =
        serde_json::from_slice(&resp).context("decode FS_MOUNT_ACK body as JSON")?;
    Ok(parsed)
}

/// Unmount a previously-mounted image. `mount_point` must be the exact
/// path returned by `fs_mount` (under `/mnt/ps5upload/`). The payload
/// refuses to unmount anything outside that root.
pub fn fs_unmount(addr: &str, mount_point: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "mount_point": mount_point }))
        .context("serialize fs_unmount")?;
    mgmt::call(addr, m::FS_UNMOUNT, &body)?;
    Ok(())
}

/// Safely remove a USB drive: the helper unmounts `/mnt/usbN` so the stick can be pulled.
/// A plain unmount is refused with `usb_eject_busy` while anything on the console holds the
/// drive open (the shell's own services usually do); `force` unmounts anyway after a sync,
/// which flushes the cache and marks the volume clean.
pub fn usb_eject(addr: &str, mount_point: &str, force: bool) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({
        "mount_point": mount_point,
        "force": if force { "1" } else { "0" },
    }))
    .context("serialize usb_eject")?;
    let resp = mgmt::call(addr, m::USB_EJECT, &body)?;
    let v: serde_json::Value = serde_json::from_slice(&resp).context("usb_eject reply")?;
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        return Ok(());
    }
    let err = v
        .get("err")
        .and_then(|e| e.as_str())
        .unwrap_or("usb_eject_failed");
    anyhow::bail!("{err}")
}

/// Rename/move a file or directory intra-volume. An existing destination is refused
/// (`fs_move_exists`), never replaced. Cross-volume moves surface as `fs_move_cross_mount`
/// error — payload uses `rename(2)` which returns EXDEV across mount points.
pub fn fs_move(addr: &str, from: &str, to: &str) -> Result<()> {
    fs_move_with_timeout(addr, from, to, false, None)
}

/// Like [`fs_move`] with the caller's choice about an existing destination and its own
/// per-socket I/O timeout. `replace` false: the console refuses with `fs_move_exists`. True:
/// `rename(2)` replaces a file, or an empty folder, that is there. Only a caller that was told
/// to replace passes true: this is how a same-drive move used to destroy files.
/// Most fs_move calls return in milliseconds (rename(2) is a metadata
/// op), but cross-volume moves where the engine retries via
/// copy-then-delete inherit the same long-deadline need as fs_copy.
pub fn fs_move_with_timeout(
    addr: &str,
    from: &str,
    to: &str,
    replace: bool,
    io_timeout: Option<std::time::Duration>,
) -> Result<()> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "from": from, "to": to, "overwrite": replace }))
            .context("serialize fs_move")?;
    mgmt::call_with(addr, m::FS_RENAME, "FS_MOVE", &body, io_timeout)?;
    Ok(())
}

/// Change permissions. `mode` is octal like "0777" (passed as string so
/// JSON parsing doesn't alter the intended octal value). If `recursive`
/// is true and path is a directory, walks + chmod's every entry.
pub fn fs_chmod(addr: &str, path: &str, mode: &str, recursive: bool) -> Result<()> {
    fs_chmod_with_timeout(addr, path, mode, recursive, None)
}

/// Like [`fs_chmod`] but with a caller-supplied per-socket I/O timeout. The
/// default 30 s deadline easily expires on `recursive=true` chmods of a
/// 22 k-file game folder (measured ~32 s for `chmod -R 0777` on PPSA17221).
/// Callers driving recursive chmod on big trees should pass at least a few
/// minutes; a `None` keeps the legacy 30 s default.
pub fn fs_chmod_with_timeout(
    addr: &str,
    path: &str,
    mode: &str,
    recursive: bool,
    io_timeout: Option<std::time::Duration>,
) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "mode": mode,
        "recursive": if recursive { 1 } else { 0 },
    }))
    .context("serialize fs_chmod")?;
    if recursive {
        // The walk of a big tree is a job (progress, cancel, no socket held for minutes).
        mgmt::run_op(
            addr,
            mgmt::ops::CHMOD_R,
            "FS_CHMOD",
            &body,
            &mgmt::JobCall {
                op_id: 0,
                subject: path,
                deadline: io_timeout.unwrap_or(mgmt::DEFAULT_TIMEOUT),
            },
        )?;
        return Ok(());
    }
    mgmt::call_with(addr, m::FS_CHMOD, "FS_CHMOD", &body, io_timeout)?;
    Ok(())
}

/// Create a directory (and any missing parents). Idempotent — succeeds
/// if the directory already exists.
pub fn fs_mkdir(addr: &str, path: &str) -> Result<()> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "path": path })).context("serialize fs_mkdir")?;
    mgmt::call(addr, m::FS_MKDIR, &body)?;
    Ok(())
}

// ─── App lifecycle (register / unregister / launch / list) ─────────────────
//
// Mirrors the payload's register.c pipeline. See the payload's register.c
// FrameType doc comments for the wire shape (the standalone specs/
// directory was consolidated into in-tree doc comments).
// "Register" stages + installs a title dir; "Launch" calls
// sceLncUtilLaunchApp on an already-registered title.

/// Response shape from APP_REGISTER_ACK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResult {
    pub title_id: String,
    pub title_name: String,
    #[serde(default)]
    pub used_nullfs: bool,
}

/// Register a PS5 game folder so Sony's launcher picks it up in XMB.
/// `src_path` must be a directory containing `sce_sys/param.json` or
/// `sce_sys/param.sfo`. Works for folders on `/data`, `/mnt/ext*`,
/// `/mnt/usb*`, AND content inside a mounted `/mnt/ps5upload/<name>/`.
/// Idempotent — calling twice with the same src_path is safe (Sony's
/// installer returns `0x80990002` which the payload normalises to OK).
///
/// `patch_drm_type=true` (added in 2.2.26) rewrites the source
/// `sce_sys/param.json`'s `applicationDrmType` to `"standard"`
/// before staging — needed when a PSN-extracted dump ships with
/// `"PSN"` or `"disc"` and the launcher rejects it. Invasive:
/// modifies the user's source file in place, so it's opt-in.
pub fn app_register(addr: &str, src_path: &str, patch_drm_type: bool) -> Result<RegisterResult> {
    let body = serde_json::to_vec(&serde_json::json!({
        "src_path": src_path,
        "patch_drm_type": if patch_drm_type { 1 } else { 0 },
    }))
    .context("serialize app_register body")?;
    // A register can run 10 s or more (copy of the metadata, the nullfs mount, Sony's installer
    // under its lock), so it gets the 60 s deadline a launch has.
    let resp = mgmt::call_with(
        addr,
        m::APP_REGISTER,
        &format!("APP_REGISTER({src_path})"),
        &body,
        Some(SONY_CALL_TIMEOUT),
    )?;
    let parsed: RegisterResult =
        serde_json::from_slice(&resp).context("decode APP_REGISTER_ACK body as JSON")?;
    Ok(parsed)
}

/// The deadline of the Sony-lock calls that can queue behind one another or run long.
const SONY_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Outcome of an unregister (a folder registration's teardown, or a package's
/// uninstall).
///
/// A helper from 7.1.6 on answers a Sony refusal with an ERROR frame
/// (`uninstall_refused:0x…`), so that arrives here as `Err`. Older helpers
/// acknowledged the refusal and carried Sony's code in the body, and put an
/// "Unregistered" toast on the console either way: the app then dropped the
/// game from its list and the next refresh brought it back (#476).
#[derive(Debug, Clone, Copy, Default)]
pub struct UnregisterOutcome {
    /// `sceAppInstUtilAppUninstall`'s return code from an older helper's ACK.
    /// 0 = accepted, or not reported. Non-zero = Sony refused.
    pub sony_uninstall_rc: u32,
    /// The helper found the title's folders still on the console after Sony
    /// accepted the uninstall (it may still be removing a large game).
    /// `None` from a helper older than 7.1.6, which did not check.
    ///
    /// `Some(false)` with a non-zero `sony_uninstall_rc` is our own folder
    /// registration torn down although Sony refused: the title is no longer
    /// listed, so it is not a failure ([`Self::refusal_stands`] is false).
    pub still_present: Option<bool>,
}

impl UnregisterOutcome {
    /// True when Sony refused the uninstall — the caller should say so
    /// rather than reporting a clean success.
    pub fn sony_refused(&self) -> bool {
        self.sony_uninstall_rc != 0
    }

    /// A refusal the caller must report: Sony refused and the helper did not
    /// confirm the title is gone.
    pub fn refusal_stands(&self) -> bool {
        self.sony_refused() && self.still_present != Some(false)
    }

    /// Parse the APP_UNREGISTER ACK body. An empty or unparsable body (the
    /// oldest helpers) means "no result reported".
    pub fn from_ack(body: &[u8]) -> Self {
        let v = serde_json::from_slice::<serde_json::Value>(body).ok();
        let rc = v
            .as_ref()
            .and_then(|v| v.get("sony_uninstall_rc"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32;
        let still_present = v
            .as_ref()
            .and_then(|v| v.get("still_present"))
            .and_then(|x| x.as_bool());
        Self {
            sony_uninstall_rc: rc,
            still_present,
        }
    }
}

/// Unmount our registration (when there is one) and ask Sony to uninstall the
/// title. See [`UnregisterOutcome`] for what the reply means.
pub fn app_unregister(addr: &str, title_id: &str) -> Result<UnregisterOutcome> {
    let body = serde_json::to_vec(&serde_json::json!({ "title_id": title_id }))
        .context("serialize app_unregister body")?;
    // On FW 13.60 the unregister repeats for two records and can run 10 s or more, and the
    // helper then waits up to 10 s for the title's folders to go: 60 s deadline.
    let resp = mgmt::call_with(
        addr,
        m::APP_UNREGISTER,
        "APP_UNREGISTER",
        &body,
        Some(SONY_CALL_TIMEOUT),
    )?;
    Ok(UnregisterOutcome::from_ack(&resp))
}

/// Launch an already-registered title via `sceLncUtilLaunchApp`. The
/// title must exist in `app.db` — call `app_register` first if needed.
///
/// Uses a 60 s I/O timeout (vs the default 30 s) because the payload's
/// triple-strategy launch chain (LncUtil zeroed-param → LncUtil NULL
/// → SystemServiceLaunchApp) plus per-strategy backoff can take longer
/// than 30 s on slow firmware or first-launch-of-the-session paths
/// where Sony's launch service has to warm up. The wall-clock launch
/// itself is fast; the headroom is for the ack round-trip under load.
pub fn app_launch(addr: &str, title_id: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "title_id": title_id }))
        .context("serialize app_launch body")?;
    mgmt::call_with(
        addr,
        m::APP_LAUNCH,
        "APP_LAUNCH",
        &body,
        Some(SONY_CALL_TIMEOUT),
    )?;
    Ok(())
}

/// One entry returned by `app_list_registered`.
///
/// `src` is the path recorded in our `/user/app/<title_id>/mount.lnk`
/// at registration time (empty if we did not install the title).
/// `image_backed` is true iff a `mount_img.lnk` file is present
/// alongside — i.e., the title depends on a disk image we mounted,
/// and unmounting the image will break the title.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredApp {
    pub title_id: String,
    pub title_name: String,
    #[serde(default)]
    pub src: String,
    #[serde(default)]
    pub image_backed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredApps {
    pub apps: Vec<RegisteredApp>,
}

/// List titles registered on the console.
///
/// Despite the name this does not read `app.db`: the payload scans
/// `/user/app/` directly, because any title the console knows about has a
/// directory there and a directory cannot be held open by the shell. The
/// old `list_sqlite_unavailable` failure is no longer reachable — callers
/// that still map it are harmless, and older payloads can still send it.
pub fn app_list_registered(addr: &str) -> Result<RegisteredApps> {
    // Over AVA1 the transport pages the list (a reply holds ~1,700 entries) and returns one document.
    let resp = mgmt::call(addr, m::APP_LIST, &[])?;
    let parsed: RegisteredApps =
        serde_json::from_slice(&resp).context("decode APP_LIST_REGISTERED_ACK body as JSON")?;
    Ok(parsed)
}

// ─── Scoped listing + reconciliation ──────────────────────────────────────

/// Flattened remote inventory: relpath → size. Populated by
/// `list_remote_scoped` from per-parent `FS_LIST_DIR` calls.
pub type RemoteInventory = std::collections::BTreeMap<String, u64>;

/// Local inventory: mirrors `RemoteInventory` but built from walking the
/// host filesystem. Relpaths use forward-slash even on Windows so
/// comparison against remote works.
pub type LocalInventory = std::collections::BTreeMap<String, u64>;

/// Walk a local directory and build the flattened `{relpath → size}` map
/// that reconcile logic compares against the remote inventory.
pub fn walk_local_inventory(root: &std::path::Path, excludes: &[String]) -> Result<LocalInventory> {
    walk_local_inventory_until(root, excludes, &|| false)
}

/// [`walk_local_inventory`], giving up with `reconcile_busy` between folders once `yield_now` says.
fn walk_local_inventory_until(
    root: &std::path::Path,
    excludes: &[String],
    yield_now: &dyn Fn() -> bool,
) -> Result<LocalInventory> {
    let mut out = LocalInventory::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if yield_now() {
            return Err(preview_yielded());
        }
        let rd = std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?;
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                let size = entry.metadata().map_or(0, |m| m.len());
                let path = entry.path();
                // Relative to the source, as the transfer matches it: a pattern naming a folder
                // that also occurs above the source must not exclude everything.
                let rel = path.strip_prefix(root).unwrap_or(&path);
                if crate::excludes::is_excluded_strings(rel, excludes) {
                    continue;
                }
                // Use forward-slash paths on every OS so relpaths match
                // what the PS5 returns (PS5 is FreeBSD, always '/').
                let rel_str = rel
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                out.insert(rel_str, size);
            }
        }
    }
    Ok(out)
}

/// Verification mode for reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReconcileMode {
    /// Size-only equality check. Near-zero false positives on game data,
    /// trivially fast — no extra payload I/O beyond the list walk.
    Fast,
    /// Size + BLAKE3 hash equality check. Paranoid-safe but pays for a
    /// full re-read of every same-size remote file through the payload.
    Safe,
}

/// Result of reconciling a local source tree against a remote
/// destination: the list of relative paths that need to be sent.
/// Already-present-and-equal files are *not* returned (they're skipped
/// on the re-upload). Bytes total across the to-send set lets callers
/// size progress bars honestly ("sending 320 MB" rather than "sending
/// 50 GB of which 49.7 are already done").
#[derive(Debug, Clone)]
pub struct ReconcilePlan {
    pub to_send: Vec<ReconcileFile>,
    pub bytes_to_send: u64,
    pub already_present: u64,
    pub bytes_already_present: u64,
}

#[derive(Debug, Clone)]
pub struct ReconcileFile {
    pub rel_path: String,
    pub size: u64,
}

/// Build a remote inventory scoped to just the parent directories that
/// the local inventory actually touches. Calls `list_dir` once per
/// unique local-relative parent, instead of recursively walking every
/// directory under `dest_root`.
///
/// Why this exists: a full `list_dir_recursive(dest_root)` walks the
/// entire destination tree even when the local upload is a single file
/// or a shallow subset. Uploading one 28 GB image into a folder that
/// already holds other games used to spend seconds-to-minutes scanning
/// unrelated GBs before diffing. With scoped listing, the remote work
/// is bounded by the shape of the *local* tree, not the destination's.
///
/// ENOENT on any parent is silently treated as "nothing on the PS5 side
/// here yet", so the diff proceeds to mark everything under that parent
/// as to-send. Matches the contract `list_dir_recursive` had for a
/// missing root.
/// Process-global serializer for the *whole* reconcile (local walk + remote
/// directory walk), acquired at the top of `reconcile()`.
///
/// A folder upload fires a reconcile, and the Upload screen fires a debounced
/// dir-diff-preview. Two problems this gate solves:
///
///   1. mgmt-port storm: without serialization, several 800+-call remote walks
///      run at once, each opening a management call per
///      directory. That connection storm
///      overran the payload's small accept backlog + 8-thread mgmt cap and
///      wedged/crashed the helper (reported on "it takes two", ~863 dirs → red
///      helper, failed upload, shards_incomplete, fs_delete_failed).
///
///   2. slow-source storm: the LOCAL walk runs first and, on a network source
///      (SMB/UNC like `\\server\share`), takes MINUTES — each `read_dir`/`stat`
///      is a network round-trip. The gate used to live inside the remote walk,
///      so a preview did the full multi-minute local walk *before* reaching it,
///      then bailed too late. Several previews + the real upload therefore
///      walked the same slow share concurrently, saturating it and taking the
///      helper down (reported on an 8571-file game served from `\\RRD-Storage`:
///      three overlapping ~134 s local walks, payload went down mid-upload).
///
/// Gating the entire reconcile means a best-effort preview bails *before* the
/// expensive local walk, and the console sees at most one mgmt connection at a
/// time from this path.
///
/// **Multi-console:** the two hazards above are per-RESOURCE — the mgmt-port
/// storm is per-CONSOLE (addr) and the slow-source storm is per-SHARE (src
/// root). A single process-global mutex would over-serialize: a folder upload
/// to console A would block an unrelated folder upload to console B from a
/// different source, defeating parallel-console operation. Instead each
/// reconcile atomically reserves a SET of keys — `{addr:<ps5>, src:<source>}` —
/// under one mutex; two reconciles conflict iff their key sets intersect. Same
/// console OR same source → serialized (both protections intact); different
/// console AND different source → fully parallel.
static RECONCILE_KEYS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static RECONCILE_CV: std::sync::Condvar = std::sync::Condvar::new();
/// The keys real uploads are waiting for. A preview holding one gives way: it is optional, and an
/// upload the user just started must not sit behind a walk nobody is waiting for any more (the
/// panel that asked for it hides once the upload runs).
static RECONCILE_WANTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn reconcile_wanted(keys: &[String]) -> bool {
    RECONCILE_WANTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|k| keys.contains(k))
}

/// Withdraws an upload's wish for its keys, however its wait ends.
struct WantedKeys<'a>(&'a [String]);

impl Drop for WantedKeys<'_> {
    fn drop(&mut self) {
        let mut wanted = RECONCILE_WANTED.lock().unwrap_or_else(|e| e.into_inner());
        for k in self.0 {
            if let Some(i) = wanted.iter().position(|w| w == k) {
                wanted.swap_remove(i);
            }
        }
    }
}

fn preview_yielded() -> anyhow::Error {
    anyhow::anyhow!("reconcile_busy: an upload is waiting for this scan")
}

/// RAII reservation of a reconcile's key set. Drop releases every key and wakes
/// any reconcile waiting on a now-free key.
struct ReconcileGate {
    keys: Vec<String>,
}

impl Drop for ReconcileGate {
    fn drop(&mut self) {
        let mut held = RECONCILE_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|k| !self.keys.contains(k));
        RECONCILE_CV.notify_all();
    }
}

/// Reserve `keys` atomically. `block=true` (real upload) waits until every key
/// is free; `block=false` (preview) bails immediately with `reconcile_busy` if
/// any key is contended — before the expensive local walk.
fn acquire_reconcile_gate(keys: Vec<String>, block: bool) -> Result<ReconcileGate> {
    if block {
        RECONCILE_WANTED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(keys.iter().cloned());
    }
    let _wanted = WantedKeys(if block { &keys } else { &[] });
    let mut held = RECONCILE_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let conflict = keys.iter().any(|k| held.contains(k));
        if !conflict {
            break;
        }
        if !block {
            crate::clog!(Info, "transfer", 
                "reconcile: a reconcile/preview sharing this console or source is already running — skipping this preview before the local walk to protect the source + mgmt port",
            );
            return Err(anyhow::anyhow!(
                "reconcile_busy: another remote scan is already in progress"
            ));
        }
        held = RECONCILE_CV.wait(held).unwrap_or_else(|e| e.into_inner());
    }
    for k in &keys {
        held.push(k.clone());
    }
    drop(held);
    drop(_wanted);
    Ok(ReconcileGate { keys })
}

/// Safety valve: above this many unique parent directories, skip the
/// per-parent reconcile walk entirely (treat the destination as empty, so
/// every file is (re)sent). Even serialized, a walk this large is minutes of
/// sequential round-trips; uploading without the size-dedup is the safer,
/// still-correct choice. Normal games are far below this.
const MAX_RECONCILE_PARENT_DIRS: usize = 12_000;

// Hard compile-time bounds so a bad edit is caught immediately (even
// without running the unit test). Matches the "reconcile_dir_cap_is_sane" test.
const _: () = assert!(MAX_RECONCILE_PARENT_DIRS >= 2_000);
const _: () = assert!(MAX_RECONCILE_PARENT_DIRS <= 100_000);

/// True only for ENOENT(2) encoded as the payload's `errno_2` token — NOT
/// `errno_20`..`errno_29` (ENOTDIR/EISDIR/EMFILE/ENOSPC/…). A plain
/// `contains("errno_2")` swallows those structural errors as "missing → empty",
/// which both hides real failures and defeats reconcile dedup. Anchors the match
/// by requiring the char after `errno_2` to be a non-digit (or end of string).
fn is_enoent_errno(es: &str) -> bool {
    let mut from = 0;
    while let Some(i) = es[from..].find("errno_2") {
        let end = from + i + "errno_2".len();
        match es[end..].chars().next() {
            Some(c) if c.is_ascii_digit() => from = end, // errno_20..29 — keep scanning
            _ => return true,                            // errno_2 at a boundary — ENOENT
        }
    }
    false
}

/// Build the scoped remote inventory. The caller (`reconcile`) already holds
/// this console's `addr:` reconcile key (see RECONCILE_KEYS), so at most one
/// scan runs against any single console at a time — at most one mgmt connection
/// is open per console from this path. (Scans against DIFFERENT consoles may
/// run concurrently; that's safe — the mgmt-storm hazard is per-console.)
fn list_remote_scoped(
    addr: &str,
    dest_root: &str,
    local: &LocalInventory,
    yield_now: &dyn Fn() -> bool,
) -> Result<RemoteInventory> {
    use std::collections::BTreeSet;
    let mut parent_rels: BTreeSet<String> = BTreeSet::new();
    for rel in local.keys() {
        let parent = match rel.rfind('/') {
            Some(i) => rel[..i].to_string(),
            None => String::new(),
        };
        parent_rels.insert(parent);
    }
    crate::clog!(
        Info,
        "transfer",
        "list_remote_scoped: {} unique parent dir(s) to list under {}",
        parent_rels.len(),
        dest_root,
    );

    // Safety valve for pathologically large trees: skip reconcile and let
    // the caller (re)send everything rather than spend minutes walking.
    if parent_rels.len() > MAX_RECONCILE_PARENT_DIRS {
        crate::clog!(Info, "transfer", 
            "list_remote_scoped: {} parent dir(s) exceeds cap {} — skipping reconcile; all files will be (re)sent",
            parent_rels.len(),
            MAX_RECONCILE_PARENT_DIRS,
        );
        return Ok(RemoteInventory::new());
    }

    // Note: per-console serialization is handled by the caller, which holds
    // this console's reconcile key (RECONCILE_KEYS) across the local walk + this
    // remote walk, so we don't re-acquire it here.

    // Probe dest_root up-front. Serves two purposes:
    //   1. Fast ENOENT path: if the destination doesn't exist at all,
    //      every deeper parent is necessarily missing too. Return an
    //      empty inventory with 1 round-trip instead of N.
    //   2. Fast fail path: if the mgmt service is unhealthy (timeout,
    //      connection refused, protocol error), propagate that error
    //      immediately. The per-parent walk below will hit the same
    //      failure every time, so attempting it N more times just makes
    //      the user wait N×timeout before seeing the error.
    let probe = list_dir_with_timeout(
        addr,
        dest_root,
        ListDirOptions {
            offset: 0,
            limit: 1,
        },
        Some(std::time::Duration::from_secs(10)),
    );
    match &probe {
        Ok(_) => {
            // Destination exists and is listable — proceed to scoped walk.
        }
        Err(e) => {
            let es = e.to_string();
            if es.contains("ENOENT")
                || es.contains("not found")
                || es.contains("no such")
                || is_enoent_errno(&es)
            {
                crate::clog!(Info, "transfer", 
                    "list_remote_scoped: dest_root {} does not exist on PS5 — treating as empty, skipping {} per-parent list_dir call(s)",
                    dest_root,
                    parent_rels.len(),
                );
                return Ok(RemoteInventory::new());
            }
            // Any other probe failure (timeout, connection error,
            // malformed response) — surface immediately. There's no
            // point attempting N per-parent calls when the first
            // round-trip already told us the mgmt service is unwell.
            crate::clog!(Info, "transfer", 
                "list_remote_scoped: dest_root probe failed ({}) — aborting reconcile instead of attempting {} per-parent list_dir call(s)",
                e,
                parent_rels.len(),
            );
            return Err(anyhow::anyhow!("dest_root probe failed: {e}"));
        }
    }

    let mut out = RemoteInventory::new();
    for (i, parent_rel) in parent_rels.iter().enumerate() {
        if yield_now() {
            return Err(preview_yielded());
        }
        let abs = if parent_rel.is_empty() {
            dest_root.to_string()
        } else {
            format!("{dest_root}/{parent_rel}")
        };
        let started = std::time::Instant::now();
        let mut pages = 0u32;
        let mut entries_seen = 0u64;
        let mut offset = 0u64;
        loop {
            crate::clog!(
                Info,
                "transfer",
                "list_remote_scoped: [{}/{}] list_dir({}) offset={} limit=256",
                i + 1,
                parent_rels.len(),
                abs,
                offset,
            );
            let listing = match list_dir_with_timeout(
                addr,
                &abs,
                ListDirOptions { offset, limit: 256 },
                Some(std::time::Duration::from_secs(10)),
            ) {
                Ok(v) => v,
                Err(e) => {
                    let es = e.to_string();
                    if offset == 0
                        && (es.contains("ENOENT")
                            || es.contains("not found")
                            || es.contains("no such")
                            || is_enoent_errno(&es))
                    {
                        crate::clog!(
                            Info,
                            "transfer",
                            "list_remote_scoped: [{}/{}] {} missing on PS5 — treating as empty",
                            i + 1,
                            parent_rels.len(),
                            abs,
                        );
                        break;
                    }
                    crate::clog!(
                        Info,
                        "transfer",
                        "list_remote_scoped: [{}/{}] list_dir({}) offset={} ERROR: {}",
                        i + 1,
                        parent_rels.len(),
                        abs,
                        offset,
                        e,
                    );
                    return Err(e);
                }
            };
            pages += 1;
            entries_seen += listing.entries.len() as u64;
            for entry in &listing.entries {
                if entry.kind != "file" {
                    continue;
                }
                let rel_child = if parent_rel.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{parent_rel}/{}", entry.name)
                };
                out.insert(rel_child, entry.size);
            }
            offset += listing.entries.len() as u64;
            // `truncated` only means "the response buffer filled up" — it
            // is NOT set when a full `limit`-sized page is returned. A dir
            // with exactly 300 files returns 256 entries with
            // truncated=false, so breaking on `!truncated` would silently
            // drop files 257+. The natural end of a listing is a *short*
            // page (fewer than `limit`) that wasn't buffer-truncated; a
            // full 256-entry page (truncated or not) means "ask again".
            // The empty-page check covers the exact-multiple-of-256 case.
            if listing.entries.is_empty()
                || (!listing.truncated && (listing.entries.len() as u64) < 256)
            {
                break;
            }
        }
        crate::clog!(
            Info,
            "transfer",
            "list_remote_scoped: [{}/{}] {} done — {} entries in {} page(s), {} ms",
            i + 1,
            parent_rels.len(),
            abs,
            entries_seen,
            pages,
            started.elapsed().as_millis(),
        );
    }
    crate::clog!(
        Info,
        "transfer",
        "list_remote_scoped: total {} remote file(s) across {} parent dir(s)",
        out.len(),
        parent_rels.len(),
    );
    Ok(out)
}

/// Reconcile a local source directory against the PS5 destination and
/// produce a list of files to upload. In Fast mode, skip when remote
/// size matches local size. In Safe mode, additionally require that
/// BLAKE3(remote) == BLAKE3(local) — re-upload on any mismatch.
pub fn reconcile(
    addr: &str,
    src: &std::path::Path,
    dest_root: &str,
    mode: ReconcileMode,
    excludes: &[String],
    // true  → this is the real upload; wait for the remote-walk gate.
    // false → best-effort preview; skip with `reconcile_busy` if a walk runs.
    block_for_gate: bool,
) -> Result<ReconcilePlan> {
    // Serialize the ENTIRE reconcile (local walk + remote scan) per console +
    // per source — see RECONCILE_KEYS. The local walk runs first and, on a slow
    // network source (SMB/UNC), takes minutes; gating up-front means a
    // best-effort preview bails BEFORE that walk instead of running several
    // concurrent
    // multi-minute walks against the same share. The real upload
    // (block_for_gate=true) waits its turn; a preview (false) bails fast.
    // Reserve this reconcile's console (addr) and source (src root) keys. A
    // concurrent reconcile to a DIFFERENT console from a DIFFERENT source
    // proceeds in parallel; one sharing either key serializes behind us (real
    // upload waits, preview bails) — see RECONCILE_KEYS.
    let keys = vec![
        format!("addr:{addr}"),
        format!("src:{}", src.to_string_lossy()),
    ];
    let _walk_gate = acquire_reconcile_gate(keys.clone(), block_for_gate)?;
    let yield_now = || !block_for_gate && reconcile_wanted(&keys);

    let t_local = std::time::Instant::now();
    crate::clog!(
        Info,
        "transfer",
        "reconcile: walking local {} …",
        src.display(),
    );
    let local = walk_local_inventory_until(src, excludes, &yield_now)?;
    crate::clog!(
        Info,
        "transfer",
        "reconcile: local walk {} files ({} ms)",
        local.len(),
        t_local.elapsed().as_millis(),
    );
    let t_remote = std::time::Instant::now();
    let remote = list_remote_scoped(addr, dest_root, &local, &yield_now)?;
    crate::clog!(
        Info,
        "transfer",
        "reconcile: remote inventory built ({} ms, mode={:?}) — starting diff",
        t_remote.elapsed().as_millis(),
        mode,
    );
    let mut to_send = Vec::new();
    let mut bytes_to_send: u64 = 0;
    let mut already_present: u64 = 0;
    let mut bytes_already_present: u64 = 0;
    for (rel, &local_size) in &local {
        let remote_size = remote.get(rel).copied();
        let needs_send = match remote_size {
            None => true,
            Some(rs) if rs != local_size => true,
            Some(_) if matches!(mode, ReconcileMode::Safe) => {
                // Size matches; verify hashes match too. If either side
                // fails to hash (unreadable local file, PS5-side EACCES,
                // mgmt-port timeout), treat the file as "unverified —
                // must re-send" rather than failing the whole reconcile.
                // The pre-2.2.28 behaviour `?`-propagated the error,
                // which aborted reconciliation on a single problem file
                // — a flaky USB or one stale permission would prevent
                // the user from resuming a 200k-file upload at all. The
                // worst case of treating-as-unverified is one extra
                // file in the to-send list; the upload still progresses.
                let local_path = src.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                let local_hash_opt =
                    match blake3_file(&local_path) {
                        Ok(h) => Some(h),
                        Err(e) => {
                            crate::clog!(Info, "transfer", 
                            "reconcile: blake3 failed for local {} ({}); treating as must-resend",
                            local_path.display(),
                            e,
                        );
                            None
                        }
                    };
                if let Some(local_hash) = local_hash_opt {
                    let remote_path = format!("{dest_root}/{rel}");
                    // 10 s per-file deadline so a single hung PS5 doesn't
                    // stall the whole reconcile linearly. The default
                    // 30 s socket timeout would multiply N files × 30 s
                    // on a crashed payload — at hundreds of files the
                    // user would think the app is dead.
                    match hash_remote_waiting_out_busy(addr, &remote_path)? {
                        Ok(r) => local_hash != r.hash,
                        Err(e) => {
                            crate::clog!(Info, "transfer", 
                                "reconcile: fs_hash failed for remote {} ({}); treating as must-resend",
                                remote_path,
                                e,
                            );
                            true
                        }
                    }
                } else {
                    // Local hash failed — must-resend, no remote round-trip.
                    true
                }
            }
            Some(_) => false,
        };
        if needs_send {
            to_send.push(ReconcileFile {
                rel_path: rel.clone(),
                size: local_size,
            });
            bytes_to_send += local_size;
        } else {
            already_present += 1;
            bytes_already_present += local_size;
        }
    }
    Ok(ReconcilePlan {
        to_send,
        bytes_to_send,
        already_present,
        bytes_already_present,
    })
}

/// `ERR_BUSY` (AVA1 status 8): the console has no free job slot or operation worker right now.
const STATUS_BUSY: u16 = 8;

/// Hashes one remote file for the reconcile. A busy console is not an answer about the file:
/// waiting it out (up to ~15 s) is right, and giving up is an error of the whole reconcile, never a
/// silent "unverified, must re-send" (which would re-upload everything while the console is merely
/// working). Any other failure is the inner `Err`, which the caller treats as unverified.
fn hash_remote_waiting_out_busy(addr: &str, remote_path: &str) -> Result<Result<HashResult>> {
    for attempt in 0..60 {
        match fs_hash_with_timeout(addr, remote_path, Some(std::time::Duration::from_secs(10))) {
            Err(e)
                if e.downcast_ref::<mgmt::MgmtError>()
                    .is_some_and(|m| m.status == STATUS_BUSY) =>
            {
                if attempt == 59 {
                    return Err(e.context("reconcile: the console stayed busy"));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            r => return Ok(r),
        }
    }
    unreachable!("the loop returns on its last attempt")
}

/// Stream a local file through BLAKE3 in 64 KiB chunks. Mirrors the
/// payload's FS_HASH streaming behavior so hex outputs compare directly.
fn blake3_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut hasher = blake3::Hasher::new();
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("open {} for hashing", path.display()))?;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::UnregisterOutcome;

    /// What the console does: one read returns at most 2 MiB, and the file ends where it ends.
    fn read_capped(file: &[u8], at: u64, want: u64) -> anyhow::Result<Vec<u8>> {
        let start = (at as usize).min(file.len());
        let end = (start + want.min(2 * 1024 * 1024) as usize).min(file.len());
        Ok(file[start..end].to_vec())
    }

    #[test]
    fn a_whole_file_is_read_past_the_helpers_ceiling() {
        let file: Vec<u8> = (0..5 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let got = super::read_whole_with("/f", file.len() as u64, |at, want| {
            read_capped(&file, at, want)
        })
        .unwrap();
        assert_eq!(got, file);
    }

    #[test]
    fn a_file_shorter_than_its_listing_is_an_error_not_a_short_result() {
        let file = vec![7u8; 1000];
        let e = super::read_whole_with("/f", 3000, |at, want| read_capped(&file, at, want))
            .unwrap_err();
        assert_eq!(e.to_string(), "/f ended at 1000 of 3000 bytes");
    }

    /// A listing that claims an impossible size is refused before anything is reserved or read.
    #[test]
    fn a_file_listed_as_huge_is_refused_unread() {
        let e =
            super::read_whole_with("/f", 1 << 45, |_, _| panic!("no read expected")).unwrap_err();
        assert!(e.to_string().contains("more than"), "{e}");
        let e = super::read_whole_with("/f", u64::MAX, |_, _| panic!("no read expected"));
        assert!(e.is_err());
    }

    #[test]
    fn a_reply_longer_than_asked_is_cut_to_the_size() {
        let got = super::read_whole_with("/f", 3, |_, _| Ok(vec![1u8; 10])).unwrap();
        assert_eq!(got, [1, 1, 1]);
    }

    #[test]
    fn an_empty_file_needs_no_read() {
        let got = super::read_whole_with("/f", 0, |_, _| panic!("no read expected")).unwrap();
        assert!(got.is_empty());
    }

    /// The request is the payload's args JSON as-is: snake_case keys, `dlc` on unless said.
    #[test]
    fn app_dump_request_is_the_payloads_args() {
        let req = super::AppDumpRequest {
            title_id: "PPSA01234".into(),
            dest: "/mnt/usb0/ps5upload-dumps".into(),
            decrypt: false,
            fself: false,
            dlc: true,
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"title_id":"PPSA01234","dest":"/mnt/usb0/ps5upload-dumps","decrypt":false,"fself":false,"dlc":true}"#
        );
        let sparse: super::AppDumpRequest =
            serde_json::from_str(r#"{"title_id":"CUSA00001","dest":"/mnt/ext0"}"#).unwrap();
        assert!(!sparse.decrypt && !sparse.fself && sparse.dlc);
    }

    /// The payload's result decodes whole, and an older or partial one still decodes.
    #[test]
    fn app_dump_result_decodes_the_payloads_reply() {
        let r: super::AppDumpResult = serde_json::from_str(
            r#"{"path":"/mnt/usb0/d/PPSA01234-app","files":12,"bytes":3456,"decrypted":3,
                "decrypt_failed":1,"decrypt_skipped":"","dlc":["EP0001-PPSA01234_00-DLC1-ac"],
                "fw":"0x13600000"}"#,
        )
        .unwrap();
        assert_eq!(r.path, "/mnt/usb0/d/PPSA01234-app");
        assert_eq!(
            (r.files, r.bytes, r.decrypted, r.decrypt_failed),
            (12, 3456, 3, 1)
        );
        assert_eq!(r.dlc, vec!["EP0001-PPSA01234_00-DLC1-ac".to_string()]);
        assert_eq!(r.fw, "0x13600000");
        let skipped: super::AppDumpResult = serde_json::from_str(
            r#"{"path":"/x","decrypt_skipped":"dump_decrypt_unsupported_fw"}"#,
        )
        .unwrap();
        assert_eq!(skipped.decrypt_skipped, "dump_decrypt_unsupported_fw");
        assert!(skipped.dlc.is_empty() && skipped.fw.is_empty() && skipped.files == 0);
    }

    /// A Sony refusal must not read as a clean uninstall. The payload
    /// reports `sceAppInstUtilAppUninstall`'s rc in the ACK body; a
    /// non-zero value means the console kept the title even though our
    /// own teardown succeeded. This is the case a user hit with
    /// `0x80B21B02`, where the tool reported success and the title stayed
    /// in Settings → Storage.
    #[test]
    fn a_nonzero_sony_rc_is_reported_as_refused() {
        let ok = UnregisterOutcome::from_ack(br#"{"sony_uninstall_rc":0}"#);
        assert!(!ok.sony_refused(), "rc 0 means Sony accepted");
        assert_eq!(ok.still_present, None, "an older helper did not check");

        // 7.1.5 and older helpers ACK a refusal with the code in the body.
        let refused = UnregisterOutcome::from_ack(br#"{"sony_uninstall_rc":2159155970}"#);
        assert!(refused.sony_refused());
        assert_eq!(format!("0x{:08X}", refused.sony_uninstall_rc), "0x80B21B02");
    }

    /// Older payloads answer APP_UNREGISTER with an EMPTY body. That must
    /// degrade to "Sony's result unknown" (rc 0) rather than failing the
    /// call, so a new engine keeps working against an old payload.
    #[test]
    fn an_empty_ack_body_degrades_to_rc_zero() {
        let parsed = UnregisterOutcome::from_ack(b"");
        assert_eq!(parsed.sony_uninstall_rc, 0);
        assert!(!parsed.sony_refused());
        assert_eq!(parsed.still_present, None);
    }

    /// #476: a 7.1.6 helper says whether the title's folders were still there
    /// after Sony accepted the uninstall.
    #[test]
    fn still_present_is_read_from_the_ack() {
        let gone = UnregisterOutcome::from_ack(br#"{"sony_uninstall_rc":0,"still_present":false}"#);
        assert_eq!(gone.still_present, Some(false));
        let left = UnregisterOutcome::from_ack(br#"{"sony_uninstall_rc":0,"still_present":true}"#);
        assert_eq!(left.still_present, Some(true));
        assert!(!left.sony_refused());
    }

    /// #476 review: our folder registration was removed although Sony refused.
    #[test]
    fn a_refusal_for_a_title_that_is_gone_does_not_stand() {
        let gone = UnregisterOutcome::from_ack(
            br#"{"sony_uninstall_rc":2159155970,"still_present":false}"#,
        );
        assert!(gone.sony_refused());
        assert!(!gone.refusal_stands());
        let legacy = UnregisterOutcome::from_ack(br#"{"sony_uninstall_rc":2159155970}"#);
        assert!(legacy.refusal_stands(), "an older helper did not check");
    }

    use super::*;

    /// All gate tests below manipulate the process-global RECONCILE_KEYS set.
    /// cargo runs tests in parallel by default, so without forcing them
    /// sequential, one test holding a key makes another's "free" assertion
    /// flake under load. This dedicated serialization lock makes them run
    /// one-at-a-time; poison-tolerant because a failing test would poison it
    /// and we still want the siblings to run and report honestly.
    static GATE_TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Regression test for the reconcile connection-storm + slow-source crash:
    /// a reconcile that shares a key (same console OR same source) must
    /// serialize so concurrent reconcile + diff-preview requests can never
    /// (a) open overlapping mgmt-port connections to ONE console (crashed the
    /// helper on large games like "it takes two", ~863 dirs) nor (b) run
    /// several multi-minute local walks against ONE slow SMB share (crashed
    /// the helper on an 8571-file game from `\\RRD-Storage`). Exercises the
    /// real static used in production.
    #[test]
    fn reconcile_gate_serializes_same_key() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let _serial = GATE_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let key = || vec!["addr:gatetest-A".to_string(), "src:/gatetest/A".to_string()];

        // (1) A best-effort preview must bail while a conflicting reconcile
        //     holds the key, rather than starting a second concurrent walk.
        {
            let _held = acquire_reconcile_gate(key(), true).unwrap();
            assert!(
                acquire_reconcile_gate(key(), false).is_err(),
                "preview must bail while a conflicting reconcile holds the key"
            );
        }
        // Key is released once the guard drops.
        assert!(
            acquire_reconcile_gate(key(), false).is_ok(),
            "key must be free after the reconcile completes"
        );

        // (2) Many concurrent walkers on the SAME key must never overlap —
        //     observed concurrency stays at exactly 1.
        let max_seen = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..12 {
            let max_seen = Arc::clone(&max_seen);
            let in_flight = Arc::clone(&in_flight);
            handles.push(std::thread::spawn(move || {
                let _g =
                    acquire_reconcile_gate(vec!["addr:gatetest-serial".to_string()], true).unwrap();
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(3));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "reconciles sharing a key must be serialized to one at a time"
        );
    }

    /// The multi-console win: reconciles with DISJOINT keys (different console
    /// AND different source) must run fully in parallel — holding one must not
    /// block the other. This is the regression guard against re-introducing a
    /// single process-global gate that would serialize unrelated consoles.
    #[test]
    fn reconcile_gate_allows_disjoint_keys_in_parallel() {
        let _serial = GATE_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // Console A holds its keys…
        let _held_a = acquire_reconcile_gate(
            vec!["addr:console-A".to_string(), "src:/share/A".to_string()],
            true,
        )
        .unwrap();
        // …console B (different addr AND source) must NOT be blocked, even in
        // the non-blocking preview path that bails on ANY conflict.
        let b = acquire_reconcile_gate(
            vec!["addr:console-B".to_string(), "src:/share/B".to_string()],
            false,
        );
        assert!(
            b.is_ok(),
            "a reconcile for a different console+source must run in parallel"
        );
        // But sharing JUST the source (same share, different console) still
        // conflicts — the slow-source hazard is per-share.
        let shared_src = acquire_reconcile_gate(
            vec!["addr:console-C".to_string(), "src:/share/A".to_string()],
            false,
        );
        assert!(
            shared_src.is_err(),
            "sharing the source share must still serialize (slow-source hazard)"
        );
    }

    /// A best-effort preview (block_for_gate=false) must bail with
    /// `reconcile_busy` BEFORE walking the local tree when a reconcile sharing
    /// its console key is already in flight. This is the fix for the slow-source
    /// crash: the gate wraps the local walk too, so a preview can't run a
    /// multi-minute SMB walk that competes with the active upload. We point
    /// `src` at a path that does NOT exist — if the preview tried to walk it, it
    /// would fail with a read_dir error rather than the clean reconcile_busy, so
    /// this also pins the gate-before-walk ordering.
    #[test]
    fn preview_bails_before_local_walk_when_gate_held() {
        let _serial = GATE_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // Hold the console key the reconcile below will try to reserve.
        let _held = acquire_reconcile_gate(vec!["addr:127.0.0.1".to_string()], true).unwrap();
        let missing = std::path::Path::new("/this/path/should/not/exist/ps5upload-test");
        let err = reconcile(
            "127.0.0.1",
            missing,
            "/data/whatever",
            ReconcileMode::Fast,
            &[],
            false, // best-effort preview
        )
        .expect_err("preview must bail while the gate is held");
        assert!(
            err.to_string().contains("reconcile_busy"),
            "preview must fail with reconcile_busy (bailed before the walk), got: {err}"
        );
    }

    /// The pathological-tree safety valve constant must stay sane: large
    /// enough that real games (hundreds of dirs) still reconcile, small
    /// enough to bound a worst-case sequential walk.
    /// (The actual numeric bounds are hard-enforced with `const _: () = assert!(...)`
    /// right next to the const definition, so bad values fail the *build*.)
    #[test]
    fn reconcile_dir_cap_is_sane() {
        // Test retained purely for documentation / discoverability.
        // No runtime assert here (would trigger clippy::assertions-on-constants
        // and is redundant with the module-level const checks).
        let _ = MAX_RECONCILE_PARENT_DIRS;
    }

    #[test]
    fn parse_sample_listing() {
        let body = br#"{
            "path":"/data",
            "entries":[
                {"name":"games","kind":"dir","size":0},
                {"name":"manifest.json","kind":"file","size":1234,"mtime":1786291200},
                {"name":"link-to-ext0","kind":"link","size":0}
            ],
            "truncated":false,
            "total_scanned":3,
            "returned":3
        }"#;
        let listing: DirListing = serde_json::from_slice(body).unwrap();
        assert_eq!(listing.path, "/data");
        assert_eq!(listing.entries.len(), 3);
        assert_eq!(listing.entries[1].size, 1234);
        assert_eq!(listing.entries[1].mtime, 1_786_291_200);
        assert_eq!(listing.entries[0].mtime, 0); // legacy payload omission
        assert!(!listing.truncated);
    }

    #[test]
    fn parse_truncated_listing() {
        let body =
            br#"{"path":"/data","entries":[],"truncated":true,"total_scanned":1024,"returned":0}"#;
        let listing: DirListing = serde_json::from_slice(body).unwrap();
        assert!(listing.truncated);
        assert_eq!(listing.total_scanned, 1024);
    }

    #[test]
    fn default_options_use_limit_256() {
        let opts = ListDirOptions::default();
        assert_eq!(opts.offset, 0);
        assert_eq!(opts.limit, 256);
    }

    #[test]
    fn a_preview_gives_way_to_an_upload_waiting_for_its_keys() {
        let keys = vec!["addr:yield-test".to_string(), "src:/yield-test".to_string()];
        let held = acquire_reconcile_gate(keys.clone(), false).unwrap();
        assert!(!reconcile_wanted(&keys), "nobody waits yet");

        let upload_keys = vec!["addr:yield-test".to_string(), "src:/other".to_string()];
        let upload = std::thread::spawn(move || {
            acquire_reconcile_gate(upload_keys, true)
                .map(|_| ())
                .is_ok()
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !reconcile_wanted(&keys) {
            assert!(
                std::time::Instant::now() < deadline,
                "the upload never asked"
            );
            std::thread::yield_now();
        }

        // The walk stops at its next folder, with the error the preview route turns into `busy`.
        let e =
            walk_local_inventory_until(std::path::Path::new("/"), &[], &|| reconcile_wanted(&keys))
                .unwrap_err();
        assert!(e.to_string().contains("reconcile_busy"), "{e}");

        drop(held);
        assert!(upload.join().unwrap(), "the upload goes ahead");
        assert!(!reconcile_wanted(&keys), "its wish is withdrawn");
    }

    #[test]
    fn walk_local_inventory_respects_excludes() {
        let root =
            std::env::temp_dir().join(format!("ps5upload-fs-ops-excludes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"real").unwrap();
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
        std::fs::write(root.join(".git").join("HEAD"), b"ref").unwrap();

        let excludes = crate::excludes::DEFAULT_EXCLUDES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let inv = walk_local_inventory(&root, &excludes).unwrap();

        assert!(inv.contains_key("eboot.bin"));
        assert!(!inv.contains_key(".DS_Store"));
        assert!(!inv.contains_key(".git/HEAD"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A console that answers `ERR_BUSY` to the first `busy` hash jobs, then hashes.
    struct BusyThenOk {
        busy: std::sync::atomic::AtomicUsize,
    }

    impl mgmt::MgmtTransport for BusyThenOk {
        fn call(
            &self,
            _: &str,
            _: mgmt::Method,
            _: &str,
            _: &[u8],
            _: std::time::Duration,
        ) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn run_job(
            &self,
            _: &str,
            _: mgmt::JobOp,
            label: &str,
            _: &[u8],
            _: &mgmt::JobCall<'_>,
        ) -> Result<Option<Vec<u8>>> {
            use std::sync::atomic::Ordering;
            // `fetch_update` is the name CI's stable Rust has; newer toolchains rename it to
            // `try_update` (unstable there) and warn. Same behaviour either way.
            #[allow(deprecated)]
            let took = self
                .busy
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if took {
                return Err(mgmt::MgmtError {
                    label: label.into(),
                    status: STATUS_BUSY,
                    cause: "the job table is full".into(),
                }
                .into());
            }
            Ok(Some(br#"{"path":"/p","size":1,"hash":"ab"}"#.to_vec()))
        }
    }

    #[test]
    fn a_busy_console_is_waited_out_never_read_as_must_resend() {
        let _g = mgmt::scoped_transport(std::sync::Arc::new(BusyThenOk { busy: 3.into() }));
        let r = hash_remote_waiting_out_busy("c:1", "/p").unwrap().unwrap();
        assert_eq!(r.hash, "ab");
        // Busy for good is an error of the whole reconcile, not an `Ok(Err(..))` the caller would
        // turn into "unverified".
        let _g = mgmt::scoped_transport(std::sync::Arc::new(BusyThenOk { busy: 1000.into() }));
        assert!(hash_remote_waiting_out_busy("c:1", "/p").is_err());
    }
}
