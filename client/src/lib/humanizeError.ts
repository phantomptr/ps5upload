// Shared error humanizer. Payload/engine error strings leak internals
// ("payload rejected FS_LIST_DIR: fs_list_dir_opendir_errno_2",
// "connect to 192.168.1.2:9114: Connection refused") that are useful
// in logs but confusing in the UI. This module rewrites the patterns
// we recognize into one-line human hints; unknown errors pass through
// unchanged so we never swallow diagnostic signal.
//
// Keeping this a pure function with no React dependency lets any
// screen (Upload, Volumes, Library, Hardware, Logs) call it with the
// same ruleset — a new firmware-specific failure mode only has to be
// added once.

/** Rewrite a raw engine/payload error string into a single line of
 *  user-facing copy. Unknown strings are returned as-is. */
export function humanizePs5Error(raw: string): string {
  if (!raw) return "";

  // ─── Source / local filesystem ─────────────────────────────────────
  // "can't read source folder: /Users/me/game" — the engine failed to
  // walk the user's local tree. We return verbatim because the path
  // is already what they need to see.
  if (/can't read source folder/i.test(raw)) {
    return raw;
  }

  // ─── Mid-transfer network drop ─────────────────────────────────────
  // The payload closed the TCP connection before sending its
  // BEGIN_TX/COMMIT_TX ACK. Engine retries once or twice; if we get
  // here the retry budget was exhausted.
  if (/read frame header|unexpected ?eof|connection reset|broken pipe/i.test(raw)) {
    return "Your PS5 stopped responding. It may have crashed or entered rest mode. Reload the payload (Connection → Send payload) and try again.";
  }

  // ─── PS5 unreachable at connect time ───────────────────────────────
  if (/connect to .+:9114/i.test(raw)) {
    return "Can't reach your PS5's management service. Make sure the payload is loaded (Connection → Send payload).";
  }
  if (/connect to .+:9113/i.test(raw)) {
    return "Can't reach your PS5 for file transfer. Make sure the payload is loaded (Connection → Send payload).";
  }

  // ─── Filesystem errors surfaced from the payload ───────────────────
  // Tight match: we only want to rewrite errors clearly originating
  // from the PS5 payload's FS path. A bare word "permission" used to
  // match unrelated Tauri / OS dialogs (e.g., "IPC permission denied"
  // during updater probes) and turned their error copy into a
  // destination-write hint. Anchor on the payload-side error shapes.
  //
  // Mount-specific errors are checked FIRST below so a payload
  // `fs_mount_attach_failed: lvd=Permission denied md=...` doesn't
  // get caught by this generic EACCES branch — the mount copy is
  // strictly more useful for that frame.
  if (
    !/fs_mount_/i.test(raw) &&
    /EACCES|permission[ _]?denied|\bfs_\w*_permission/i.test(raw)
  ) {
    return "The PS5 refused to write to this destination. Try a different storage volume or destination folder.";
  }
  if (/ENOSPC|no space left|disk.*full/i.test(raw)) {
    return "Your PS5 storage is full at that destination. Pick a different volume or free up space.";
  }

  // ─── Firmware / Sony-API availability ──────────────────────────────
  // These errors originate from the payload when a Sony API or kernel
  // interface isn't available on the running firmware. None of them
  // are fatal to the rest of the app — they gate one specific feature
  // and the user can keep using everything else.
  if (/fs_list_volumes_getmntinfo_failed/i.test(raw)) {
    // `getmntinfo(MNT_NOWAIT)` returned -1 / empty. Usually transient
    // (another process momentarily locked the mount table). Retrying
    // almost always succeeds. Not firmware-specific.
    return "PS5 didn't return the volume list this time — try again in a second. If it keeps failing, reload the payload from Connection → Send payload.";
  }
  if (/sqlite_unavailable/i.test(raw)) {
    // `libSceSqlite.sprx` couldn't be dlopened (missing or moved in
    // this firmware). Library-filter features degrade but don't fail.
    return "Title-registration lookups aren't available on this PS5 firmware. The rest of the library view still works.";
  }
  if (/launch_service_unavailable|service_unavailable/i.test(raw)) {
    return "This action needs a Sony service that isn't exported on your firmware. Everything else still works.";
  }

  // ─── Sony launcher error codes (after 2.2.32 auto-retry) ──────────
  // launch_title surfaces sceLncUtilLaunchApp's return value as
  // `launch_sony_error_0xNNNNNNNN`. The codes below are the ones we
  // can map to actionable user copy. Anything we don't recognize
  // falls through to the generic "PS5 rejected the request" path.
  const lncMatch = raw.match(/launch_sony_error_0x([0-9a-fA-F]{8})/);
  if (lncMatch) {
    const code = lncMatch[1].toUpperCase();
    if (code === "8094000F") {
      return "PS5 has no profile selected. Pick a user profile on the PS5 home screen, then try Launch again.";
    }
    if (code === "8094000C" || code === "8094000D") {
      return "PS5 says the title isn't registered. Click Register first, or unregister + re-register if it was already added.";
    }
    if (code === "80940020" || code === "80940021") {
      return "PS5 launcher is busy with another title. Close any running game on the PS5 and try Launch again.";
    }
    if (code === "8094001F") {
      return "PS5 says this title's data is corrupted. The eboot.bin or sce_sys folder may be incomplete — re-upload the game.";
    }
    return `PS5 launcher returned 0x${code}. The title may have been removed, or the install isn't complete — try Re-register from the Library tab.`;
  }

  if (/launch_title_id_invalid/i.test(raw)) {
    return "Title ID doesn't look valid. Make sure the game's PARAM.SFO has a title_id like CUSA12345 or PPSA01234.";
  }

  // ─── Mount error mapping ────────────────────────────────────────
  // Payload errors from handle_fs_mount. These are user-fixable in
  // most cases (re-upload, pick a different image, wait for the
  // upload to settle).
  if (/fs_mount_image_not_a_file/i.test(raw)) {
    return "PS5 can't find that file at the destination. The upload may not have completed — wait a moment and retry.";
  }
  if (/fs_mount_unsupported_format/i.test(raw)) {
    return "PS5 doesn't recognize this file as a mountable disk image. Only .ffpkg (UFS), .exfat, and .ffpfs are supported.";
  }
  if (/fs_mount_source_unstable/i.test(raw)) {
    return "PS5 sees the file is still being written. Wait 5 seconds for the upload to finish, then click Mount again.";
  }
  if (/fs_mount_path_not_allowed/i.test(raw)) {
    return "PS5 doesn't allow mounts at that path. Use /data, /user, /mnt/ext*, /mnt/usb*, or /mnt/ps5upload.";
  }
  if (/fs_mount_attach_failed/i.test(raw)) {
    return "PS5 couldn't attach the image to a block device (LVD or md). Image may be corrupt — try re-uploading or rebuild it.";
  }
  if (/fs_mount_dev_node_missing/i.test(raw)) {
    return "PS5 attached the image but the device node didn't appear. Reboot the PS5 and re-load the payload, then try again.";
  }
  // The PS5 kernel's nmount enforces its own mount-point policy on
  // top of our path allowlist. The most common kernel-policy failure
  // is EPERM ("Operation not permitted") when the chosen fspath is
  // a path the kernel doesn't allow exfat/ufs mounts under — most
  // notably arbitrary subpaths of /mnt/usb*/ or /mnt/ext*/. The
  // backing image's location on disk doesn't matter; what matters
  // is the *mount point* we're attaching to. /mnt/ps5upload/<name>
  // and most /data/<...> paths work; USB/ext slots are firmware-
  // and sub-path-dependent. Surface this clearly so the user knows
  // to retry with a different mount point instead of debugging the
  // .exfat itself.
  if (
    /fs_mount_nmount_failed/i.test(raw) &&
    /Operation not permitted|EPERM|operation_not_permitted/i.test(raw)
  ) {
    return "PS5 kernel refused this mount point (Operation not permitted). The .exfat file's location doesn't matter here — try mounting under /data/homebrew/<name> or /mnt/ps5upload/<name>. Some USB/ext sub-paths are blocked by kernel policy on certain firmware.";
  }
  if (/fs_mount_nmount_failed/i.test(raw)) {
    // Non-EPERM nmount failure — surface the kernel's own errmsg
    // verbatim (the payload formats it as "fs_mount_nmount_failed:
    // <kernel-errmsg-or-strerror>"); strip our prefix for clarity.
    const tail = raw.match(/fs_mount_nmount_failed:\s*(.+?)(?:\)|$)/i);
    return `PS5 kernel rejected the mount: ${tail ? tail[1] : "unknown reason"}. Try a different mount point (e.g. under /data or /mnt/ps5upload) — the image itself is fine.`;
  }

  // ─── BGFT / Install Package error codes ─────────────────────────
  // err_code_message in ps5upload-core covers the user-facing copy for
  // the common 0x80990xxx codes; the queue surfaces these directly.
  // This block catches the rare cases where a code comes through raw
  // (e.g. from a nested error wrap).
  if (/0x80990088|already.installed/i.test(raw)) {
    return "This title is already installed. Uninstall it first if you want to re-install.";
  }
  if (/0x80990085|defrag/i.test(raw)) {
    return "Your PS5 needs defragmented free space. Settings → Storage → Free up space, then retry.";
  }

  // ─── Generic payload rejection — extract the reason verbatim ───────
  // "payload rejected FS_LIST_DIR(/foo): fs_list_dir_opendir_errno_13"
  // The `errno_N` tail is opaque — strip the frame-name prefix so the
  // surfaced copy is at least not alarming even for unrecognized codes.
  const m = raw.match(/payload rejected [A-Z_]+(?:\([^)]*\))?:\s*(.+)$/);
  if (m) return `PS5 rejected the request: ${m[1]}`;

  return raw;
}
