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

import { isNpxsContentId } from "./npxs";

/** Rewrite a raw engine/payload error string into a single line of
 *  user-facing copy. Unknown strings are returned as-is.
 *
 *  `errCode` is an optional second source: many of the regex branches
 *  below look for a Sony hex code (`0x80B22404`, `0x80A2FF15`, …) but
 *  pre-fix-round-6 the humanizer only saw the err_message string. The
 *  PKG install path stores err_code in a separate field (rendered as
 *  a `<code>` block under the human-friendly text) — when err_message
 *  was just the BGFT-side `detail` (e.g. "BGFT symbol missing (tried:
 *  …)") and err_code was unmapped (e.g. 0x80B22404), the humanizer
 *  matched on `BGFT symbol missing` and surfaced the misleading "BGFT
 *  not loadable" message even though the actual rejection was Sony's
 *  format check. Passing err_code lets the regex find the hex form
 *  directly. Optional for back-compat with non-pkg callers (FS ops,
 *  transfers) that don't have a numeric err code. */
export function humanizePs5Error(
  raw: string,
  errCode?: number,
  /** 2.2.55: optional install-context for content-id-aware messages.
   *  When set, an NPXS-prefix system-pkg install that loses the
   *  mgmt connection mid-flight gets a clear "this is the system-
   *  pkg destabilisation symptom" message instead of the generic
   *  "send payload" hint — the payload IS loaded, Sony just froze
   *  the mgmt service while trying to install something the API
   *  isn't designed for. */
  contentId?: string,
): string {
  if (!raw && !errCode) return "";
  // Build a lookup string that includes both sources so existing
  // hex-pattern regexes match either way. Lowercase the hex digits
  // so case-insensitive regexes don't have to backtrack on every
  // call. Decimal form is also appended because some humanizer
  // branches match the negative-int form Sony's docs use.
  const codeForms =
    errCode && errCode > 0
      ? ` 0x${errCode.toString(16).padStart(8, "0")} ${(errCode | 0).toString()}`
      : "";
  raw = (raw ?? "") + codeForms;
  if (!raw) return "";

  // ─── Source / local filesystem ─────────────────────────────────────
  // "can't read source folder: /Users/me/game" — the engine failed to
  // walk the user's local tree. We return verbatim because the path
  // is already what they need to see.
  if (/can't read source folder/i.test(raw)) {
    return raw;
  }

  // ─── Unmount: kernel-busy (game running) ─────────────────────────────
  // Payload returns `fs_unmount_busy` when the kernel refused the
  // unmount with EBUSY — the most common cause is that a process on
  // the PS5 has files inside the mount open (i.e. the game is
  // running). Pre-2.2.59 the same case surfaced as a generic
  // "fs_unmount_failed" with no actionable hint. Surface what's
  // actually wrong and what the user should do.
  if (/fs_unmount_busy/i.test(raw)) {
    return "Can't unmount: the game inside this image is currently running on the PS5. Exit it (PS Home → close the game) and try again.";
  }
  if (/fs_unmount_permission/i.test(raw)) {
    return "Can't unmount: kernel refused with EACCES/EPERM. The payload may have lost root credentials — reload it from Connection → Send payload.";
  }

  // ─── NPXS system-pkg + mgmt disconnect mid-install ─────────────────
  // Sony accepts the register call (we see `register_path=shellui-rpc
  // accepted`) but `sceAppInstUtilInstallByPackage` isn't designed
  // for system app pkgs (Store updates, Settings, built-in apps).
  // The destabilisation surfaces as a mgmt connect/transport failure
  // some seconds AFTER register success — different root cause from
  // the generic "no payload loaded" branch below. Caller passes
  // contentId so we can route this case to the actually-helpful
  // message.
  if (
    isNpxsContentId(contentId) &&
    /connect to .+:911[34]|read frame header|unexpected ?eof|connection reset|broken pipe/i.test(
      raw,
    )
  ) {
    return "PS5 mgmt service stopped responding mid-install. This is the known NPXS-system-pkg failure mode: Sony accepts the register but `sceAppInstUtilInstallByPackage` isn't designed for system patches (Store updates, Settings, etc.). The PS5 typically recovers on its own in a minute or two, or after a reboot — but ps5upload can't install this pkg. Use Settings → Debug Settings → Game → Package Installer on the PS5 itself for system pkgs.";
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

  // ─── Sony AppInstUtil error codes ─────────────────────────────────
  // Range 0x80A2_FFXX / 0x80A3_00XX. SCE_APP_INST_UTIL_* names are
  // Sony's own symbol set (psdevwiki + libSceAppInstUtil exports).
  // This block catches the most common ones with actionable copy;
  // rarer ones fall through to the generic "PS5 rejected the
  // request" path.
  // Each Sony AppInstUtil code is matched against three string forms:
  //   - canonical hex (0xNNNNNNNN)
  //   - decimal signed (engine sometimes forwards int32 cast as
  //     decimal when err_code_message has no mapping)
  //   - the SCE_… symbolic name (in case someone wraps it in a
  //     friendlier message upstream)
  // Without the decimal forms the humanizer silently fell through
  // for any code the engine emitted in decimal, leaving the user
  // with the raw "PS5 rejected the request: -2136862720" copy.
  if (/0x80A30000|-2136862720|SCE_APP_INST_UTIL_ERROR_NOT_INITIALIZED/i.test(raw)) {
    return "Sony's installer subsystem isn't initialised yet — push the latest bundled payload (Connection → Send payload) so the lazy-init in 2.2.46+ runs. If the error persists, the install API isn't reachable from our process context on this firmware; FTP-upload + Library → Register is the workaround.";
  }
  if (/0x80A2FF02|-2136801278|SCE_APP_INSTALLER_ERROR_NOSPACE/i.test(raw)) {
    return "Your PS5 doesn't have enough free space for this install. Settings → Storage → Free up space, then retry.";
  }
  if (/0x80A2FF06|-2136801274|SCE_APP_INSTALLER_ERROR_PKG_INVALID_DRM_TYPE/i.test(raw)) {
    return "Sony's installer rejected this PKG's DRM type. Try the Library → Register flow with 'Patch DRM' instead — it rewrites applicationDrmType to 'standard' before installing.";
  }
  if (/0x80A2FF09|-2136801271|SCE_APP_INSTALLER_ERROR_PKG_INVALID_CONTENT_TYPE/i.test(raw)) {
    return "Sony's installer doesn't accept this PKG's content type on the current firmware (e.g. some patch-pkgs / DLC formats). The base game's PKG should still install if you have it.";
  }
  if (/0x80A2FF14|-2136801260|SCE_APP_INSTALLER_ERROR_BUSY/i.test(raw)) {
    return "Sony's installer is busy with another install or an unfinished BGFT task. Wait a moment, or check the PS5's Notifications for a stuck download to clear, then retry.";
  }
  if (/0x80A2FF15|-2136801259|SCE_APP_INSTALLER_ERROR_DLAPP_ALREADY_INSTALLED/i.test(raw)) {
    return "This title is already installed. Uninstall it first if you want to re-install.";
  }
  if (/0x80A30001|-2136862719|SCE_APP_INST_UTIL_ERROR_OUT_OF_MEMORY/i.test(raw)) {
    return "Sony's installer ran out of memory mid-install. Reboot the PS5, reload the payload, and retry.";
  }

  // ─── 0x80B2_xxxx — Sony installer / package-format rejection ────────
  // 0x80B2_2404 in particular has been observed on PS5-native fakepkgs
  // submitted via PS4-emu install paths and on pkgs whose header magic
  // isn't `\x7FCNT` (e.g. the non-canonical `\x7FFIH` produced by some
  // PS5-native fakepkg signing tools). The rejection happens AFTER
  // Sony fetched the bytes, so the HTTP-host plumbing is fine — the
  // installer just can't parse what we handed it. Common causes:
  //
  //   - Pkg has a non-canonical header magic (run our /api/pkg/parse
  //     output past `magic_hex` to verify); Sony's installer expects
  //     `\x7FCNT` for PS4-format and rejects others.
  //   - DLC pkg without the base game installed first. Sony refuses
  //     to install DLC against a missing base.
  //   - PS5-native pkg (PS5GD/PS5DP) submitted with package_type
  //     "PS4GD" — type mismatch makes the installer fail header check.
  //
  // Without Sony docs for the 0x80B2 namespace we surface the most
  // likely actionable cause rather than a generic "unknown".
  if (/\b0x80020023\b/i.test(raw)) {
    // SCE_KERNEL_ERROR_EAGAIN — errno 35. Sony's installer says "busy /
    // try again later". Most common cause on FW 9.60: a previous
    // install for the same content_id is still queued in Sony's
    // installer state and blocking the new one.
    return "Sony's installer is busy — error 0x80020023 (EAGAIN). A previous install with the same content_id is still queued on the PS5. Open Settings → Notifications on the PS5 and dismiss any pending Store-related entries, OR reboot the PS5 to clear stale install state. Then retry.";
  }
  if (/\b0x80b21106\b/i.test(raw)) {
    // 0x80B21106 from Sony's AppInstaller. Direct testing across
    // many install attempts proved this isn't a Sony-stuck-state
    // symptom — it's "Sony already has a task queued for this
    // content_id and won't accept a duplicate register". The
    // first install attempt for a given content_id succeeds; the
    // second one for the SAME content_id (within the same PS5
    // session) gets 0x80B21106 because Sony's installer is busy
    // running the first.
    //
    // The previous install probably IS running (or queued) on the
    // PS5 right now. The desktop's UI shows phase=install
    // synthetically forever for shellui-rpc-backed tasks; the
    // actual install runs in Sony's background and surfaces in
    // the PS5's own Settings → Notifications → Downloads panel.
    return "Sony's installer rejected this register with 0x80B21106 — most likely because the previous install for the same content_id is still queued/running on the PS5. Check Settings → Notifications → Downloads on the PS5 to see if it's already installing. If you really want to re-register (e.g. the previous attempt failed silently), reboot the PS5 first. DO NOT click Start repeatedly — each retry just confirms Sony's response.";
  }
  if (
    /0x80B2_?2404\b/i.test(raw) ||
    /-2135858684/i.test(raw) ||
    /\b0x80b22404\b/i.test(raw)
  ) {
    // 0x80B22404 = SCE_PLAYGO_ERROR_CORE_HTTP_STATUS_CODE_404_NOT_FOUND.
    // This is NOT about the pkg — Sony's installer never looked at
    // the file. PlayGo's URL pre-flight rejected the fetch before
    // touching any bytes. The cause is our process context: Sony
    // whitelists ShellUI's authid for install-side HTTP fetch; calls
    // from our payload's own context get this error regardless of
    // pkg validity, cred-forge, or URL contents. The pkg is almost
    // certainly fine — same file installs via Settings → Debug
    // Settings → Install Package on the PS5 itself, because that
    // path runs from ShellUI.
    return "PS5 rejected our HTTP fetch attempt for the install (0x80B22404). This isn't about the pkg's format — Sony's installer didn't read any of the file's bytes. It's a process-context issue: Sony's PlayGo whitelists ShellUI's process for install-side HTTP fetch and rejects ours. The 2.2.52 build has a new ShellUI-RPC install path that routes through ShellUI's process so the same fetch succeeds. If you're still seeing this error, the running payload is the old one — push the latest payload via Connection → Send payload, restart the install, and the diag panel should show register_path=shellui-rpc.";
  }
  if (/\b0x80B2[0-9A-Fa-f]{4}\b/i.test(raw)) {
    // Other 0x80B2_xxxx — PlayGo errors, not pkg-format errors.
    // Don't blame the file.
    return "PS5's PlayGo subsystem rejected the install with a 0x80B2_xxxx error. This is the install fetch path, not the pkg parser — your file likely is fine. Try pushing the latest payload (Connection → Send payload); the new ShellUI-RPC install path bypasses the most common 0x80B2 reject class.";
  }

  // ─── BGFT init failure (0xE0000001 = BGFT_ERR_LIB_NOT_LOADABLE) ──
  // The payload couldn't dlopen libSceBgft.sprx or couldn't resolve
  // one of its export symbols on the user's firmware. The 2.2.43
  // payload tries multiple library paths and symbol-name variants
  // before giving up; reaching this means none of them worked. The
  // user's options are limited: either upload the bundled payload
  // again (Connection → Send payload — newer payload may know more
  // variants) or fall back to non-BGFT install (FTP + manual
  // register, or the Library's Mount → Register flow for image
  // games).
  if (
    /BGFT symbol missing/i.test(raw) ||
    /dlopen libSceBgft\.sprx failed/i.test(raw) ||
    /0xE0000001|BGFT_ERR_LIB_NOT_LOADABLE/i.test(raw)
  ) {
    return "Your PS5 firmware doesn't expose Sony's BGFT installer in a way ps5upload can use. Push the latest bundled payload (Connection → Send payload) — it tries more library paths and symbol variants. If it still fails, install via FTP + Library → Register instead; .pkg-via-BGFT isn't available on this firmware.";
  }

  // ─── 0x80020002 — sceKernelOpen() ENOENT from Sony's installer ──
  // Sony encodes Unix errno values as 0x80020000 + errno. 0x80020002
  // is ENOENT ("No such file or directory") from the kernel-side
  // sceKernelOpen — meaning Sony's installer COULDN'T OPEN something
  // when it tried to resolve the pkg.
  //
  // The most common cause (verified via klog on 2026-05-11) is a DLC
  // pkg whose base game isn't installed:
  //
  //   - DLC content_ids have shape <store>-<base>_<patch>-<dlc>
  //     e.g. EP7579-PPSA17599_00-EXP33DLC10000PS5  (PPSA17599 is base)
  //   - When Sony's installer opens the pkg, it follows references
  //     into the base game's app_home / sce_sys dir under /rnps/.
  //     If the base game isn't installed, that path is missing →
  //     sceKernelOpen returns ENOENT → 0x80020002 surfaces.
  //
  // For NON-DLC content_ids the same error means the file was deleted
  // between staging and install (race condition we've fixed) OR the
  // engine staged to a path Sony's installer doesn't accept. Surface
  // both as options.
  if (
    /\b0x80020002\b/i.test(raw) ||
    /sceKernelOpen.*0x80020002/i.test(raw) ||
    /SCE_KERNEL_ERROR_ENOENT/i.test(raw)
  ) {
    // Sniff the content_id from the error context to give a sharper
    // message. DLC pattern: "<2 letters><4 digits>-<base>_<patch>-<dlc>"
    // where the third dash is the DLC marker.
    const dlcMatch = (contentId ?? "").match(
      /^([A-Z]{2}\d{4})-([A-Z0-9]+)_\d+-([A-Z0-9_]+)$/i,
    );
    if (dlcMatch) {
      const baseTitle = dlcMatch[2];
      return `This looks like a DLC pkg (content_id ${contentId}). Sony's installer needs the base game (${baseTitle}) to be installed BEFORE the DLC, because the install reads metadata from the base game's app_home. Install ${baseTitle} first, then retry this DLC. The 0x80020002 is the kernel reporting "no such file" when it tried to follow the base-game reference — not a problem with your DLC pkg.`;
    }
    return "Sony's installer couldn't open a file it needed during install (kernel error 0x80020002 = ENOENT). If this is a DLC pkg, the base game isn't installed yet — install the base first. Otherwise the staging file may have been deleted between upload and install; retry the install once. If it keeps failing, FTP-upload the pkg to /user/data/ps5upload/pkg_temp/ manually and use Library → Register to install.";
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
