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
//
// The user-facing copy is localised: each branch returns `te("err_…")`,
// which looks the key up in the active locale (falling back to English,
// then to the key). The English source of truth for every `err_*` key
// lives in `i18n/locales/en.ts`; the other 17 locales mirror it.

import { t } from "../i18n";
import { useLangStore } from "../state/lang";
import { isNpxsContentId } from "./npxs";

/** Localised lookup for the humanised error copy below. This module is a
 *  plain function (not a React component), so it reads the active language
 *  from the store at call time and delegates to the shared `t()`. A missing
 *  translation degrades to the English string (then the key) rather than a
 *  blank, because `t()` walks active-locale → English → key. */
function te(key: string, vars?: Record<string, string | number>): string {
  return t(useLangStore.getState().lang, key, vars);
}

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
    return te("err_unmount_busy");
  }
  if (/fs_unmount_permission/i.test(raw)) {
    return te("err_unmount_permission");
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
    return te("err_npxs_mgmt_disconnect");
  }

  // ─── Mid-transfer network drop ─────────────────────────────────────
  // The payload closed the TCP connection before sending its
  // BEGIN_TX/COMMIT_TX ACK. Engine retries once or twice; if we get
  // here the retry budget was exhausted.
  if (/read frame header|unexpected ?eof|connection reset|broken pipe/i.test(raw)) {
    return te("err_network_drop");
  }

  // ─── PS5 unreachable at connect time ───────────────────────────────
  if (/connect to .+:9114/i.test(raw)) {
    return te("err_connect_mgmt");
  }
  if (/connect to .+:9113/i.test(raw)) {
    return te("err_connect_transfer");
  }

  // ─── RAR archive (.rar) inspect/extract ────────────────────────────
  // Tokens emitted by ps5upload-core's rar_support::map_rar_err and the
  // engine's Android stub. The two password tokens are the ones the Upload
  // screen reacts to (prompt / re-prompt); the rest get friendly copy here so
  // a raw "open rar: …" never reaches the user. Checked early because these
  // strings are specific and shouldn't be shadowed by the generic branches.
  if (/rar_password_required/i.test(raw)) {
    return te("err_rar_password_required");
  }
  if (/rar_password_wrong/i.test(raw)) {
    return te("err_rar_password_wrong");
  }
  if (/RAR is not supported on this build/i.test(raw)) {
    // Android (and any build without the desktop-only unrar dep). The UI
    // should feature-gate before calling, but if a request slips through the
    // engine returns 501 with this body.
    return te("err_rar_unsupported");
  }
  if (/rar contains an unsafe or invalid entry path/i.test(raw)) {
    return te("err_rar_unsafe_entry");
  }
  // Any other unrar failure surfaces as "open rar: …" / "read rar header: …" /
  // "extract rar entry: …". The dominant real-world cause is an incomplete
  // multi-volume set (a missing .partN, or the user picked a middle volume),
  // so lead with that actionable guidance rather than the opaque library text.
  if (/\b(open rar|read rar header|extract rar entry|skip rar entry)\b/i.test(raw)) {
    return te("err_rar_open_failed");
  }

  // ─── Multi-file manifest rejected at BEGIN_TX ──────────────────────
  // The payload surfaces `manifest_invalid` ("BeginTx rejected (Error):
  // manifest_invalid") when its manifest parser refuses the file list.
  // Two real-world causes: (1) a file/folder name contains a character
  // the parser mishandled — most notably '}', which older payloads
  // mistook for a JSON object terminator — or (2) a single destination
  // path is too long (the payload caps paths at 512 bytes). The engine
  // now pre-flights the length case with a clear, file-named error
  // ("destination path is too long…"), so a bare manifest_invalid that
  // reaches here is most likely the character case against a payload
  // from before the brace-safe parser fix.
  if (/manifest_invalid/i.test(raw)) {
    return te("err_manifest_invalid");
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
    return te("err_dest_write_refused");
  }
  if (/ENOSPC|no space left|disk.*full/i.test(raw)) {
    return te("err_dest_full");
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
    return te("err_volumes_unavailable");
  }
  if (/sqlite_unavailable/i.test(raw)) {
    // `libSceSqlite.sprx` couldn't be dlopened (missing or moved in
    // this firmware). Library-filter features degrade but don't fail.
    return te("err_sqlite_unavailable");
  }
  if (/launch_service_unavailable|service_unavailable/i.test(raw)) {
    return te("err_service_unavailable");
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
      return te("err_launch_no_profile");
    }
    if (code === "8094000C" || code === "8094000D") {
      return te("err_launch_not_registered");
    }
    if (code === "80940020" || code === "80940021") {
      return te("err_launch_busy");
    }
    if (code === "8094001F") {
      return te("err_launch_corrupt");
    }
    return te("err_launch_unknown", { code });
  }

  if (/launch_title_id_invalid/i.test(raw)) {
    return te("err_launch_title_id_invalid");
  }

  // ─── Mount error mapping ────────────────────────────────────────
  // Payload errors from handle_fs_mount. These are user-fixable in
  // most cases (re-upload, pick a different image, wait for the
  // upload to settle).
  if (/fs_mount_image_not_a_file/i.test(raw)) {
    return te("err_mount_not_a_file");
  }
  if (/fs_mount_unsupported_format/i.test(raw)) {
    return te("err_mount_unsupported_format");
  }
  if (/fs_mount_source_unstable/i.test(raw)) {
    return te("err_mount_source_unstable");
  }
  if (/fs_mount_path_not_allowed/i.test(raw)) {
    return te("err_mount_path_not_allowed");
  }
  if (/fs_mount_attach_failed/i.test(raw)) {
    return te("err_mount_attach_failed");
  }
  if (/fs_mount_dev_node_missing/i.test(raw)) {
    return te("err_mount_dev_node_missing");
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
    return te("err_mount_nmount_eperm");
  }
  if (/fs_mount_nmount_failed/i.test(raw)) {
    // Non-EPERM nmount failure — surface the kernel's own errmsg
    // verbatim (the payload formats it as "fs_mount_nmount_failed:
    // <kernel-errmsg-or-strerror>"); strip our prefix for clarity.
    const tail = raw.match(/fs_mount_nmount_failed:\s*(.+?)(?:\)|$)/i);
    return te("err_mount_nmount_other", {
      reason: tail ? tail[1] : te("err_unknown_reason"),
    });
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
    return te("err_appinst_not_initialized");
  }
  if (/0x80A2FF02|-2136801278|SCE_APP_INSTALLER_ERROR_NOSPACE/i.test(raw)) {
    return te("err_appinst_nospace");
  }
  if (/0x80A2FF06|-2136801274|SCE_APP_INSTALLER_ERROR_PKG_INVALID_DRM_TYPE/i.test(raw)) {
    return te("err_appinst_drm_type");
  }
  if (/0x80A2FF09|-2136801271|SCE_APP_INSTALLER_ERROR_PKG_INVALID_CONTENT_TYPE/i.test(raw)) {
    return te("err_appinst_content_type");
  }
  if (/0x80A2FF14|-2136801260|SCE_APP_INSTALLER_ERROR_BUSY/i.test(raw)) {
    return te("err_appinst_busy");
  }
  if (/0x80A2FF15|-2136801259|SCE_APP_INSTALLER_ERROR_DLAPP_ALREADY_INSTALLED/i.test(raw)) {
    return te("err_already_installed");
  }
  if (/0x80A30001|-2136862719|SCE_APP_INST_UTIL_ERROR_OUT_OF_MEMORY/i.test(raw)) {
    return te("err_appinst_oom");
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
    return te("err_install_eagain");
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
    return te("err_install_dup_register");
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
    return te("err_install_http_fetch");
  }
  // ─── 0x80B2_116F — installer rejected (FW 9.60 + NPXS path) ────────
  // Hardware-observed against an NPXS pkg on FW 9.60 when all 3 tiers
  // failed. The 3 tiers each surfaced this code AND the BGFT-direct
  // tier additionally reported "BGFT symbol missing" — that firmware
  // point doesn't export the BGFT registers our Tier-3 fallback uses.
  // For NPXS-prefix pkgs we route to the system-pkg-specific advice;
  // for game pkgs the FW-incompatibility advice is more useful. MUST
  // come before the generic 0x80B2 wildcard below, otherwise the
  // wildcard catches it first and this branch is dead code.
  if (/0x80B2_?116F/i.test(raw)) {
    if (isNpxsContentId(contentId)) {
      return te("err_install_116f_npxs");
    }
    return te("err_install_116f_game");
  }
  // ─── 0x80B2_1401 — ShellUI install path rejected ───────────────────
  // Tier-2 (shellui-rpc) returned this; usually paired with 0x80B2116F
  // on the same firmware point. Distinct surface so the user knows
  // it's the ShellUI path specifically that bailed.
  if (/0x80B2_?1401/i.test(raw)) {
    return te("err_install_1401");
  }
  // ─── 0x80B2_2101 — earlier download still queued ───────────────────
  // Must precede the generic 0x80B2_xxxx wildcard below, which would
  // otherwise shadow this specific, actionable case (0x80B2 + "2101").
  if (/0x80B22101/i.test(raw)) {
    return te("err_install_2101");
  }
  if (/\b0x80B2[0-9A-Fa-f]{4}\b/i.test(raw)) {
    // Other 0x80B2_xxxx — PlayGo errors, not pkg-format errors.
    // Don't blame the file.
    return te("err_install_80b2_generic");
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
    return te("err_bgft_not_loadable");
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
      return te("err_install_enoent_dlc", {
        contentId: contentId ?? "",
        baseTitle,
      });
    }
    return te("err_install_enoent_generic");
  }

  // ─── BGFT / Install Package error codes ─────────────────────────
  // err_code_message in ps5upload-core covers the user-facing copy for
  // the common 0x80990xxx codes; the queue surfaces these directly.
  // This block catches the rare cases where a code comes through raw
  // (e.g. from a nested error wrap).
  if (/0x80990088|already.installed/i.test(raw)) {
    return te("err_already_installed");
  }
  if (/0x80990085|defrag/i.test(raw)) {
    return te("err_install_defrag");
  }
  if (/0x80990086/i.test(raw)) {
    return te("err_install_leftover_download");
  }
  if (/0x80990036/i.test(raw)) {
    return te("err_install_drm_mismatch");
  }
  if (/0x80990038/i.test(raw)) {
    return te("err_install_entitlement");
  }
  if (/0x80990039|0x80A30026|out of (free )?space/i.test(raw)) {
    return te("err_install_no_free_space");
  }
  if (/0x80B64002/i.test(raw)) {
    return te("err_install_parental");
  }
  if (/0x80020005/i.test(raw)) {
    return te("err_install_esrch");
  }
  // 0x80B2116F, 0x80B21401, and 0x80B22101 handled above (each must
  // precede the generic 0x80B2 wildcard, which would otherwise shadow
  // them).

  // ─── Generic 0x80990xxx fallback — Sony's BGFT family ──────────────
  // We've mapped the codes seen most often; any remaining one in this
  // namespace at least gets a clear "this is the install service
  // saying no" framing instead of a raw hex dump.
  if (/\b0x80990[0-9A-Fa-f]{3}\b/i.test(raw)) {
    return te("err_install_bgft_generic", {
      code: raw.match(/0x80990[0-9A-Fa-f]{3}/i)?.[0] ?? "",
    });
  }

  // ─── Generic payload rejection — extract the reason verbatim ───────
  // "payload rejected FS_LIST_DIR(/foo): fs_list_dir_opendir_errno_13"
  // The `errno_N` tail is opaque — strip the frame-name prefix so the
  // surfaced copy is at least not alarming even for unrecognized codes.
  const m = raw.match(/payload rejected [A-Z_]+(?:\([^)]*\))?:\s*(.+)$/);
  if (m) return te("err_payload_rejected", { reason: m[1] });

  return raw;
}
