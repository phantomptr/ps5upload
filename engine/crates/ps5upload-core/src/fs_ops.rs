//! File-system ops over FTX2.
//!
//! This module is the home for all non-transfer RPCs the UI needs:
//! list_dir, stat, mkdir, move, copy, read_file, query_hashes. Starting
//! with `list_dir`; others land in phases.
//!
//! Each helper opens a fresh TCP connection, sends one request frame,
//! awaits the matching ACK, and returns the parsed body. All helpers
//! surface payload-side errors (frames with `FrameType::Error`) as anyhow
//! errors with the payload's error string verbatim, so UI can switch on
//! the vocabulary the payload defines.

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

// ─── FS_LIST_DIR ─────────────────────────────────────────────────────────────

/// One directory entry returned by `list_dir`.
///
/// `kind` is one of `"file"`, `"dir"`, `"link"`, `"other"`, or `"unknown"`
/// (for entries whose `lstat` on the payload side failed). Size is 0 for
/// non-regular-file kinds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: String,
    pub size: u64,
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
    let mut c = Connection::connect(addr)?;
    if let Some(t) = io_timeout {
        c.set_io_timeout(t)
            .context("applying reconcile I/O timeout")?;
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "offset": opts.offset,
        "limit": opts.limit,
    }))
    .context("serialize list_dir body")?;
    c.send_frame(FrameType::FsListDir, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_LIST_DIR({}): {}",
            path,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsListDirAck {
        bail!("expected FS_LIST_DIR_ACK, got {:?}", ft);
    }
    let parsed: DirListing =
        serde_json::from_slice(&resp).context("decode FS_LIST_DIR_ACK body as JSON")?;
    Ok(parsed)
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
    let mut c = Connection::connect(addr)?;
    let body = serde_json::to_vec(&serde_json::json!({ "path": path }))
        .context("serialize fs_hash body")?;
    c.send_frame(FrameType::FsHash, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_HASH({}): {}",
            path,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsHashAck {
        bail!("expected FS_HASH_ACK, got {:?}", ft);
    }
    let parsed: HashResult =
        serde_json::from_slice(&resp).context("decode FS_HASH_ACK body as JSON")?;
    Ok(parsed)
}

// ─── FS_READ ────────────────────────────────────────────────────────────────

/// Ask the payload for up to `limit` bytes of a file on the PS5 starting at
/// `offset`. Used to pull small metadata blobs (`param.json`, `icon0.png`)
/// out of a game folder so the UI can render covers + titles. The payload
/// caps the response at `FS_READ_MAX_BYTES` regardless of `limit`, so large
/// requests are silently truncated — callers that need exact-size reads
/// should chunk with updated `offset` values.
pub fn fs_read(addr: &str, path: &str, offset: u64, limit: u64) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "offset": offset,
        "limit": limit,
    }))
    .context("serialize fs_read body")?;
    c.send_frame(FrameType::FsRead, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_READ({}): {}",
            path,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsReadAck {
        bail!("expected FS_READ_ACK, got {:?}", ft);
    }
    Ok(resp)
}

// ─── Destructive ops (delete / move / chmod / mkdir) ────────────────────────

/// Send a management-port frame that expects an empty ACK body (or an
/// error frame). Used for delete/move/chmod/mkdir which have no data
/// to return on success — the frame type itself is the confirmation.
fn send_empty_ack_op(
    addr: &str,
    frame: FrameType,
    body: &[u8],
    expected: FrameType,
    what: &str,
) -> Result<()> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(frame, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {what}: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != expected {
        bail!("expected {expected:?}, got {ft:?}");
    }
    Ok(())
}

/// Delete a file or directory recursively on the PS5. Path must be under
/// the payload's writable-root allowlist (/data, /user, /mnt/ext*, /mnt/usb*).
pub fn fs_delete(addr: &str, path: &str) -> Result<()> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "path": path })).context("serialize fs_delete")?;
    send_empty_ack_op(
        addr,
        FrameType::FsDelete,
        &body,
        FrameType::FsDeleteAck,
        "FS_DELETE",
    )
}

/// Copy a file or directory recursively on the PS5. Both `from` and `to`
/// must pass the writable-root allowlist; `to` must not already exist.
/// Unlike FS_MOVE (which is rename()-based and errors EXDEV across
/// mounts), FS_COPY works cross-volume — the payload reads and writes
/// bytes explicitly.
pub fn fs_copy(addr: &str, from: &str, to: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "from": from, "to": to }))
        .context("serialize fs_copy")?;
    send_empty_ack_op(
        addr,
        FrameType::FsCopy,
        &body,
        FrameType::FsCopyAck,
        "FS_COPY",
    )
}

// ─── FS_MOUNT / FS_UNMOUNT ─────────────────────────────────────────────────

/// Return shape from FS_MOUNT_ACK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResult {
    pub mount_point: String,
    pub dev_node: String,
    pub fstype: String,
}

/// Mount a disk image on the PS5. `image_path` must be an absolute path
/// under the payload's writable-root allowlist and have a `.exfat` or
/// `.ffpkg` extension. `mount_name` is optional — when None the payload
/// derives a filesystem-safe name from the image basename. Mount points
/// always live under `/mnt/ps5upload/`.
pub fn fs_mount(addr: &str, image_path: &str, mount_name: Option<&str>) -> Result<MountResult> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::to_vec(&serde_json::json!({
        "image_path": image_path,
        "mount_name": mount_name,
    }))
    .context("serialize fs_mount body")?;
    c.send_frame(FrameType::FsMount, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_MOUNT({}): {}",
            image_path,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsMountAck {
        bail!("expected FS_MOUNT_ACK, got {:?}", ft);
    }
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
    send_empty_ack_op(
        addr,
        FrameType::FsUnmount,
        &body,
        FrameType::FsUnmountAck,
        "FS_UNMOUNT",
    )
}

/// Rename/move a file or directory intra-volume. Cross-volume moves
/// surface as `fs_move_cross_mount` error — payload uses `rename(2)` which
/// returns EXDEV across mount points.
pub fn fs_move(addr: &str, from: &str, to: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "from": from, "to": to }))
        .context("serialize fs_move")?;
    send_empty_ack_op(
        addr,
        FrameType::FsMove,
        &body,
        FrameType::FsMoveAck,
        "FS_MOVE",
    )
}

/// Change permissions. `mode` is octal like "0777" (passed as string so
/// JSON parsing doesn't alter the intended octal value). If `recursive`
/// is true and path is a directory, walks + chmod's every entry.
pub fn fs_chmod(addr: &str, path: &str, mode: &str, recursive: bool) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "mode": mode,
        "recursive": if recursive { 1 } else { 0 },
    }))
    .context("serialize fs_chmod")?;
    send_empty_ack_op(
        addr,
        FrameType::FsChmod,
        &body,
        FrameType::FsChmodAck,
        "FS_CHMOD",
    )
}

/// Create a directory (and any missing parents). Idempotent — succeeds
/// if the directory already exists.
pub fn fs_mkdir(addr: &str, path: &str) -> Result<()> {
    let body =
        serde_json::to_vec(&serde_json::json!({ "path": path })).context("serialize fs_mkdir")?;
    send_empty_ack_op(
        addr,
        FrameType::FsMkdir,
        &body,
        FrameType::FsMkdirAck,
        "FS_MKDIR",
    )
}

// ─── App lifecycle (register / unregister / launch / list) ─────────────────
//
// Mirrors the payload's register.c pipeline. See specs/ftx2-protocol.md
// for the wire shape. "Register" stages + installs a title dir;
// "Launch" calls sceLncUtilLaunchApp on an already-registered title.

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
pub fn app_register(addr: &str, src_path: &str) -> Result<RegisterResult> {
    let mut c = Connection::connect(addr)?;
    let body = serde_json::to_vec(&serde_json::json!({ "src_path": src_path }))
        .context("serialize app_register body")?;
    c.send_frame(FrameType::AppRegister, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected APP_REGISTER({}): {}",
            src_path,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::AppRegisterAck {
        bail!("expected APP_REGISTER_ACK, got {:?}", ft);
    }
    let parsed: RegisterResult =
        serde_json::from_slice(&resp).context("decode APP_REGISTER_ACK body as JSON")?;
    Ok(parsed)
}

/// Reverse of `app_register`. Unmounts the nullfs at
/// `/system_ex/app/<title_id>/`, removes tracking link files, and
/// (where available) calls Sony's AppUninstall to clear the XMB tile.
/// Best-effort: returns Ok even when the Sony API is missing, as long
/// as the unmount succeeded.
pub fn app_unregister(addr: &str, title_id: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "title_id": title_id }))
        .context("serialize app_unregister body")?;
    send_empty_ack_op(
        addr,
        FrameType::AppUnregister,
        &body,
        FrameType::AppUnregisterAck,
        "APP_UNREGISTER",
    )
}

/// Launch an already-registered title via `sceLncUtilLaunchApp`. The
/// title must exist in `app.db` — call `app_register` first if needed.
pub fn app_launch(addr: &str, title_id: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "title_id": title_id }))
        .context("serialize app_launch body")?;
    send_empty_ack_op(
        addr,
        FrameType::AppLaunch,
        &body,
        FrameType::AppLaunchAck,
        "APP_LAUNCH",
    )
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

/// List titles registered in Sony's `app.db`. Fails with
/// `list_sqlite_unavailable` on firmwares where `libSceSqlite` has
/// been renamed or removed; callers should surface that as "Library
/// filter unavailable on this firmware" rather than a hard error.
pub fn app_list_registered(addr: &str) -> Result<RegisteredApps> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::AppListRegistered, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected APP_LIST_REGISTERED: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::AppListRegisteredAck {
        bail!("expected APP_LIST_REGISTERED_ACK, got {:?}", ft);
    }
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
pub fn walk_local_inventory(root: &std::path::Path) -> Result<LocalInventory> {
    let mut out = LocalInventory::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?;
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                let size = entry.metadata().map_or(0, |m| m.len());
                let path = entry.path();
                let rel = path.strip_prefix(root).unwrap_or(&path);
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
fn list_remote_scoped(
    addr: &str,
    dest_root: &str,
    local: &LocalInventory,
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
    crate::core_log!(
        "list_remote_scoped: {} unique parent dir(s) to list under {}",
        parent_rels.len(),
        dest_root,
    );

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
                || es.contains("errno_2")
            {
                crate::core_log!(
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
            crate::core_log!(
                "list_remote_scoped: dest_root probe failed ({}) — aborting reconcile instead of attempting {} per-parent list_dir call(s)",
                e,
                parent_rels.len(),
            );
            return Err(anyhow::anyhow!("dest_root probe failed: {e}"));
        }
    }

    let mut out = RemoteInventory::new();
    for (i, parent_rel) in parent_rels.iter().enumerate() {
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
            crate::core_log!(
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
                            || es.contains("errno_2"))
                    {
                        crate::core_log!(
                            "list_remote_scoped: [{}/{}] {} missing on PS5 — treating as empty",
                            i + 1,
                            parent_rels.len(),
                            abs,
                        );
                        break;
                    }
                    crate::core_log!(
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
            if listing.entries.is_empty() || !listing.truncated {
                break;
            }
        }
        crate::core_log!(
            "list_remote_scoped: [{}/{}] {} done — {} entries in {} page(s), {} ms",
            i + 1,
            parent_rels.len(),
            abs,
            entries_seen,
            pages,
            started.elapsed().as_millis(),
        );
    }
    crate::core_log!(
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
) -> Result<ReconcilePlan> {
    let t_local = std::time::Instant::now();
    crate::core_log!("reconcile: walking local {} …", src.display(),);
    let local = walk_local_inventory(src)?;
    crate::core_log!(
        "reconcile: local walk {} files ({} ms)",
        local.len(),
        t_local.elapsed().as_millis(),
    );
    let t_remote = std::time::Instant::now();
    let remote = list_remote_scoped(addr, dest_root, &local)?;
    crate::core_log!(
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
                // Size matches; verify hashes match too.
                let local_hash =
                    blake3_file(&src.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))?;
                let remote_path = format!("{dest_root}/{rel}");
                let remote_hash = fs_hash(addr, &remote_path)?.hash;
                local_hash != remote_hash
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
    use super::*;

    #[test]
    fn parse_sample_listing() {
        let body = br#"{
            "path":"/data",
            "entries":[
                {"name":"games","kind":"dir","size":0},
                {"name":"manifest.json","kind":"file","size":1234},
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
}
