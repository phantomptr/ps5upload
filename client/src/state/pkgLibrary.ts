import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { invoke } from "@tauri-apps/api/core";

import { fsListDir, fsDelete, fsMkdir, fsCopy } from "../api/ps5";
import type { ExternalPkg } from "../api/ps5";
import { hostOf, mgmtAddr, transferAddr } from "../lib/addr";
import { removableMountRoot } from "../lib/mountPaths";
import { humanizePs5Error } from "../lib/humanizeError";
import {
  stagingBasename,
  stagingSubdirForCategory,
  categoryForSubdir,
} from "../lib/pkgStagingPath";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { useInstallSettingsStore } from "./installSettings";
import { useConnectionStore } from "./connection";
import { log } from "./logs";
import { parsePS5Firmware } from "../lib/ps5Firmware";

/**
 * Package Library — the model behind the redesigned Install Package screen.
 *
 * Unlike the old transient install queue (which uploaded a `.pkg`, installed
 * it, then deleted it), the library treats the PS5 as durable storage: a
 * `.pkg` you add is uploaded ONCE into `/user/data/ps5upload/pkg_library/`
 * and stays there until you delete it, so you can (re)install it any time
 * without re-uploading. The source of truth is the on-PS5 directory listing,
 * not local state — so the list survives app restarts.
 *
 * Install PRIMARY path (since 2.25.2): the main payload's
 * `sceAppInstUtilInstallByPackage` run in the full jailbreak context, which
 * installs LAUNCHABLE content. If that's rejected we fall back to the DPI
 * daemon on :9040 (also InstallByPackage since 2.25.2), then re-send the main
 * payload (the single-payload loader swapped it out). The payload's own tier
 * ladder (bgft.c) only drops to the unlaunchable `sceAppInstUtilAppInstallPkg`
 * as an absolute last resort, flagging it so we warn the user — see
 * `pkgInstallMayNotLaunch`.
 *
 * IMPORTANT: the library dir is deliberately NOT `pkg_temp/` — the payload
 * sweeps that on boot (`runtime_sweep_stale_pkg_temp`), which would wipe the
 * user's library. `pkg_library/` is left untouched.
 */
export const PKG_LIBRARY_DIR = "/user/data/ps5upload/pkg_library";

/** Transient staging dir for install-from-USB: we copy a USB pkg here, install
 *  it, then delete the copy. The payload sweeps this on boot, so a leftover
 *  copy (e.g. after a crash) self-cleans. */
export const PKG_TEMP_DIR = "/user/data/ps5upload/pkg_temp";

/** Warning copy shown when an install lands via the unlaunchable last-resort
 *  path (register_path "appinst-local"); see `pkgInstallMayNotLaunch`. */
export const PKG_MAY_NOT_LAUNCH_MESSAGE =
  "Installed, but via a fallback that may not launch on this firmware. If the game won't start (“can't start the game or app”), re-install it from the PS5: Settings → System → Debug Settings → Game → Package Installer.";

/** Whether an install response indicates the title may not launch.
 *
 *  Precedence:
 *   1. `launchable` — the engine's definitive app.db verification result
 *      (elf-arsenal `wait_for_install_row` analogue). `true` means the
 *      title_id was confirmed registered in the PS5's app.db, so the title
 *      IS launchable even if it landed via the `appinst-local` last-resort
 *      path → returns false (clean success). `false` means the title never
 *      registered within the verification window → returns true.
 *   2. When `launchable` is null/absent (verification not applicable —
 *      sqlite unavailable on this firmware, no real title_id, or an older
 *      engine/client), fall back to the heuristic: the engine's
 *      `may_not_launch` flag, or the unlaunchable `appinst-local`
 *      register_path. Every launchable tier (appinst / shellui-rpc /
 *      intdebug / regular) → false. */
export function pkgInstallMayNotLaunch(r: {
  register_path?: string;
  may_not_launch?: boolean;
  launchable?: boolean | null;
}): boolean {
  if (r.launchable === true) return false;
  if (r.launchable === false) return true;
  return r.may_not_launch ?? r.register_path === "appinst-local";
}

/** The lastResult for a SUCCESSFUL primary install — amber warn when the title
 *  may not launch, plain green success otherwise. */
export function installedLastResult(mayNotLaunch: boolean): {
  ok: true;
  message: string;
  warn?: boolean;
} {
  return mayNotLaunch
    ? { ok: true, warn: true, message: PKG_MAY_NOT_LAUNCH_MESSAGE }
    : { ok: true, message: "Installed." };
}

export type PkgStatus = "idle" | "queued" | "uploading" | "installing";

export interface PkgEntry {
  /** Filename on the PS5, e.g. `CUSA00207.pkg`. Unique within the dir. */
  name: string;
  /** Absolute PS5 path (`PKG_LIBRARY_DIR/name`). The install/delete key. */
  path: string;
  /** Size in bytes — from the dir listing, or the local file while uploading. */
  size: number;
  /** ContentID (from the filename, which we name `<ContentID>.pkg`, or from
   *  parsed metadata while uploading). May be empty for oddly-named files. */
  contentId: string;
  /** Friendly title, when known (parsed at upload, cached in localStorage). */
  title?: string;
  /** Title id (PPSA/CUSA…) derived from the ContentID. Drives cover art and
   *  the "installed" badge. Undefined when it can't be derived. */
  titleId?: string;
  /** PARAM.SFO category — `gd` (base game), `gp` (update/patch), `ac` (DLC).
   *  A base and its update share a ContentID, so this is the only thing that
   *  tells them apart. On upload it comes from the parsed header; on refresh
   *  it's inferred from the staging sub-directory the file lives in. Undefined
   *  for root-level files of unknown category. Drives the Update/DLC badge. */
  category?: string;
  /** Target platform — "ps4" | "ps5" | "" (unknown). From the parsed header
   *  on upload; inferred from the title-id prefix on refresh-from-disk.
   *  Drives the PS4/PS5 badge. */
  platform?: string;
  /** Transient per-row state (never persisted; recomputed each session). */
  status: PkgStatus;
  /** Bytes transferred so far (upload) — drives the row progress bar. */
  bytes?: number;
  /** Total bytes for the active upload. */
  totalBytes?: number;
  /** Outcome of the last install attempt this session, for inline feedback.
   *  `warn` marks a soft-success: the install was accepted but via the
   *  unlaunchable last-resort path (`register_path === "appinst-local"`), so
   *  the title may not start on some firmwares (notably FW 12.xx). Rendered
   *  amber rather than green so the user knows to re-install via the PS5's
   *  Settings → Package Installer if it won't boot. */
  lastResult?: { ok: boolean; message: string; warn?: boolean };
}

/** PS5 title ids look like `CUSA12345` / `PPSA01234` — four letters then five
 *  digits. A ContentID embeds one as its second dash-segment, e.g.
 *  `EP9000-CUSA00207_00-BLOODBORNE000000`. Returns it, or null. Exported for
 *  unit testing. */
export function titleIdFromContentId(contentId: string): string | null {
  if (!contentId) return null;
  const seg = contentId.split("-")[1]; // "CUSA00207_00"
  const id = seg?.split("_")[0] ?? "";
  return /^[A-Z]{4}\d{5}$/.test(id) ? id : null;
}

/** Target platform ("ps4" | "ps5" | "") from a title-id prefix — CUSA = PS4,
 *  PPSA = PS5. Mirrors the engine's `derive_platform` fallback for the
 *  refresh-from-disk path, where we list staged files without re-parsing the
 *  header. Exported for unit testing. */
export function platformFromTitleId(titleId: string | null | undefined): string {
  if (!titleId) return "";
  if (titleId.startsWith("CUSA")) return "ps4";
  if (titleId.startsWith("PPSA")) return "ps5";
  return "";
}

/** ContentID from a `<ContentID>.pkg` filename (strip the extension). */
function contentIdFromName(name: string): string {
  return name.toLowerCase().endsWith(".pkg") ? name.slice(0, -4) : name;
}

// ── Title metadata cache ──────────────────────────────────────────────
// Filenames are `<ContentID>.pkg`, so the list always has the ContentID, but
// not a friendly title. We capture the parsed title at upload time and cache
// it (contentId → title) so rows show real names. A cache miss (uploaded on
// another machine, or cleared storage) just falls back to the ContentID.
const TITLE_CACHE_KEY = "ps5upload.pkg_library.titles.v1";

function loadTitleCache(): Record<string, string> {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(TITLE_CACHE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function cacheTitle(contentId: string, title: string): void {
  if (!contentId || !title) return;
  try {
    const c = loadTitleCache();
    c[contentId] = title;
    window.localStorage.setItem(TITLE_CACHE_KEY, JSON.stringify(c));
  } catch {
    /* best-effort */
  }
}

interface SplitParseResponse {
  parts?: string[];
  total_size?: number;
  head?: {
    content_id?: string;
    title?: string;
    /** PARAM.SFO CATEGORY ("gd"/"gp"/"ac"). Already present in the
     *  engine's parse-split response (it serialises the full PkgMetadata);
     *  we just hadn't been reading it. */
    category?: string;
    /** Target platform for badging: "ps4" | "ps5" | "" (unknown). Derived
     *  engine-side from the header magic + title-id prefix. */
    platform?: string;
    warnings?: string[];
  };
}

function pkgError(e: unknown): string {
  return humanizePs5Error(e instanceof Error ? e.message : String(e));
}

interface PkgLibraryState {
  /** Library contents, derived from the on-PS5 dir + transient row state. */
  entries: PkgEntry[];
  loading: boolean;
  error: string | null;
  /** True while an install is mid-flight (including the time it spends queued
   *  behind an active upload). Installs swap the payload in/out, so the UI
   *  blocks refresh/other installs until it settles. */
  installing: boolean;
  /** Human-readable "what's happening" line shown while an install or .pkg
   *  upload is QUEUED behind an active transfer (the PS5 can only do one at a
   *  time). Null when nothing is waiting. */
  busyNotice: string | null;
  /** True ONLY while an install is still WAITING its turn behind an active
   *  transfer (the cancellable window). It is cleared the instant the real
   *  install begins — unlike `busyNotice`, which on FW 12.x stays set during
   *  the actual install to show the "screen may go black" notice. Cancel must
   *  key off this, never `busyNotice`, or it could abort a real install
   *  mid-swap and leave a half-loaded payload. */
  installPending: boolean;

  refresh: (host: string) => Promise<void>;
  addAndUpload: (localPath: string, host: string) => Promise<void>;
  install: (path: string, host: string) => Promise<void>;
  /** Abandon an install that's still waiting its turn behind an upload. */
  cancelPendingInstall: () => void;
  remove: (path: string, host: string) => Promise<void>;
  /** Install a `.pkg` discovered on an external/USB drive. Sony's installer
   *  can't read the exfat USB mount directly (hardware-confirmed: it accepts
   *  the request but installs nothing), so this copies the file to internal
   *  staging on-console (fast, no upload), installs via the normal cascade,
   *  then deletes the copy. Shares the `installing` lock with `install()`.
   *  Returns `{ ok, message?, mayNotLaunch? }`. */
  installExternal: (
    pkg: ExternalPkg,
    host: string,
  ) => Promise<{ ok: boolean; message?: string; mayNotLaunch?: boolean }>;
  /** Install a `.pkg` the user found while browsing the File System, at an
   *  arbitrary on-console path. Removable mounts (`/mnt/usb*`, `/mnt/ext*`)
   *  route through the copy-to-internal `installExternal` flow (Sony can't
   *  read exfat directly); anything already on internal storage installs in
   *  place with no copy. Lets users install straight from where a package
   *  sits instead of going through the External Packages scan. */
  installFromConsolePath: (
    path: string,
    host: string,
  ) => Promise<{ ok: boolean; message?: string; mayNotLaunch?: boolean }>;
  /** Delete (from the PS5) every staged package that has already been
   *  installed successfully — clears the spent-package clutter without
   *  touching anything mid-flight or not-yet-installed. */
  clearFinished: (host: string) => Promise<void>;
  /** Delete (from the PS5) every idle staged package — a full wipe of the
   *  staging library. Skips rows that are uploading/installing/queued. */
  clearAll: (host: string) => Promise<void>;
}

/** A library row is "finished" when it's settled (idle) AND its last
 *  install succeeded — i.e. a spent staged package safe to delete. */
export function isFinishedPkg(e: PkgEntry): boolean {
  return e.status === "idle" && e.lastResult?.ok === true;
}

/** Merge a freshly-listed set with the current rows, preserving transient
 *  status (a row mid-upload/install must not be clobbered by a refresh). */
function mergeListing(prev: PkgEntry[], listed: PkgEntry[]): PkgEntry[] {
  const byPath = new Map(prev.map((e) => [e.path, e]));
  // Keep active (uploading) rows that aren't on disk yet.
  const active = prev.filter(
    (e) => e.status !== "idle" && !listed.some((l) => l.path === e.path),
  );
  const merged = listed.map((l) => {
    const old = byPath.get(l.path);
    if (old && old.status !== "idle") {
      // Preserve in-flight state but adopt the real on-disk size.
      return { ...old, size: l.size || old.size };
    }
    // Carry forward last install result for an idle row.
    return old?.lastResult ? { ...l, lastResult: old.lastResult } : l;
  });
  // `||` (not `??`) so an empty-string contentId (headerless pkg) falls
  // through to the filename instead of sorting as "".
  const label = (e: PkgEntry) => (e.title || e.contentId || e.name).toLowerCase();
  return [...active, ...merged].sort((a, b) => label(a).localeCompare(label(b)));
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export interface PkgInstallOutcome {
  /** The install COMPLETED — Sony's installer accepted it AND the engine's
   *  progress tracker confirmed the title actually landed (or DPI ok). This is
   *  the ONLY state in which the staged pkg may be deleted. A stall or an async
   *  failure leaves this false so the pkg is KEPT. */
  installed: boolean;
  /** Installed only via the unlaunchable last-resort path (may not start on
   *  some firmware, notably FW 12.xx). Surface a caution, not a clean OK. */
  mayNotLaunch: boolean;
  /** First error seen (empty when installed cleanly). */
  errMessage: string;
  /** The install STALLED — accepted, but disk progress flatlined before the
   *  title registered (e.g. a wedged large install). The staged pkg is KEPT so
   *  the user can retry; the UI shows a "stalled — package kept" message rather
   *  than a generic failure. `installed` is false. */
  stalled?: boolean;
}

/**
 * How often to poll `pkg_install_status` while tracking an install to
 * completion. The engine, not a client-side timer, decides when the install is
 * terminal: it polls the on-disk launch check + bytes-landing and reports
 * `done` (confirmed), `error`/`stalled` (terminal failure, pkg kept), or
 * `install` (still progressing — keep polling). So there is NO fixed client
 * deadline that could declare a large install "done" prematurely (the old
 * 100s-then-optimistic window is exactly what deleted the 25 GB Bloodborne pkg
 * mid-install). We poll until the engine says terminal, scaling to any size.
 */
const PKG_VERIFY_POLL_MS = 2_500;
/** Give up tracking after this many consecutive poll errors (e.g. the engine
 *  GC'd the session, or a sustained socket failure). On give-up we do NOT
 *  assume success — `completed` stays false so the pkg is KEPT (never delete on
 *  uncertainty). */
const PKG_VERIFY_MAX_POLL_ERRORS = 5;
/** Absolute backstop so a pathological engine that keeps returning "install"
 *  forever can't block the queue indefinitely. Generously past the engine's
 *  own 2h session GC — under normal operation the engine reaches a terminal
 *  verdict (done/stalled) long before this. On hitting it we KEEP the pkg. */
const PKG_VERIFY_SAFETY_CAP_MS = 3 * 60 * 60 * 1000;

/** Re-install guidance appended when Sony fails the async install — mirrors the
 *  may-not-launch copy so the user has a concrete next step. */
const PKG_ASYNC_FAILED_HINT =
  'the PS5 reported the install didn’t complete (the tile may show "Can’t start the game or app. The data is corrupted."). Re-install from the PS5: Settings → System → Debug Settings → Game → Package Installer.';

/** Stall guidance — the install accepted but disk progress flatlined. The
 *  staged pkg was KEPT, so the user can simply retry. */
const PKG_STALL_HINT =
  "the install stopped making progress on the PS5 before it finished. The package was kept on the PS5 — try installing it again (and check the console has enough free space).";

/** What the install tracker concludes. */
interface VerifyOutcome {
  /** Confirmed complete — the title registered (or byte-settled on
   *  unverifiable FW). The ONLY state that permits deleting the staged pkg. */
  completed: boolean;
  /** Terminal failure or stall — `completed` is false and the pkg is KEPT. */
  failed: boolean;
  /** Specifically a stall (flatlined), as opposed to a Sony-reported error. */
  stalled: boolean;
  message: string;
  launchable?: boolean | null;
}

/**
 * Track an install to a genuine terminal state by polling `pkg_install_status`.
 *
 * Unlike the old fixed-window verify, this NEVER assumes success on a timeout —
 * the engine's progress tracker observes the install (on-disk launch check +
 * bytes landing) and tells us `done` (confirmed), `error`/`stalled` (terminal,
 * pkg kept), or keeps reporting `install` while bytes are still landing. We
 * poll until terminal, so a 25 GB or 200 GB install is tracked to actual
 * completion instead of being declared done after 100s and deleted mid-write.
 *
 * `onProgress(installedBytes, total)` is called each poll so callers can render
 * a live install % for large titles.
 */
async function verifyInstallCompleted(
  session: string | undefined,
  onProgress?: (installedBytes: number, total: number) => void,
): Promise<VerifyOutcome> {
  // No session id ⇒ older engine (or a test harness) that can't report status.
  // Can't verify — preserve the pre-fix optimistic behavior (treat as done).
  if (!session)
    return { completed: true, failed: false, stalled: false, message: "" };
  const safetyDeadline = Date.now() + PKG_VERIFY_SAFETY_CAP_MS;
  let pollErrors = 0;
  while (Date.now() < safetyDeadline) {
    await sleep(PKG_VERIFY_POLL_MS);
    try {
      const s = (await invoke("pkg_install_status", { session })) as {
        phase?: string;
        err_code?: number;
        err_message?: string;
        launchable?: boolean | null;
        // Progress-tracker fields (engine ≥ this release). Absent on older
        // engines (serde default) — then we just don't render a live %.
        installed_bytes?: number;
        total?: number;
        stalled?: boolean;
      };
      // An engine that doesn't speak status returns no `phase` — treat as
      // "can't verify" and resolve done (stay optimistic), no infinite spin.
      if (typeof s?.phase !== "string")
        return { completed: true, failed: false, stalled: false, message: "" };
      if (typeof s.installed_bytes === "number" && typeof s.total === "number") {
        onProgress?.(s.installed_bytes, s.total);
      }
      if (s.phase === "error") {
        // Distinguish a stall (flatlined, pkg kept, retry) from a Sony-reported
        // async failure — both are terminal and both KEEP the pkg.
        if (s.stalled) {
          return {
            completed: false,
            failed: true,
            stalled: true,
            message: PKG_STALL_HINT,
            launchable: s.launchable,
          };
        }
        const code = (s.err_code ?? 0) >>> 0;
        const sony =
          s.err_message ||
          (code ? `0x${code.toString(16).padStart(8, "0")}` : "");
        return {
          completed: false,
          failed: true,
          stalled: false,
          message: sony
            ? `${sony} — ${PKG_ASYNC_FAILED_HINT}`
            : PKG_ASYNC_FAILED_HINT,
          launchable: s.launchable,
        };
      }
      if (s.phase === "done") {
        // Confirmed terminal success — the engine only reports `done` once the
        // title actually registered (or byte-settled). Safe to delete.
        return {
          completed: true,
          failed: false,
          stalled: false,
          message: "",
          launchable: s.launchable,
        };
      }
      pollErrors = 0; // a clean in-progress poll resets the error streak
    } catch {
      // Session GC'd or a sustained blip: after a few, give up tracking — but
      // do NOT assume success. `completed:false` keeps the pkg (never delete on
      // uncertainty); the install may well have finished, so don't shout error.
      if (++pollErrors >= PKG_VERIFY_MAX_POLL_ERRORS) {
        return {
          completed: false,
          failed: false,
          stalled: false,
          message: "",
        };
      }
    }
  }
  // Safety cap hit (pathological): keep the pkg, don't claim success.
  return { completed: false, failed: false, stalled: false, message: "" };
}

/**
 * The bare `.pkg` install mechanism, with NO store/UI side effects — shared by
 * the Install Package screen's manual install (`install()`) and the upload
 * queue's pkg finisher (`uploadQueue.runOne`). The `.pkg` must already be
 * staged on the PS5 at `localPs5Path`.
 *
 * Cascade (HW-proven): the MAIN PAYLOAD's InstallByPackage first — it runs in
 * the full jailbreak context, which is what actually installs LAUNCHABLE
 * content into /user/app/<title>. Only if the firmware rejects that do we fall
 * back to the standalone DPI daemon on :9040 (registers metadata, may not be
 * launchable on newer firmware), then restore the main payload. Returns whether
 * it installed, whether it's the unlaunchable path, and the first error.
 *
 * Throws only if the DPI daemon itself can't come up after a main-payload
 * reject (a genuine dead-end) — callers should catch and treat as failure.
 */
export async function runPkgInstall(
  host: string,
  localPs5Path: string,
  contentId: string | null,
  // The user's "Auto Delete after installation" preference. When false, the
  // engine KEEPS the staged pkg after install instead of deleting it. This is
  // the single source of truth for staging deletion now — previously the engine
  // always deleted, ignoring the setting (the reported data-loss bug).
  deleteStaging: boolean,
  // Called each status poll with the install's live byte progress, so the UI
  // can render a real % for large titles (Sony's BGFT progress isn't
  // meaningful on the file:// staging path). Optional — no-op if omitted.
  onProgress?: (installedBytes: number, total: number) => void,
): Promise<PkgInstallOutcome> {
  const ip = hostOf(host);
  let installed = false;
  // True when the install accepted but stalled (flatlined) before completing —
  // the pkg is KEPT and the caller shows a retry message, not a hard failure.
  let stalled = false;
  // Whether the payload *rejected* the install at register time (rc != 0 or an
  // RPC error). Only a genuine start rejection should trigger the DPI fallback
  // — an install that was ACCEPTED but then failed Sony's async install must
  // NOT fall back to DPI, since DPI's local-path install is the metadata-only
  // path that produces the very "data is corrupted" tile we're guarding against
  // on FW 12.xx.
  let startRejected = false;
  let mainErr = "";
  let mayNotLaunch = false;
  try {
    const r = (await invoke("pkg_install_start", {
      ps5Addr: mgmtAddr(host),
      path: null,
      splitRoot: null,
      packageTypeOverride: null,
      localPs5Path,
      contentId: contentId || null,
      deleteStaging,
    })) as {
      err_code?: number;
      register_path?: string;
      err_message?: string;
      may_not_launch?: boolean;
      session_id?: string;
    };
    const rc = (r.err_code ?? 0) >>> 0;
    if (rc === 0) {
      // Accept != complete. TRACK the async install to a genuine terminal state
      // before reporting success — the engine observes it to completion (no
      // size-blind timer), so a large install isn't declared done early and its
      // staged pkg deleted mid-write (the Bloodborne data-loss).
      const verdict = await verifyInstallCompleted(r.session_id, onProgress);
      if (verdict.completed) {
        installed = true;
        // Prefer the engine's definitive app.db verdict captured during
        // verification; fall back to the register_path heuristic when the
        // status poll didn't carry one (older engine, or app.db unreadable).
        mayNotLaunch = pkgInstallMayNotLaunch({
          ...r,
          launchable: verdict.launchable,
        });
      } else {
        // Stall, async failure, or unconfirmed. `installed` stays false → the
        // pkg is KEPT (never deleted on a non-confirmed install). startRejected
        // stays false → NO DPI fallback (the task was accepted; DPI's
        // metadata-only install would just produce the corrupted tile).
        mainErr = verdict.message;
        stalled = verdict.stalled;
      }
    } else {
      startRejected = true;
      mainErr = r.err_message || `0x${rc.toString(16).padStart(8, "0")}`;
    }
  } catch (e) {
    startRejected = true;
    mainErr = pkgError(e);
  }

  if (!installed && startRejected) {
    // FALLBACK: the jailbroken-context install was rejected. Try the
    // standalone DPI daemon (replaces our payload on the single-payload
    // loader, installs, then we restore the payload).
    const ens = (await invoke("dpi_ensure", { ip })) as {
      ok?: boolean;
      error?: string;
    };
    if (!ens.ok) {
      throw new Error(
        ens.error ||
          `Main-payload install failed (${mainErr}) and the DPI daemon didn't come up on :9040`,
      );
    }
    let resp: { ok?: boolean; rc?: number; err_message?: string } = {};
    try {
      resp = (await invoke("pkg_dpi_install", {
        ps5Addr: mgmtAddr(host),
        localPs5Path,
      })) as typeof resp;
    } catch (e) {
      resp = { ok: false, rc: 0, err_message: pkgError(e) };
    }
    // Restore our main payload — the daemon replaced it. Best-effort.
    try {
      const bp = (await invoke("payload_bundled_path")) as {
        ok?: boolean;
        path?: string;
      };
      if (bp?.ok && bp.path) {
        await invoke("payload_send", { ip, path: bp.path, port: null });
      }
    } catch {
      /* best-effort restore */
    }
    installed = !!resp.ok;
    if (!installed) {
      const rc = (resp.rc ?? 0) >>> 0;
      mainErr =
        resp.err_message ||
        mainErr ||
        `Install was rejected (0x${rc.toString(16).padStart(8, "0")}).`;
    }
  }
  return { installed, mayNotLaunch, errMessage: mainErr, stalled };
}

/**
 * One isolated PkgLibrary store per PS5 console (see the registry below). The
 * store body is unchanged from the old single-instance design — making it a
 * factory is what gives each console its own `entries` + `installing` state, so
 * installing on console A never blocks console B. Every method still takes
 * `host` (it matches this instance's console) and uses it for addresses.
 */
const makePkgLibraryStore = () =>
  createStore<PkgLibraryState>((set, get) => ({
  entries: [],
  loading: false,
  error: null,
  installing: false,
  busyNotice: null,
  installPending: false,

  async refresh(host) {
    if (!host?.trim() || get().installing) return;
    set({ loading: true, error: null });
    const addr = mgmtAddr(host);
    const titles = loadTitleCache();
    try {
      // List one dir, tolerating ENOENT (errno 2 — dir not created yet =
      // empty, not an error). `strict` re-throws any OTHER error so an
      // offline/refused console surfaces instead of silently wiping the
      // list; we use it only for the root dir. Sub-dir scans are
      // best-effort — a successful root list already proved reachability,
      // so a stray sub-dir error shouldn't blank the whole library.
      const listOne = async (dir: string, strict: boolean) => {
        try {
          return await fsListDir(addr, dir);
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          if (/fs_list_dir_opendir_errno_2\b/.test(msg)) return [];
          if (strict) throw err;
          return [];
        }
      };
      // Base/unknown live at the library root; updates + DLC in their own
      // sub-dirs (see lib/pkgStagingPath). Scan all three so a base and its
      // update both show, each badged by the dir it came from.
      const entries: PkgEntry[] = [];
      const addFrom = (
        listed: Awaited<ReturnType<typeof listOne>>,
        subdir: string,
        dir: string,
      ) => {
        for (const e of listed) {
          if (e.kind !== "file" || !e.name.toLowerCase().endsWith(".pkg")) {
            continue;
          }
          const contentId = contentIdFromName(e.name);
          entries.push({
            name: e.name,
            path: `${dir}/${e.name}`,
            size: e.size,
            contentId,
            title: titles[contentId],
            titleId: titleIdFromContentId(contentId) ?? undefined,
            category: categoryForSubdir(subdir),
            platform: platformFromTitleId(titleIdFromContentId(contentId)),
            status: "idle" as PkgStatus,
          });
        }
      };
      // Ensure the library dir exists so the root listing below never hits
      // ENOENT (which the engine surfaces as a noisy 502 + WARN). `fsMkdir`
      // is idempotent on the payload (EEXIST → success), so this is a no-op
      // once the library has been used.
      await fsMkdir(transferAddr(host), PKG_LIBRARY_DIR).catch(() => {});
      // Base/unknown pkgs live at the library root; updates + DLC each get a
      // sub-dir (see lib/pkgStagingPath). List the root once (strict — proves
      // the console is reachable), then descend into `updates`/`dlc` ONLY when
      // the root listing shows they exist (no speculative ENOENT probes).
      const rootList = await listOne(PKG_LIBRARY_DIR, true);
      addFrom(rootList, "", PKG_LIBRARY_DIR);
      const presentDirs = new Set(
        rootList.filter((e) => e.kind === "dir").map((e) => e.name),
      );
      for (const subdir of ["updates", "dlc"]) {
        if (!presentDirs.has(subdir)) continue;
        const dir = `${PKG_LIBRARY_DIR}/${subdir}`;
        addFrom(await listOne(dir, false), subdir, dir);
      }
      set({ entries: mergeListing(get().entries, entries), loading: false });
    } catch (e) {
      set({ error: pkgError(e), loading: false });
    }
  },

  async addAndUpload(localPath, host) {
    if (!host?.trim()) return;
    // An install swaps the main payload out (DPI), which kills the transfer
    // port — never start an upload while one is running.
    if (get().installing) {
      set({ error: "Can't upload while an install is in progress." });
      return;
    }
    set({ error: null });
    // 1. Parse the local .pkg header for ContentID + title, and reject inputs
    //    DPI can't take.
    let meta: SplitParseResponse;
    try {
      meta = (await invoke("pkg_metadata_split", {
        path: localPath,
      })) as SplitParseResponse;
    } catch (e) {
      set({ error: `Couldn't read .pkg header: ${pkgError(e)}` });
      return;
    }
    if ((meta.parts?.length ?? 1) > 1) {
      set({
        error:
          "Split .pkg sets aren't supported by the DPI installer — pick the single lead .pkg.",
      });
      return;
    }
    const contentId = meta.head?.content_id ?? "";
    const title = meta.head?.title;
    const category = meta.head?.category;
    // Prefer the engine's parsed platform; fall back to the title-id prefix
    // (covers headerless / FIH pkgs whose ids we recovered another way).
    const platform =
      meta.head?.platform ||
      platformFromTitleId(titleIdFromContentId(contentId));
    const totalBytes = meta.total_size ?? 0;
    if (title) cacheTitle(contentId, title);

    // 2. Name the on-PS5 file `<ContentID>.pkg` (Sony's installer keys on the
    //    basename matching the ContentID — see lib/pkgStagingPath). A base
    //    game and its update/DLC share that ContentID, so they're routed to
    //    distinct sub-directories (basename unchanged) to keep them from
    //    overwriting each other in the library.
    const basename = stagingBasename(
      contentId,
      Math.random().toString(36).slice(2),
      Date.now(),
    );
    const subdir = stagingSubdirForCategory(category);
    const destPath = subdir
      ? `${PKG_LIBRARY_DIR}/${subdir}/${basename}`
      : `${PKG_LIBRARY_DIR}/${basename}`;

    // Refuse to re-add a pkg that's already uploading to the same path:
    // two concurrent transfers to one file would corrupt it, and the two
    // poll loops would fight over the same row's progress. (A headerless
    // pkg gets a unique random basename each time, so this only triggers
    // for a real ContentID being added twice mid-upload.)
    if (
      get().entries.some(
        (e) => e.path === destPath && e.status === "uploading",
      )
    ) {
      set({ error: `${title || contentId || basename} is already uploading.` });
      return;
    }

    // The transfer port (:9113) is single-client: another upload (from the
    // Upload screen or another .pkg here) must finish before this one starts,
    // or they collide on the port. `othersBusy` is true while any such
    // transfer holds it.
    const othersBusy = () =>
      transferScreenBusy(host) ||
      get().entries.some(
        (e) => e.path !== destPath && e.status === "uploading",
      );

    // 3. Optimistic row — "queued" if it has to wait for the port, else
    //    straight to "uploading".
    const optimistic: PkgEntry = {
      name: basename,
      path: destPath,
      size: totalBytes,
      contentId,
      title,
      titleId: titleIdFromContentId(contentId) ?? undefined,
      category,
      platform,
      status: othersBusy() ? "queued" : "uploading",
      bytes: 0,
      totalBytes,
    };
    set({
      entries: [
        optimistic,
        ...get().entries.filter((e) => e.path !== destPath),
      ],
    });

    const patch = (p: Partial<PkgEntry>) =>
      set({
        entries: get().entries.map((e) =>
          e.path === destPath ? { ...e, ...p } : e,
        ),
      });

    // 4. Upload over the bulk-transfer port, polling job_status for progress.
    try {
      // Wait our turn on the single-client port instead of colliding. Bail if
      // the user removed this queued row in the meantime.
      while (othersBusy()) {
        if (!get().entries.some((e) => e.path === destPath)) return;
        await sleep(400);
      }
      patch({ status: "uploading" });
      // Make sure the console is on the matching (hardened) payload before we
      // stream — same guard the upload queue uses.
      await ensurePayloadCurrent(hostOf(host));
      // Updates/DLC stage into a sub-dir; create it first (mkdir -p,
      // EEXIST-tolerant) so the single-file transfer's open() doesn't fail
      // with ENOENT on a parent that doesn't exist yet.
      if (subdir) {
        try {
          await fsMkdir(transferAddr(host), `${PKG_LIBRARY_DIR}/${subdir}`);
        } catch (e) {
          patch({
            status: "idle",
            bytes: undefined,
            lastResult: {
              ok: false,
              message: `Couldn't create the ${subdir} folder on the PS5: ${pkgError(e)}`,
            },
          });
          return;
        }
      }
      const tx = (await invoke("transfer_file", {
        req: {
          src: localPath,
          dest: destPath,
          addr: transferAddr(host),
          tx_id: null,
        },
      })) as { job_id?: string };
      const jobId = tx.job_id;
      if (!jobId) throw new Error("upload did not start");
      let polls = 0;
      // No-progress watchdog: bail if bytes don't advance for a while.
      // We key on progress rather than a fixed total cap so an honest
      // multi-GB upload isn't killed, while a job wedged in a non-terminal
      // state (or a silently dead transfer) can't spin forever — which
      // would leave the row "uploading" and block every future install.
      const STALL_LIMIT = 240; // × 500ms = 120s with zero progress
      let lastBytes = -1;
      let stalled = 0;
      for (;;) {
        await sleep(500);
        let js: {
          status?: string;
          bytes_sent?: number;
          total_bytes?: number;
          error?: string | null;
        };
        try {
          js = (await invoke("job_status", { jobId })) as typeof js;
        } catch (e) {
          if (++polls >= 5) throw e;
          continue;
        }
        polls = 0;
        if (typeof js.bytes_sent === "number") {
          patch({ bytes: js.bytes_sent, totalBytes: js.total_bytes ?? totalBytes });
          if (js.bytes_sent > lastBytes) {
            lastBytes = js.bytes_sent;
            stalled = 0;
          } else if (++stalled >= STALL_LIMIT) {
            throw new Error("upload stalled — no progress for 2 minutes");
          }
        } else if (++stalled >= STALL_LIMIT) {
          throw new Error("upload stalled — no status from the PS5");
        }
        if (js.status === "done") break;
        if (js.status === "failed") {
          throw new Error(js.error || "upload failed");
        }
      }
      // Settle to idle and re-sync from the dir (authoritative).
      patch({ status: "idle", bytes: undefined });
      await get().refresh(host);
      // Hands-off flow: once the .pkg has landed, kick the install without a
      // second manual click (opt-out via the Install Package screen). install()
      // owns its own waiting/queueing, the FW-12 notice, and — when
      // autoRemoveAfterInstall is on — the post-install cleanup, so "upload →
      // installed → staged copy removed" becomes one action. It never throws
      // (try/finally inside), so awaiting it here is safe; the caller already
      // treats addAndUpload as fire-and-forget.
      {
        const s = useInstallSettingsStore.getState();
        // Log the post-upload decision (install or not) with the settings that
        // drove it, so a "it auto-installed/deleted even though I disabled that"
        // report is answerable from the bundle alone.
        log.info(
          "install",
          `staged pkg uploaded: ${destPath} — auto-install=${s.autoInstallAfterUpload}, auto-delete=${s.autoRemoveAfterInstall}`,
        );
        if (s.autoInstallAfterUpload) {
          await get().install(destPath, host);
        }
      }
    } catch (e) {
      // Drop the optimistic row and surface the error.
      set({
        entries: get().entries.filter((e2) => e2.path !== destPath),
        error: `Upload failed: ${pkgError(e)}`,
      });
    }
  },

  async install(path, host) {
    if (!host?.trim() || get().installing) return;
    set({ installing: true, busyNotice: null, installPending: false });
    // Any transfer that the payload swap would interrupt: an Upload-screen
    // transfer, or a .pkg upload/queued here. Installing must wait for all of
    // them so it doesn't tear the payload out mid-upload.
    const transfersActive = () =>
      transferScreenBusy(host) ||
      get().entries.some(
        (e) => e.status === "uploading" || e.status === "queued",
      );
    const patch = (p: Partial<PkgEntry>) =>
      set({
        entries: get().entries.map((e) => (e.path === path ? { ...e, ...p } : e)),
      });
    try {
      // Queue behind any active transfer instead of crashing it. Cancellable
      // via cancelPendingInstall (which flips `installing` off mid-wait).
      if (transfersActive()) {
        // `installPending` marks the cancellable WAITING window — cleared the
        // moment the real install starts below.
        set({
          installPending: true,
          busyNotice:
            "Waiting for the current upload to finish before installing…",
        });
        while (transfersActive()) {
          if (!get().installing) return; // user cancelled the pending install
          await sleep(400);
        }
        set({ installPending: false, busyNotice: null });
      }
      // The real install begins here: clear installPending so Cancel can no
      // longer abort it (even though busyNotice gets set again on FW 12.x).
      set({ installPending: false });
      // Inside the try so any throw still hits `finally` and clears the
      // `installing` flag — otherwise a wedged flag would lock the screen.
      patch({ status: "installing", lastResult: undefined });

      // FW 12.xx heads-up: the main-payload InstallByPackage hands the install
      // to Sony's installer from the jailbroken context, which briefly
      // destabilizes SceShellUI on newer firmware — the PS5 screen goes black
      // for a few seconds, then recovers with the install queued. It still
      // completes + launches fine (HW-reported), so we DON'T switch to the DPI
      // daemon (that risks an unlaunchable metadata-only install on 12.xx) —
      // we just warn so the blip isn't alarming. Gated to FW >= 12 where it's
      // observed; older firmware installs silently.
      {
        const rt =
          useConnectionStore.getState().runtimeByHost[hostOf(host)] ?? null;
        const fw = parsePS5Firmware(rt?.ps5Kernel ?? null);
        const major = fw ? parseFloat(fw) : 0;
        if (major >= 12) {
          set({
            busyNotice:
              "Installing… on FW 12.x the PS5 screen may go black for a few seconds — that's normal. Don't touch the console; the install finishes in the background.",
          });
        }
      }

      // The library entry carries the content id parsed at upload time —
      // pass it so the engine doesn't need to re-read a PC-side file (the
      // pkg is already staged on the PS5). The install cascade itself lives in
      // the shared `runPkgInstall` helper (also used by the upload queue's pkg
      // finisher), so the mechanism stays identical across both surfaces.
      const entry = get().entries.find((e) => e.path === path);
      // delete_staging = the user's Auto Delete preference: the engine keeps
      // the uploaded pkg when this is off (the separate client-side remove()
      // below is also gated on the same setting, so OFF means truly kept).
      const autoRemove =
        useInstallSettingsStore.getState().autoRemoveAfterInstall;
      const { installed, mayNotLaunch, errMessage: mainErr, stalled } =
        await runPkgInstall(
          host,
          path,
          entry?.contentId || null,
          autoRemove,
          // Live install %: a large title installs over minutes — show progress
          // instead of a frozen spinner. Guarded so a 0 total can't divide.
          (installedBytes, total) => {
            if (total > 0) {
              const pct = Math.min(
                99,
                Math.floor((installedBytes / total) * 100),
              );
              set({ busyNotice: `Installing on the PS5… ${pct}%` });
            }
          },
        );

      if (installed) {
        patch({ status: "idle", lastResult: installedLastResult(mayNotLaunch) });
        // Optional: auto-delete the spent staged .pkg so the library
        // doesn't accumulate installed packages. Best-effort + awaited so
        // the row vanishes deterministically; remove() swallows its own
        // errors (surfaces via store.error, never throws here). The ENGINE
        // already cleaned the staged file (delete_staging=autoRemove), so this
        // mainly drops the library ROW; logged so the deletion is traceable.
        if (autoRemove) {
          log.info("install", `auto-deleting staged pkg after install: ${path}`);
          await get().remove(path, host);
        } else {
          log.info(
            "install",
            `keeping staged pkg after install (auto-delete off): ${path}`,
          );
        }
      } else {
        // Not completed → the pkg was KEPT on the PS5 (never deleted on a
        // non-confirmed install). A stall gets the retry-oriented copy; a hard
        // failure gets the reject copy. Either way the staged pkg is still
        // there, so re-running the install is the natural next step.
        log.info(
          "install",
          stalled
            ? `install stalled — staged pkg KEPT for retry: ${path}`
            : `install not confirmed — staged pkg KEPT: ${path}`,
        );
        patch({
          status: "idle",
          lastResult: { ok: false, message: mainErr || "Install was rejected." },
        });
      }
    } catch (e) {
      patch({ status: "idle", lastResult: { ok: false, message: pkgError(e) } });
    } finally {
      set({ installing: false, busyNotice: null, installPending: false });
    }
  },

  async installExternal(pkg, host) {
    if (!host?.trim() || get().installing) {
      return { ok: false, message: "Another install is in progress." };
    }
    set({ installing: true, busyNotice: null, installPending: false });
    // Stage to internal with the Sony-friendly `<ContentID>.pkg` basename
    // (falls back to a unique name for headerless pkgs).
    const basename = stagingBasename(
      pkg.contentId,
      Math.random().toString(36).slice(2),
      Date.now(),
    );
    const internalPath = `${PKG_TEMP_DIR}/${basename}`;
    try {
      // Wait for any active transfer (the :9113 port is single-client and an
      // install swaps the payload).
      if (transferScreenBusy(host)) {
        set({
          busyNotice:
            "Waiting for the current upload to finish before installing…",
        });
        while (transferScreenBusy(host)) {
          if (!get().installing) return { ok: false, message: "Cancelled." };
          await sleep(400);
        }
        set({ busyNotice: null });
      }
      // 1. On-console copy USB → internal staging (Sony can't install off the
      //    exfat USB mount directly). `fsCopy` refuses to overwrite; rather
      //    than speculatively delete (which logs an ENOENT warning on the
      //    common fresh-install path), only clear + retry when the copy
      //    actually reports the dest already exists.
      set({
        // Spell out the why: Sony's installer can't read the exfat USB
        // directly, so we stage to internal storage first — and it's removed
        // again right after the install, so it doesn't permanently eat SSD
        // space. (Answers the common "why is it copying / will this fill my
        // drive" question.)
        busyNotice: `Staging ${pkg.name || pkg.contentId} from ${pkg.drive} to internal storage — removed automatically after install…`,
      });
      await fsMkdir(transferAddr(host), PKG_TEMP_DIR).catch(() => {});
      try {
        await fsCopy(mgmtAddr(host), pkg.path, internalPath);
      } catch (e) {
        if (/fs_copy_dest_exists/.test(String(e))) {
          await fsDelete(mgmtAddr(host), internalPath).catch(() => {});
          await fsCopy(mgmtAddr(host), pkg.path, internalPath);
        } else {
          throw e;
        }
      }
      set({ busyNotice: null });

      // 2. Install from the internal copy via the shared cascade. The internal
      //    copy is a TRANSIENT staging file (the user's USB/external original is
      //    untouched), so always clean it — deleteStaging: true.
      const { installed, mayNotLaunch, errMessage } = await runPkgInstall(
        host,
        internalPath,
        pkg.contentId || null,
        true,
      );

      // 3. Clean up the transient copy regardless of outcome.
      await fsDelete(mgmtAddr(host), internalPath).catch(() => {});

      return installed
        ? { ok: true, mayNotLaunch }
        : { ok: false, message: errMessage || "Install was rejected." };
    } catch (e) {
      await fsDelete(mgmtAddr(host), internalPath).catch(() => {});
      return { ok: false, message: pkgError(e) };
    } finally {
      set({ installing: false, busyNotice: null, installPending: false });
    }
  },

  async installFromConsolePath(path, host) {
    const name = path.split("/").pop() || path;
    // Removable mounts can't be installed off directly (exfat + Sony's
    // installer) — reuse the staged copy-then-install path. Derive the
    // mount root so the staging notice names the right drive.
    const mountRoot = removableMountRoot(path);
    if (mountRoot) {
      return get().installExternal(
        {
          path,
          drive: mountRoot,
          name,
          size: 0,
          contentId: "",
          titleId: "",
          platform: "",
        },
        host,
      );
    }
    // Already on internal storage (/data, /user, …): Sony can read it in
    // place, so install directly with no wasteful copy.
    if (!host?.trim() || get().installing) {
      return { ok: false, message: "Another install is in progress." };
    }
    set({ installing: true, busyNotice: null, installPending: false });
    try {
      if (transferScreenBusy(host)) {
        set({
          busyNotice:
            "Waiting for the current upload to finish before installing…",
        });
        while (transferScreenBusy(host)) {
          if (!get().installing) return { ok: false, message: "Cancelled." };
          await sleep(400);
        }
      }
      set({ busyNotice: `Installing ${name}…` });
      // In-place install of a pkg the user pointed at on the console's disk
      // (e.g. from the File System browser). It's THEIR file at THEIR path, not
      // a staging copy we made — never delete it. deleteStaging: false.
      const { installed, mayNotLaunch, errMessage } = await runPkgInstall(
        host,
        path,
        null,
        false,
      );
      return installed
        ? { ok: true, mayNotLaunch }
        : { ok: false, message: errMessage || "Install was rejected." };
    } catch (e) {
      return { ok: false, message: pkgError(e) };
    } finally {
      set({ installing: false, busyNotice: null, installPending: false });
    }
  },

  cancelPendingInstall() {
    // Only abandon an install still WAITING its turn (installPending) — never
    // yank a real install mid-swap, which would leave the payload half-loaded.
    // Keyed off installPending, NOT busyNotice: on FW 12.x busyNotice stays set
    // during the genuine install to show the "screen may go black" notice, so
    // keying off it let Cancel kill a real install.
    if (get().installPending) {
      set({ installing: false, busyNotice: null, installPending: false });
    }
  },

  async remove(path, host) {
    if (!host?.trim()) return;
    // Optimistic removal — drop the row immediately, restore on failure.
    const prev = get().entries;
    set({ entries: prev.filter((e) => e.path !== path), error: null });
    try {
      await fsDelete(mgmtAddr(host), path);
    } catch (e) {
      set({ entries: prev, error: `Delete failed: ${pkgError(e)}` });
    }
  },

  async clearFinished(host) {
    await bulkDelete(get, set, host, isFinishedPkg);
  },

  async clearAll(host) {
    // Only idle rows — never yank a file out from under an in-flight
    // upload/install (status uploading/installing/queued).
    await bulkDelete(get, set, host, (e) => e.status === "idle");
  },
  }));

/**
 * Per-console store registry. One isolated PkgLibrary store instance per PS5
 * host (port-stripped) — each console's library + install state is fully
 * independent and runs in parallel, so an install on one console never blocks
 * another. This is the literal "a separate ps5upload per console" model.
 */
export type PkgLibraryStore = ReturnType<typeof makePkgLibraryStore>;
const pkgLibraryStores = new Map<string, PkgLibraryStore>();

/** Get (creating on first use) the isolated store for one console. */
export function pkgLibraryStore(host: string): PkgLibraryStore {
  const key = hostOf(host) || "_unset_";
  let s = pkgLibraryStores.get(key);
  if (!s) {
    s = makePkgLibraryStore();
    pkgLibraryStores.set(key, s);
  }
  return s;
}

/**
 * Drop the cached store for a console. Call when a roster profile is removed
 * or re-pointed at a new IP, so a later console that REUSES that IP starts
 * clean instead of inheriting the old console's transient state (entries are
 * re-listed from the live PS5 on mount, but `installing` / `lastResult` /
 * `busyNotice` would otherwise carry over). No-op if nothing is cached for the
 * host. The next `pkgLibraryStore(host)` lazily creates a fresh instance.
 */
export function evictPkgLibraryStore(host: string): void {
  pkgLibraryStores.delete(hostOf(host) || "_unset_");
}

/**
 * Hook: subscribe to ONE console's PkgLibrary store. Pass the host so each
 * Install Package view binds to that console's isolated state. (Replaces the
 * old global `usePkgLibrary(selector)` — the host argument is what makes the
 * per-console isolation work.)
 */
export function usePkgLibrary<T>(
  host: string,
  selector: (s: PkgLibraryState) => T,
): T {
  return useStore(pkgLibraryStore(host), selector);
}

/** Shared optimistic bulk-delete for clearFinished/clearAll. Drops the
 *  matching rows immediately, deletes each on the PS5 concurrently, and
 *  restores only the ones whose delete failed (so a partial failure
 *  leaves the list accurate). Pure-ish: takes the store's get/set so it
 *  unit-tests against the store with a mocked fsDelete. */
async function bulkDelete(
  get: () => PkgLibraryState,
  set: (
    partial:
      | Partial<PkgLibraryState>
      | ((s: PkgLibraryState) => Partial<PkgLibraryState>),
  ) => void,
  host: string,
  pred: (e: PkgEntry) => boolean,
): Promise<void> {
  if (!host?.trim()) return;
  const prev = get().entries;
  const targets = prev.filter(pred);
  if (targets.length === 0) return;
  const targetPaths = new Set(targets.map((e) => e.path));
  // Optimistic: drop all targets up front.
  set({ entries: prev.filter((e) => !targetPaths.has(e.path)), error: null });
  const addr = mgmtAddr(host);
  const failed: PkgEntry[] = [];
  await Promise.all(
    targets.map(async (e) => {
      try {
        await fsDelete(addr, e.path);
      } catch {
        failed.push(e);
      }
    }),
  );
  if (failed.length > 0) {
    const failedPaths = new Set(failed.map((e) => e.path));
    // Re-insert the rows we couldn't delete, de-duped against whatever
    // the list looks like now (a concurrent refresh may have re-added
    // some), and surface a single summary error.
    set((s) => ({
      entries: [
        ...s.entries.filter((e) => !failedPaths.has(e.path)),
        ...failed,
      ],
      error: `Failed to delete ${failed.length} file(s).`,
    }));
  }
}
