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
  if (/EACCES|permission[ _]?denied|\bfs_\w*_permission/i.test(raw)) {
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

  // ─── Generic payload rejection — extract the reason verbatim ───────
  // "payload rejected FS_LIST_DIR(/foo): fs_list_dir_opendir_errno_13"
  // The `errno_N` tail is opaque — strip the frame-name prefix so the
  // surfaced copy is at least not alarming even for unrecognized codes.
  const m = raw.match(/payload rejected [A-Z_]+(?:\([^)]*\))?:\s*(.+)$/);
  if (m) return `PS5 rejected the request: ${m[1]}`;

  return raw;
}
