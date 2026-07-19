import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { invoke } from "../lib/invokeLogged";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

import {
  fsListDir,
  fsDelete,
  fsMkdir,
  fsCopy,
  fsOpStatus,
  pkgMetadataConsole,
  toastPush,
  installFreeBytes,
  consoleReadiness,
} from "../api/ps5";
import type { ExternalPkg } from "../api/ps5";
import { formatBytes } from "../lib/format";
import { hostOf, mgmtAddr, transferAddr } from "../lib/addr";
import { removableMountRoot } from "../lib/mountPaths";
import { humanizePs5Error } from "../lib/humanizeError";
import {
  stagingBasename,
  stagingSubdirForCategory,
  categoryForSubdir,
  isAddonCategory,
} from "../lib/pkgStagingPath";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { useInstallSettingsStore } from "./installSettings";
import { useConnectionStore } from "./connection";
import { log } from "./logs";
import { pushNotification } from "./notifications";
import { useActivityHistoryStore } from "./activityHistory";
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

/** A warning when a pkg likely won't fit, else null. `neededBytes` is the pkg
 *  size (a lower bound on the installed size — PS5-native pkgs decompress
 *  larger, PS4 backports install ~1:1); `freeBytes` is free space across
 *  installable volumes, or null when it couldn't be read (then we never block —
 *  return null). Surfaced BEFORE the install so the user isn't left waiting
 *  through a doomed install only to hit a late out-of-space error (issue #115).
 *  Exported for unit testing. */
export function installSpaceWarning(
  label: string,
  neededBytes: number,
  freeBytes: number | null,
): string | null {
  if (freeBytes == null || neededBytes <= 0) return null;
  if (neededBytes <= freeBytes) return null;
  return `Not enough free space to install ${label}: it needs about ${formatBytes(
    neededBytes,
  )}, but only ${formatBytes(
    freeBytes,
  )} is free on the PS5. Free up space (uninstall a game, or clear staged packages) and try again. Note: this is an estimate — the installed size can be a bit larger.`;
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
  /** The original on-disk filename the user uploaded, e.g.
   *  `Jak X… [v01.04].pkg`. The staged file is renamed to `<ContentID>.pkg`
   *  (Sony's installer keys on that), and a base game + its updates share a
   *  ContentID — so the PARAM.SFO `title` is identical across them and the
   *  staged name is useless for telling versions apart. The original filename
   *  (which usually carries the version) is the one human-readable
   *  distinguisher, so we capture it at upload and cache it (path → name).
   *  Best-effort, like `title`: undefined for files staged on another machine
   *  or after the cache was cleared. */
  originalName?: string;
  /** PARAM.SFO `APP_VER` (e.g. `01.04`) — the authoritative package version,
   *  parsed at upload and cached (path → version). For a patch this is the
   *  definitive "which update is this", since updates share a ContentID and a
   *  title. Best-effort, same caveats as `originalName`. */
  appVer?: string;
  /** True once THIS staged package has been installed via the app (persisted
   *  per path). Lets an update/DLC row show "Reinstall" — the console's
   *  app_list can't confirm a specific update/DLC was applied (it only tracks
   *  the base title id), so this per-package flag is how we track them. */
  installedHere?: boolean;
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

/** Install ordering for a staged library row: base (0) → update (1) → DLC (2).
 *  Mirrors `uploadQueue.installOrderPriority` but reads a `PkgEntry` (PARAM.SFO
 *  category `gd`/`gp`/`ac`, falling back to a `/updates/` or `/dlc/` path hint
 *  for headerless rows). `installAll` sorts by this so an add-on never installs
 *  before its base — Sony's installer accepts an update/DLC whose base isn't
 *  present yet but then lands nothing. Exported for unit testing. */
export function pkgEntryInstallOrder(e: Pick<PkgEntry, "category" | "path">): number {
  const cat = e.category;
  if (cat === "gp") return 1;
  if (cat === "ac") return 2;
  if (cat === "gd") return 0;
  if (/\/updates\/[^/]*$/.test(e.path)) return 1;
  if (/\/dlc\/[^/]*$/.test(e.path)) return 2;
  return 0;
}

/**
 * Whether a library row should get the "installed"/"Reinstall" treatment
 * (badge + secondary button) for a given set of installed title ids.
 *
 * The console's `app_list` is keyed on TITLE id, which only proves the BASE
 * game is present — it says nothing about whether a specific update/DLC has
 * been applied, because an add-on shares the base game's title id (a patch
 * literally bumps the base title's version). So an installed base would
 * otherwise make a never-installed update read as "INSTALLED · Reinstall"
 * (hardware-reported). Only a base game is eligible; an add-on always reads as
 * installable. Exported for unit testing.
 */
export function pkgRowInstalled(
  entry: Pick<PkgEntry, "titleId" | "category" | "installedHere">,
  installedTitleIds: Set<string>,
): boolean {
  // We installed THIS exact package via the app → it's installed, whatever its
  // category. This is the ONLY reliable signal for an update/DLC (the console's
  // app_list is keyed on the base title id and can't confirm a specific
  // update/DLC was applied).
  if (entry.installedHere) return true;
  // Otherwise only a base game can be confirmed from app_list — an add-on
  // shares the base's title id, so a present base must NOT make a
  // never-installed update/DLC read as installed (the 3.3.8 fix).
  return (
    !!entry.titleId &&
    installedTitleIds.has(entry.titleId) &&
    !isAddonCategory(entry.category)
  );
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
    const raw = safeGetItem(TITLE_CACHE_KEY);
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
    safeSetItem(TITLE_CACHE_KEY, JSON.stringify(c));
  } catch {
    /* best-effort */
  }
}

// ── Per-path metadata cache ───────────────────────────────────────────
// The staged file is renamed to `<ContentID>.pkg` (losing the user's original
// filename) and refresh-from-disk lists files without re-parsing them (so the
// version is unknown then too). Both are the only things that tell a game's
// updates apart — they share a ContentID *and* a PARAM.SFO title — so we
// capture them at upload and cache here. Keyed by the on-PS5 PATH (unique per
// staged file) rather than ContentID, because a base and its update share a
// ContentID but live at distinct paths. A newer update overwrites the same
// path (updates are cumulative — you only keep the latest), so the cached
// entry tracks whatever is currently staged there.
const PATH_META_CACHE_KEY = "ps5upload.pkg_library.pathmeta.v1";

/** Per-staged-path metadata we can't recover from a bare dir listing. */
interface PkgPathMeta {
  /** Original uploaded filename, e.g. `Jak X… [v01.04].pkg`. */
  name?: string;
  /** PARAM.SFO `APP_VER`, e.g. `01.04` — the authoritative package version. */
  appVer?: string;
  /** PARAM.SFO `CATEGORY` (`gd`/`gp`/`ac`) — authoritative, vs. the directory
   *  inference. Populated when we read the staged pkg off the console. */
  category?: string;
}

function loadPathMetaCache(): Record<string, PkgPathMeta> {
  if (typeof window === "undefined") return {};
  try {
    const raw = safeGetItem(PATH_META_CACHE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function cachePathMeta(path: string, meta: PkgPathMeta): void {
  if (!path) return;
  try {
    const c = loadPathMetaCache();
    // Merge so a later partial write doesn't drop a previously-cached field.
    c[path] = { ...c[path], ...meta };
    safeSetItem(PATH_META_CACHE_KEY, JSON.stringify(c));
  } catch {
    /* best-effort */
  }
}

// Per-CONSOLE record of which staged paths we've installed. Keyed by bare host
// (port-stripped) → the set of installed paths on THAT console. This must NOT
// live in the path-only PATH_META cache: staged packages land at identical
// paths on every console (e.g. the shared staging dir), so a path-keyed flag
// would make installing on one console light up "Reinstall" on all the others
// that happen to have the same file. The console's own app_list can't
// distinguish a specific update/DLC (it only tracks the base title id), so this
// is the authoritative per-console signal. Survives restarts.
const INSTALLED_CACHE_KEY = "ps5upload.pkg_library.installed.v1";

function loadInstalledCache(): Record<string, string[]> {
  if (typeof window === "undefined") return {};
  try {
    const raw = safeGetItem(INSTALLED_CACHE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

/** Persistently mark the staged package at `path` as installed ON THIS CONSOLE,
 *  so its library row reads "Reinstall" instead of "Install" — scoped to `host`
 *  so the same file staged on a sibling console is unaffected. Called from both
 *  install paths (the library tab and the upload-queue finisher). */
export function recordPkgInstalled(host: string, path: string): void {
  if (!path || typeof window === "undefined") return;
  try {
    const h = hostOf(host);
    const c = loadInstalledCache();
    const set = new Set(c[h] ?? []);
    set.add(path);
    c[h] = [...set];
    safeSetItem(INSTALLED_CACHE_KEY, JSON.stringify(c));
  } catch {
    /* best-effort */
  }
}

/** Whether `path` was installed on `host` via the app (see recordPkgInstalled). */
export function isPkgInstalledHere(host: string, path: string): boolean {
  if (!path || typeof window === "undefined") return false;
  return (loadInstalledCache()[hostOf(host)] ?? []).includes(path);
}

/** The trailing path component of a local file path (handles both `/` and
 *  Windows `\\` separators). Used to capture the user's original .pkg
 *  filename at upload time. */
function basenameOf(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
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
    /** PARAM.SFO APP_VER ("01.04") — the package's application version. */
    app_ver?: string;
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

  /** True while `installAll` is driving a sequential batch. Cosmetic — it
   *  disables the Install-all button and shows batch progress; it does NOT
   *  gate the inner per-pkg `install()`, which self-serializes on `installing`. */
  installingAll: boolean;

  refresh: (host: string) => Promise<void>;
  addAndUpload: (localPath: string, host: string) => Promise<void>;
  install: (path: string, host: string) => Promise<void>;
  /** Stream-install (beta, #81) a local PC-side `.pkg` WITHOUT uploading it
   *  to PS5 staging first. The engine serves the file over HTTP at
   *  `/pkg-host/{session}/` and the DPI daemon pulls it directly. Saves the
   *  staging upload (and the disk space) for the quick-install case. The
   *  pkg is NOT retained on the PS5 afterwards — nothing was staged.
   *  Shares the `installing` lock with `install()`.
   *  Returns `{ ok, message?, mayNotLaunch? }`. */
  installStream: (
    localPcPath: string,
    host: string,
  ) => Promise<{ ok: boolean; message?: string; mayNotLaunch?: boolean }>;
  /** Install every staged, not-yet-installed, idle row sequentially, in
   *  base → update → DLC order (`pkgEntryInstallOrder`). Each item runs the
   *  full readiness-gated `install()` cascade; one failure doesn't abort the
   *  rest. Ends with a single summary bell. No-op if a single install is
   *  already running. */
  installAll: (host: string) => Promise<void>;
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

/** Delete a staged pkg with a few retries on a transient payload failure. Right
 *  after an install the staged file can be momentarily busy (Sony's installer
 *  is still releasing it), and a single attempt then surfaces a scary
 *  "fs_delete_failed". Retry a couple of times with a short backoff; throws the
 *  last error only if every attempt fails. */
async function fsDeleteWithRetry(addr: string, path: string): Promise<void> {
  let lastErr: unknown;
  for (let attempt = 0; attempt < 3; attempt++) {
    if (attempt > 0) await sleep(700);
    try {
      await fsDelete(addr, path);
      return;
    } catch (e) {
      lastErr = e;
    }
  }
  throw lastErr;
}

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

// ── Console-readiness gate ───────────────────────────────────────────────────
// A console goes unresponsive on the AppListRegistered frame while it recovers
// from a prior install (the post-install SceShellUI black-screen blip). Firing
// an install into that window is what produces the transient rejections seen on
// hardware — DPI rc=0x80020002 and appinst 0x80b21106 — which clear once the
// console settles (the user's logs: rejected twice, then ok minutes later after
// they waited). We gate installs on a readiness probe and retry the transient.
const READY_POLL_MS = 1_500;
/** Cap on each readiness wait. We never hard-block forever — if the probe can't
 *  clear (e.g. an older payload that can't report registered apps), attempting
 *  the install is better than refusing to try, and the DPI transient-retry is
 *  the real safety net for a genuinely-busy console. Kept modest (30s) so a
 *  console whose probe never clears adds a bounded delay, not a 90s stall; the
 *  real post-install blip settles well within this. */
const READY_WAIT_TIMEOUT_MS = 30_000;
/** The DPI daemon's "console not in an installable state" rejection — transient
 *  (clears on settle). Retried with a readiness gate rather than failed outright. */
const DPI_TRANSIENT_BUSY_RC = 0x80020002;
/** How many times to (re)attempt a DPI install that keeps hitting the transient
 *  busy rc, each gated on the console becoming ready again. */
const DPI_MAX_ATTEMPTS = 4;

/**
 * Poll the console-readiness probe (the AppListRegistered round-trip) until it
 * reports ready, or `timeoutMs` elapses. Returns true once ready, false on
 * timeout. The first probe runs with no delay — a settled console proceeds
 * instantly. `onWait` fires before each subsequent poll so the caller can
 * surface a "waiting for the PS5…" notice. Exported for the queue's pre-install
 * gate and for tests.
 */
export async function waitForConsoleReady(
  host: string,
  opts: { timeoutMs?: number; onWait?: (elapsedMs: number) => void } = {},
): Promise<boolean> {
  const timeoutMs = opts.timeoutMs ?? READY_WAIT_TIMEOUT_MS;
  const maxAttempts = Math.max(1, Math.ceil(timeoutMs / READY_POLL_MS));
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    if (await consoleReadiness(host)) return true;
    if (attempt < maxAttempts - 1) {
      opts.onWait?.(attempt * READY_POLL_MS);
      await sleep(READY_POLL_MS);
    }
  }
  return false;
}
/** Absolute backstop so a pathological engine that keeps returning "install"
 *  forever can't block the queue indefinitely. Generously past the engine's
 *  own 2h session GC — under normal operation the engine reaches a terminal
 *  verdict (done/stalled) long before this. On hitting it we KEEP the pkg. */
const PKG_VERIFY_SAFETY_CAP_MS = 3 * 60 * 60 * 1000;

/** Re-install guidance appended when Sony fails the async install — mirrors the
 *  may-not-launch copy so the user has a concrete next step. */
const PKG_ASYNC_FAILED_HINT =
  'the PS5 reported the install didn’t complete (the tile may show "Can’t start the game or app"). That tile is empty and safe to delete from the PS5. Re-install from the PS5: Settings → System → Debug Settings → Game → Package Installer.';

/** Stall guidance — the install accepted but disk progress flatlined. The
 *  staged pkg was KEPT, so the user can simply retry. We deliberately do NOT
 *  blame free space (a stall has many causes, and users with plenty of space
 *  found that misleading) and we tell them the empty tile is safe to delete. */
const PKG_STALL_HINT =
  'ps5upload couldn’t confirm the install finished. On newer firmware the PS5 often keeps installing in the background — if a tile appeared on your PS5 and it’s downloading or shows progress, let it finish; it becomes playable when that completes (check the PS5 home screen, or its Notifications / Downloads). Your package was kept on the PS5, so if nothing appeared — or a tile appeared that won’t open ("Can’t start the game or app", which is empty and safe to delete) — you can simply try again. For stubborn .pkg files (often PS4 backports), the PS5’s own Package Installer (Settings → System → Debug Settings → Game → Package Installer) is the most reliable.';

/** Guidance when a PATCH/UPDATE (a "…DP" package) can't be applied even after
 *  the DPI fallback. ps5upload applies updates through Sony's safe installer
 *  (in-process appinst, or the DPI daemon when the in-process path hits a
 *  firmware authid gate) — never the destructive tier that would delete the
 *  base. When even DPI declines it, the update usually doesn't match the
 *  installed game (or the base isn't installed). The base game is untouched;
 *  the PS5's own Package Installer is the most reliable last resort. This
 *  replaces the raw, misleading "PKG header — corrupt or wrongly named" text. */
export const PKG_PATCH_REJECTED_HINT =
  "This update couldn’t be applied. ps5upload installs updates through the PS5’s safe install path (never one that could delete your base game), and on this console the PS5 declined it — most often because the update doesn’t match your installed version of the game, or the base game isn’t installed yet. Your base game is untouched. If you have the right update, you can also apply it from the PS5 itself: Settings → System → Debug Settings → Game → Package Installer.";

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
 * Install a staged/-on-disk `.pkg` via the standalone DPI daemon (:9040): bring
 * the daemon up (it replaces our payload on the single-payload loader), install,
 * then restore our payload. This is the path elf-arsenal uses to install
 * directly from a USB/exFAT path. Returns `daemonFailed:true` distinctly so a
 * caller that needs the cascade semantics (runPkgInstall) can still throw on a
 * genuine "daemon never came up" dead-end. The `rc==0`/`ok` here is NOT proof
 * of a real install — callers should confirm with `titleRegisteredOnDisk`.
 */
async function runDpiInstall(
  host: string,
  localPs5Path: string,
  onStatus?: (msg: string) => void,
): Promise<{ ok: boolean; errMessage: string; daemonFailed: boolean; rc: number }> {
  const ip = hostOf(host);
  // dpi_ensure sends the DPI ELF to the loader port (:9021), which REPLACES the
  // running main payload with the clean DPI process. Log around it: this is the
  // exact moment the main helper is torn down, and issue #152's "helper dies
  // ~4s after a rejected update" reports land right here — the next bundle will
  // show whether dpi_ensure succeeded, timed out, or never returned.
  log.info("install", `DPI ensure: bringing up daemon on ${ip}:9040 (loads via :9021)`);
  const ens = (await invoke("dpi_ensure", { ip })) as {
    ok?: boolean;
    error?: string;
    listening?: boolean;
    sent?: boolean;
  };
  log.info(
    "install",
    `DPI ensure result: ok=${ens.ok} listening=${ens.listening ?? "?"} sent=${ens.sent ?? "?"}` +
      (ens.error ? ` error="${ens.error}"` : ""),
  );
  if (!ens.ok) {
    return {
      ok: false,
      daemonFailed: true,
      rc: 0,
      errMessage: ens.error || "the DPI daemon didn't come up on :9040",
    };
  }
  // Send the install, retrying the transient "console busy" rejection
  // (0x80020002) — it clears once the console settles, so we gate each retry on
  // the readiness probe instead of failing the way a single attempt used to.
  let resp: { ok?: boolean; rc?: number; err_message?: string } = {};
  for (let attempt = 1; attempt <= DPI_MAX_ATTEMPTS; attempt++) {
    try {
      resp = (await invoke("pkg_dpi_install", {
        ps5Addr: mgmtAddr(host),
        localPs5Path,
      })) as typeof resp;
    } catch (e) {
      resp = { ok: false, rc: 0, err_message: pkgError(e) };
    }
    const rcNow = (resp.rc ?? 0) >>> 0;
    if (resp.ok || rcNow !== DPI_TRANSIENT_BUSY_RC || attempt === DPI_MAX_ATTEMPTS) {
      break;
    }
    // Transient busy → wait for the console to settle, then retry.
    onStatus?.(
      `PS5 is busy finishing the last install — waiting for it to be ready (attempt ${attempt}/${DPI_MAX_ATTEMPTS})…`,
    );
    await waitForConsoleReady(host, {
      onWait: () => onStatus?.("Waiting for the PS5 to be ready…"),
    });
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
  const ok = !!resp.ok;
  const rc = (resp.rc ?? 0) >>> 0;
  return {
    ok,
    daemonFailed: false,
    rc,
    errMessage: ok
      ? ""
      : resp.err_message ||
        `Install was rejected (0x${rc.toString(16).padStart(8, "0")}).`,
  };
}

/**
 * Streaming/direct install (beta, #81): hand the DPI daemon the engine's
 * `/pkg-host/` URL for an existing session instead of a staged PS5 path.
 * The daemon pulls the pkg over HTTP — no staging copy is uploaded to
 * the PS5 first. Mirrors `runDpiInstall`'s daemon-bring-up + restore
 * dance (the DPI ELF still replaces the main payload on the loader), but
 * sends a session_id rather than a local path.
 *
 * The caller MUST have already registered the session with the engine
 * (via `pkg_install_start` with `localPs5Path: null` and a PC-side file
 * path) so `/pkg-host/{session}/` is serving bytes when the daemon comes
 * asking. Returns `daemonFailed:true` distinctly so callers can treat a
 * "daemon never came up" dead-end separately from a Sony-side reject.
 *
 * Like `runDpiInstall`, `ok:true` here is the daemon's word that
 * `InstallByPackage` accepted — callers should confirm with
 * `titleRegisteredOnDisk` before claiming a real install.
 */
async function runDpiDirectInstall(
  host: string,
  sessionId: string,
  onStatus?: (msg: string) => void,
): Promise<{ ok: boolean; errMessage: string; daemonFailed: boolean; rc: number }> {
  const ip = hostOf(host);
  log.info("install", `DPI ensure (direct): bringing up daemon on ${ip}:9040 (loads via :9021)`);
  const ens = (await invoke("dpi_ensure", { ip })) as {
    ok?: boolean;
    error?: string;
    listening?: boolean;
    sent?: boolean;
  };
  log.info(
    "install",
    `DPI ensure (direct) result: ok=${ens.ok} listening=${ens.listening ?? "?"} sent=${ens.sent ?? "?"}` +
      (ens.error ? ` error="${ens.error}"` : ""),
  );
  if (!ens.ok) {
    return {
      ok: false,
      daemonFailed: true,
      rc: 0,
      errMessage: ens.error || "the DPI daemon didn't come up on :9040",
    };
  }
  let resp: { ok?: boolean; rc?: number; err_message?: string } = {};
  for (let attempt = 1; attempt <= DPI_MAX_ATTEMPTS; attempt++) {
    try {
      resp = (await invoke("pkg_dpi_direct_install", {
        ps5Addr: mgmtAddr(host),
        sessionId,
      })) as typeof resp;
    } catch (e) {
      resp = { ok: false, rc: 0, err_message: pkgError(e) };
    }
    const rcNow = (resp.rc ?? 0) >>> 0;
    if (resp.ok || rcNow !== DPI_TRANSIENT_BUSY_RC || attempt === DPI_MAX_ATTEMPTS) {
      break;
    }
    onStatus?.(
      `PS5 is busy finishing the last install — waiting for it to be ready (attempt ${attempt}/${DPI_MAX_ATTEMPTS})…`,
    );
    await waitForConsoleReady(host, {
      onWait: () => onStatus?.("Waiting for the PS5 to be ready…"),
    });
  }
  // Restore the main payload — the daemon replaced it. Best-effort.
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
  const ok = !!resp.ok;
  const rc = (resp.rc ?? 0) >>> 0;
  return {
    ok,
    daemonFailed: false,
    rc,
    errMessage: ok
      ? ""
      : resp.err_message ||
        `Install was rejected (0x${rc.toString(16).padStart(8, "0")}).`,
  };
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
/** Map a PARAM.SFO CATEGORY to BGFT's package_type string. MUST mirror the
 *  engine's `derive_package_type` (ps5upload-pkg). The "…DP" suffix is what
 *  arms the payload's patch guard — a patch (`gp`) shares the base game's
 *  content_id, so re-registering it via the shellui-rpc / BGFT fallbacks WIPES
 *  the base. For a staged/local install the engine does NOT re-parse the pkg
 *  (the bytes are on the PS5), so this is the ONLY place the patch-ness reaches
 *  the payload: passing `null` (the old behaviour) made every patch look like a
 *  full game (PS4GD) and defeated the guard — a hardware-confirmed data-loss
 *  bug (a Jak X patch deleted the installed base). Returns null for unknown
 *  categories so the payload keeps its own default. */
export function pkgTypeForCategory(category?: string | null): string | null {
  switch (category) {
    case "gd":
      return "PS4GD"; // full game
    case "gp":
      return "PS4DP"; // patch (shares the base content_id — guarded)
    case "ac":
      return "PS4AC"; // add-on / DLC
    case "gde":
      return "PS4GDE";
    case "la":
      return "PS4LA";
    default:
      return null;
  }
}

export async function runPkgInstall(
  host: string,
  localPs5Path: string,
  contentId: string | null,
  /** BGFT package_type for the staged pkg, derived from its PARAM.SFO category
   *  (see `pkgTypeForCategory`). Passed straight through to the payload so a
   *  patch ("…DP") arms the data-loss guard. Null ⇒ payload default. */
  packageType: string | null,
  // The user's "Auto Delete after installation" preference. When false, the
  // engine KEEPS the staged pkg after install instead of deleting it. This is
  // the single source of truth for staging deletion now — previously the engine
  // always deleted, ignoring the setting (the reported data-loss bug).
  deleteStaging: boolean,
  // Called each status poll with the install's live byte progress, so the UI
  // can render a real % for large titles (Sony's BGFT progress isn't
  // meaningful on the file:// staging path). Optional — no-op if omitted.
  onProgress?: (installedBytes: number, total: number) => void,
  // Called with a human-readable status line while the install waits on the
  // console-readiness gate (pre-install + DPI transient retry). Lets the caller
  // surface "Waiting for the PS5 to be ready…" instead of a frozen UI.
  onStatus?: (msg: string) => void,
): Promise<PkgInstallOutcome> {
  // PRE-INSTALL GATE: don't fire an install into the post-install SceShellUI
  // recovery window — that's what produces the transient rejections. Wait for
  // the console to answer the readiness probe cleanly first. Best-effort: on
  // timeout we proceed anyway (an older payload may never report ready, and the
  // transient-retry below still rescues a genuinely-busy console).
  await waitForConsoleReady(host, {
    onWait: () => onStatus?.("Waiting for the PS5 to be ready…"),
  });

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
  // The package_type the install actually ran with — the engine resolves it
  // from the staged pkg when we didn't send one, so this is authoritative for
  // "was this treated as a patch" even on the USB/queue/File-System paths.
  let resolvedType = packageType ?? "";
  try {
    const r = (await invoke("pkg_install_start", {
      ps5Addr: mgmtAddr(host),
      path: null,
      splitRoot: null,
      packageTypeOverride: packageType,
      localPs5Path,
      contentId: contentId || null,
      deleteStaging,
    })) as {
      err_code?: number;
      register_path?: string;
      err_message?: string;
      may_not_launch?: boolean;
      session_id?: string;
      package_type?: string;
    };
    if (r.package_type) resolvedType = r.package_type;
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
    // FALLBACK: the in-process install was rejected. Hand off to the standalone
    // DPI daemon (:9040), which runs Sony's appinst in a SEPARATE, properly-
    // authid'd process. This rescues two cases the in-process path can't:
    //   • a patch ("…DP") whose only remaining in-process route is the
    //     destructive shellui-rpc tier the data-loss guard forbids, and
    //   • firmware (e.g. FW 5.10) where the in-process appinst file:// install
    //     hits an authid gate (err 0x80B21106 — the base game itself lands via
    //     shellui-rpc, but a patch can't use that tier).
    // DPI is NON-destructive: appinst applies an update on top of the base, so
    // it's safe for patches. HW-proven on the Phat — a Jak X v01.04 patch landed
    // in /user/patch/CUSA07842/patch.pkg with the 3.8 GB base in
    // /user/app/CUSA07842/app.pkg fully intact.
    // Instrument the whole DPI cascade at info level. Before this, the fallback
    // ran entirely through busyNotice/onStatus (UI-only, never logged), so a
    // bug bundle at the default `info` level showed the in-process rejection and
    // then the payload dying with NO trace of whether DPI was even attempted —
    // which made the "updates crash the helper" reports (issue #152) impossible
    // to pin down from logs alone. Now the bundle shows the exact cascade.
    log.info(
      "install",
      `in-process install rejected (${mainErr}) for type=${resolvedType || "?"} — ` +
        `handing off to DPI daemon (:9040)`,
    );
    const dpi = await runDpiInstall(host, localPs5Path, onStatus);
    log.info(
      "install",
      `DPI fallback result: ok=${dpi.ok} daemonFailed=${dpi.daemonFailed} ` +
        `rc=0x${(dpi.rc >>> 0).toString(16).padStart(8, "0")}` +
        (dpi.errMessage ? ` err="${dpi.errMessage}"` : ""),
    );
    if (dpi.daemonFailed) {
      // DPI couldn't even come up. For a patch, give update-specific guidance
      // (base is safe; try the PS5's Package Installer) rather than a raw
      // daemon error.
      if (resolvedType.endsWith("DP")) {
        return {
          installed: false,
          mayNotLaunch,
          errMessage: PKG_PATCH_REJECTED_HINT,
          stalled,
        };
      }
      throw new Error(
        `Main-payload install failed (${mainErr}) and ${dpi.errMessage}`,
      );
    }
    installed = dpi.ok;
    if (!installed) {
      // DPI ran but the PS5 declined the install. An update that can't apply
      // (wrong base version, or no base) gets the update-specific guidance;
      // everything else keeps DPI's own error.
      mainErr = resolvedType.endsWith("DP")
        ? PKG_PATCH_REJECTED_HINT
        : dpi.errMessage || mainErr;
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
  installingAll: false,

  async refresh(host) {
    if (!host?.trim() || get().installing) return;
    set({ loading: true, error: null });
    const addr = mgmtAddr(host);
    const titles = loadTitleCache();
    const pathMeta = loadPathMetaCache();
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
          const path = `${dir}/${e.name}`;
          entries.push({
            name: e.name,
            path,
            size: e.size,
            contentId,
            title: titles[contentId],
            originalName: pathMeta[path]?.name,
            appVer: pathMeta[path]?.appVer,
            installedHere: isPkgInstalledHere(host, path),
            titleId: titleIdFromContentId(contentId) ?? undefined,
            // Authoritative category (read off the console) when we have it,
            // else the directory inference (updates/ → gp, dlc/ → ac).
            category: pathMeta[path]?.category ?? categoryForSubdir(subdir),
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
      // Fill in the authoritative version/category/title for rows the
      // upload-time cache didn't capture (e.g. pkgs staged before this
      // existed) by reading each staged pkg off the console. Fire-and-forget
      // so the list shows immediately and rows upgrade in place.
      void enrichStagedMetadata(get, set, host);
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
    // The user's original filename (e.g. `… [v01.04].pkg`) and the authoritative
    // PARAM.SFO version — the things that distinguish a game's updates, which
    // share a ContentID and a title. Captured here at upload (the only point we
    // parse the pkg) and cached by staged path for later refresh-from-disk.
    const originalName = basenameOf(localPath);
    const appVer = meta.head?.app_ver || undefined;

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
    // Remember the filename + version for this staged path so the row can show
    // them (survives refresh-from-disk and app restarts via localStorage).
    cachePathMeta(destPath, { name: originalName, appVer });

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
      originalName,
      appVer,
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
      const label = entry?.title || entry?.contentId || basenameOf(path);

      // Pre-flight free-space check (#115): if the pkg clearly won't fit, warn
      // NOW instead of letting the user wait through a doomed install that ends
      // in a late out-of-space error. Best-effort — if free space can't be read
      // it returns null and we never block.
      const spaceWarn = installSpaceWarning(
        label,
        entry?.size ?? 0,
        await installFreeBytes(transferAddr(host)),
      );
      if (spaceWarn) {
        patch({ status: "idle", lastResult: { ok: false, message: spaceWarn } });
        pushNotification("warning", `Not enough space for ${label}`, {
          body: spaceWarn,
        });
        return;
      }

      // delete_staging = the user's Auto Delete preference: the engine keeps
      // the uploaded pkg when this is off (the separate client-side remove()
      // below is also gated on the same setting, so OFF means truly kept).
      const autoRemove =
        useInstallSettingsStore.getState().autoRemoveAfterInstall;
      // Surface the install in the global Activity bar at the bottom of the app
      // (with a live %), so it stays visible while the user browses other
      // screens — same treatment uploads/downloads already get.
      const actId = useActivityHistoryStore
        .getState()
        .start("library-install", `Installing ${label}`, {
          addr: mgmtAddr(host),
        });
      const { installed, mayNotLaunch, errMessage: mainErr, stalled } =
        await runPkgInstall(
          host,
          path,
          entry?.contentId || null,
          pkgTypeForCategory(entry?.category),
          autoRemove,
          // Live install %: a large title installs over minutes — feed both the
          // inline notice and the global Activity bar so progress shows
          // everywhere. Guarded so a 0 total can't divide.
          (installedBytes, total) => {
            useActivityHistoryStore
              .getState()
              .update(actId, { bytes: installedBytes, totalBytes: total });
            if (total > 0) {
              const pct = Math.min(
                99,
                Math.floor((installedBytes / total) * 100),
              );
              set({ busyNotice: `Installing on the PS5… ${pct}%` });
            }
          },
          // Readiness-gate status (pre-install wait / DPI transient retry).
          (msg) => set({ busyNotice: msg }),
        );
      useActivityHistoryStore
        .getState()
        .finish(actId, installed ? "done" : stalled ? "stopped" : "failed");

      if (installed) {
        // Record THIS package as installed ON THIS CONSOLE (persisted) and
        // reflect it on the row, so an update/DLC that's been installed shows
        // "Reinstall" — not "Install" — even though app_list can't confirm an
        // add-on. Scoped to `host` so a sibling console with the same staged
        // file isn't wrongly marked installed.
        recordPkgInstalled(host, path);
        patch({
          status: "idle",
          installedHere: true,
          lastResult: installedLastResult(mayNotLaunch),
        });
        // Notify on confirmed completion (the engine only reports installed
        // once the title actually registered on disk — i.e. it's ready to
        // play). Surfaces in the bell even if the user navigated away while a
        // large title finished, which is exactly when a heads-up is wanted.
        pushNotification(
          mayNotLaunch ? "warning" : "success",
          `${label} installed`,
          {
            body: mayNotLaunch
              ? PKG_MAY_NOT_LAUNCH_MESSAGE
              : "Installed on the PS5 and ready to play.",
          },
        );
        // Flash a toast on the PS5 itself (sceNotificationSend) so the
        // confirmation shows on the console screen too — handy when the desktop
        // app isn't focused. Fire-and-forget; never let it affect the install.
        void toastPush(mgmtAddr(host), `${label} installed`, {
          subtitle: mayNotLaunch
            ? "Installed — may need the PS5’s Package Installer to launch"
            : "Ready to play",
        }).catch(() => {});
        // Optional: auto-delete the spent staged .pkg so the library doesn't
        // accumulate installed packages. The ENGINE usually already removed the
        // staged file (delete_staging=autoRemove), so this mainly drops the
        // library ROW. Make it a QUIET best-effort: a brief settle lets Sony's
        // installer release the file (it can still hold it for a beat after the
        // title registers — the cause of the reported "Delete failed:
        // fs_delete_failed" toast), then a retrying delete; if it STILL fails,
        // we log and drop the row anyway rather than alarm the user mid-success
        // (the leftover is harmless staging that "Clear finished" sweeps).
        if (autoRemove) {
          log.info("install", `auto-deleting staged pkg after install: ${path}`);
          await sleep(800);
          try {
            await fsDeleteWithRetry(mgmtAddr(host), path);
          } catch (e) {
            log.info(
              "install",
              `post-install staged-pkg cleanup deferred (${pkgError(e)}): ${path}`,
            );
          }
          set({ entries: get().entries.filter((e) => e.path !== path) });
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
        // Surface failures in the bell too (success already notifies above).
        // Without this a failed item — an update or DLC especially — was silent
        // if the user navigated away from the Library tab mid-install.
        pushNotification(
          stalled ? "warning" : "error",
          `${label} install ${stalled ? "didn’t finish" : "failed"}`,
          { body: mainErr || "The PS5 didn’t confirm the install. Try again." },
        );
      }
    } catch (e) {
      patch({ status: "idle", lastResult: { ok: false, message: pkgError(e) } });
    } finally {
      set({ installing: false, busyNotice: null, installPending: false });
    }
  },

  async installAll(host) {
    if (!host?.trim()) return;
    // Don't start a batch on top of a single in-flight install (or another
    // batch) — the inner install() would early-return and we'd silently skip
    // rows. The button is disabled in this state too; this is the guard.
    if (get().installing || get().installingAll) return;

    // Snapshot the not-yet-installed, idle rows and order them base → update →
    // DLC so an add-on never installs before its base. `installedHere` is the
    // authoritative per-package signal (see pkgRowInstalled); a row mid-upload
    // or queued is skipped — installAll only drives ready staged packages.
    const targets = get()
      .entries.filter((e) => e.status === "idle" && !e.installedHere)
      .slice()
      .sort((a, b) => {
        const d = pkgEntryInstallOrder(a) - pkgEntryInstallOrder(b);
        // Stable within a tier: preserve listing order (base games in the
        // order they were staged), matching uploadQueue's install ordering.
        return d;
      });

    if (targets.length === 0) {
      pushNotification("info", "Nothing to install", {
        body: "Every staged package is already installed on this console.",
      });
      return;
    }

    set({ installingAll: true });
    let ok = 0;
    let failed = 0;
    try {
      for (const target of targets) {
        // Re-read the row: an earlier item in the batch (or an outside action)
        // may have changed its state. Skip if it's no longer an idle, not-yet-
        // installed row.
        const cur = get().entries.find((e) => e.path === target.path);
        if (!cur || cur.status !== "idle" || cur.installedHere) continue;
        // install() self-serializes on `installing` and awaits the full
        // readiness-gated cascade; it sets the row's lastResult. It never
        // throws (it catches internally), so a single failure can't abort the
        // batch — we just count the outcome and move on.
        await get().install(target.path, host);
        const after = get().entries.find((e) => e.path === target.path);
        if (after?.lastResult?.ok) ok++;
        else failed++;
      }
    } finally {
      set({ installingAll: false });
    }

    // One summary bell for the whole batch (each item's own success/failure
    // bell still fires inside install(), matching the single-install UX).
    pushNotification(
      failed === 0 ? "success" : "warning",
      failed === 0
        ? `Installed ${ok} package${ok === 1 ? "" : "s"}`
        : `Installed ${ok} of ${ok + failed}; ${failed} failed`,
      {
        body:
          failed === 0
            ? "All staged packages installed."
            : "Some packages didn't install — check the rows marked failed and retry them.",
      },
    );
  },

  async installStream(localPcPath, host) {
    if (!host?.trim()) {
      return { ok: false, message: "No PS5 host selected." };
    }
    if (get().installing) {
      return { ok: false, message: "Another install is in progress." };
    }
    set({ installing: true, busyNotice: null, installPending: false });
    const clearBusy = () =>
      set({ installing: false, busyNotice: null, installPending: false });
    try {
      // Wait behind any active transfer — the DPI payload swap would kill
      // the transfer port mid-upload, same as install()/installExternal().
      const transfersActive = () =>
        transferScreenBusy(host) ||
        get().entries.some(
          (e) => e.status === "uploading" || e.status === "queued",
        );
      if (transfersActive()) {
        set({
          installPending: true,
          busyNotice:
            "Waiting for the current upload to finish before installing…",
        });
        while (transfersActive()) {
          if (!get().installing) return { ok: false, message: "Cancelled." };
          await sleep(400);
        }
        set({ installPending: false, busyNotice: null });
      }
      set({ installPending: false });

      // 1. Parse the PC-side pkg header for content_id + category. The
      //    engine needs the content_id to canonicalise the pkg-host URL
      //    filename (Sony's installer cross-checks it against the header).
      let meta: SplitParseResponse;
      try {
        meta = (await invoke("pkg_metadata_split", {
          path: localPcPath,
        })) as SplitParseResponse;
      } catch (e) {
        return { ok: false, message: `Couldn't read .pkg header: ${pkgError(e)}` };
      }
      if ((meta.parts?.length ?? 1) > 1) {
        return {
          ok: false,
          message:
            "Split .pkg sets aren't supported by the streaming installer — pick the single lead .pkg.",
        };
      }
      const contentId = meta.head?.content_id ?? "";
      const label = meta.head?.title || contentId || basenameOf(localPcPath);

      set({
        busyNotice: `Stream-installing ${label} (beta) — the PS5 pulls the pkg directly over HTTP, no staging upload…`,
      });

      // 2. Register the session with the engine. Passing `localPs5Path:
      //    null` + a PC `path` makes the engine create a pkg-host serving
      //    session WITHOUT expecting a staged file on the PS5. The URL
      //    the engine builds is what the DPI daemon will fetch.
      const onStatus = (msg: string) => set({ busyNotice: msg });
      const startResp = (await invoke("pkg_install_start", {
        ps5Addr: mgmtAddr(host),
        path: localPcPath,
        splitRoot: null,
        packageTypeOverride: pkgTypeForCategory(meta.head?.category),
        localPs5Path: null,
        contentId: contentId || null,
        // No staging file is created, so deleteStaging is moot — pass
        // false so the engine doesn't record a staging_path to clean up.
        deleteStaging: false,
        // Serve-only: create the /pkg-host/ session but DON'T run the
        // in-process InstallByPackage. That call, handed our http:// URL,
        // hangs the FW<11 payload until its watchdog kills the helper — the
        // 3.3.25 "stream install crashed my PS5" bug. The DPI daemon does the
        // real install in step 3 (runDpiDirectInstall).
        serveOnly: true,
      })) as {
        err_code?: number;
        session_id?: string;
        err_message?: string;
        may_not_launch?: boolean;
      };

      const rc = (startResp.err_code ?? 0) >>> 0;
      const sessionId = startResp.session_id;
      // The engine creates a pkg-host session even when BGFT register
      // rejects (rc != 0) — but without a session_id there's nothing for
      // the daemon to fetch, so this is a hard fail.
      if (!sessionId) {
        return {
          ok: false,
          message:
            startResp.err_message ||
            `The engine wouldn't start a serving session (0x${rc.toString(16).padStart(8, "0")}).`,
        };
      }

      // 3. Hand the session's pkg-host URL to the DPI daemon. The daemon
      //    pulls the pkg over HTTP; no staging copy lands on the PS5.
      const dpi = await runDpiDirectInstall(host, sessionId, onStatus);
      if (dpi.daemonFailed) {
        return { ok: false, message: dpi.errMessage };
      }
      if (!dpi.ok) {
        return { ok: false, message: dpi.errMessage };
      }

      // 4. Verify the title actually landed (DPI's `ok` alone isn't
      //    proof — the daemon reports InstallByPackage's rc, not the
      //    async install result). The pkg-host session is still alive
      //    for this, then the engine GCs it.
      const verdict = await verifyInstallCompleted(sessionId);
      if (verdict.completed) {
        pushNotification("success", `Installed ${label}`, {
          body: "Stream-install complete. The pkg was fetched over HTTP — nothing was staged on the PS5.",
        });
        return { ok: true, mayNotLaunch: false };
      }
      // Stall / async failure. Nothing was staged on the PS5 so there's
      // no pkg to keep — the pkg-host session is engine-side only.
      return {
        ok: false,
        message: verdict.message || "The install didn't complete.",
      };
    } catch (e) {
      return { ok: false, message: pkgError(e) };
    } finally {
      clearBusy();
    }
  },

  async installExternal(pkg, host) {
    if (!host?.trim() || get().installing) {
      return { ok: false, message: "Another install is in progress." };
    }
    set({ installing: true, busyNotice: null, installPending: false });
    // The fast external scan often returns an EMPTY content id — it derives the
    // title id from the filename and skips the per-file header read. But Sony's
    // installer keys on the staged basename matching the content id, so staging
    // under a random fallback name gets rejected ("PKG header — wrongly named").
    // Read the real content id off the console first (best-effort) so the
    // staged copy is named `<ContentID>.pkg`, exactly like the upload flow.
    let contentId = pkg.contentId;
    if (!contentId) {
      const m = await pkgMetadataConsole(transferAddr(host), pkg.path);
      if (m?.contentId) contentId = m.contentId;
    }
    // Stage to internal with the Sony-friendly `<ContentID>.pkg` basename
    // (falls back to a unique name for headerless pkgs).
    const basename = stagingBasename(
      contentId,
      Math.random().toString(36).slice(2),
      Date.now(),
    );
    const internalPath = `${PKG_TEMP_DIR}/${basename}`;
    const label = pkg.name || contentId || "package";
    // Live install %, shared by the direct-from-USB and the copy-fallback paths.
    const onProgress = (installedBytes: number, total: number) => {
      if (total > 0) {
        const pct = Math.min(99, Math.floor((installedBytes / total) * 100));
        set({ busyNotice: `Installing ${label} from ${pkg.drive}… ${pct}%` });
      }
    };
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

      // Copy USB → internal, then install from there. We do NOT install
      // directly from the USB path: handing Sony's installer a `/mnt/usb…`
      // package registers it as a BGFT *download task* that streams the pkg off
      // USB at a crawl — a 25 GB game shows "Downloading… 50 hours left" and
      // leaves a broken, undeletable tile (HW-observed on Bloodborne, 3.3.4).
      // The internal copy is a TRANSIENT staging file (the USB original is
      // untouched) → clean it after.
      log.info(
        "install",
        `install-from-usb: copy→internal then install for ${pkg.path}`,
      );
      set({
        busyNotice: `Staging ${label} from ${pkg.drive} to internal storage — removed automatically after install…`,
      });
      await fsMkdir(transferAddr(host), PKG_TEMP_DIR).catch(() => {});
      // Trackable op_id → the (drop-tolerant) copy drives a live % bar, and a
      // dropped connection no longer aborts a healthy 25 GB copy.
      const copyOpId = Math.floor(Math.random() * 0xff_ffff_ffff) + 1;
      let copying = true;
      const pollCopy = (async () => {
        while (copying) {
          await sleep(1500);
          try {
            const s = await fsOpStatus(mgmtAddr(host), copyOpId);
            if (s.total_bytes > 0) {
              const pct = Math.min(
                99,
                Math.floor((s.bytes_copied / s.total_bytes) * 100),
              );
              set({
                busyNotice: `Staging ${label} from ${pkg.drive} to internal storage… ${pct}% (removed after install)`,
              });
            }
          } catch {
            /* op not yet registered or already finished — ignore */
          }
        }
      })();
      try {
        try {
          await fsCopy(mgmtAddr(host), pkg.path, internalPath, copyOpId);
        } catch (e) {
          if (/fs_copy_dest_exists/.test(String(e))) {
            await fsDelete(mgmtAddr(host), internalPath).catch(() => {});
            await fsCopy(mgmtAddr(host), pkg.path, internalPath, copyOpId);
          } else {
            throw e;
          }
        }
      } finally {
        // Signal the progress poller to stop; it exits on its next tick. Don't
        // await it (it may be mid-sleep) — it's a harmless detached no-op once
        // `copying` is false.
        copying = false;
        void pollCopy;
      }
      set({ busyNotice: null });

      const viaCopy = await runPkgInstall(
        host,
        internalPath,
        contentId || null,
        // External scan (USB/exFAT) carries no PARAM.SFO category, so the
        // package_type is unknown here. The engine reads the category straight
        // from the staged pkg to detect a patch and arm the data-loss guard.
        null,
        true, // the internal copy is transient — always clean it
        onProgress,
      );

      // Clean up the transient copy regardless of outcome.
      await fsDelete(mgmtAddr(host), internalPath).catch(() => {});

      return viaCopy.installed
        ? { ok: true, mayNotLaunch: viaCopy.mayNotLaunch }
        : { ok: false, message: viaCopy.errMessage || "Install was rejected." };
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
        // In-place install of a user-pointed path: we never parsed this pkg, so
        // the package_type is unknown. The engine reads the category from the
        // staged pkg itself to detect a patch and arm the data-loss guard.
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
      await fsDeleteWithRetry(mgmtAddr(host), path);
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

/** Per-session dedupe for on-console metadata enrichment, keyed by
 *  `host:path`, so repeated refreshes don't re-read the same staged pkg. */
const metaEnrichAttempted = new Set<string>();

/**
 * Fill in each staged pkg's authoritative version / category / title by
 * reading it off the console — for rows the upload-time cache didn't capture
 * (notably pkgs staged before that cache existed, which otherwise never show a
 * version). Reads one pkg at a time (gentle on the FS RPC), skips rows already
 * known or already tried this session, and bails the moment an install starts
 * (the payload is swapped then, so a read would be unsafe). Results are cached
 * by path so a later session — and `mergeListing` — keep them without re-reading.
 */
async function enrichStagedMetadata(
  get: () => PkgLibraryState,
  set: (
    partial:
      | Partial<PkgLibraryState>
      | ((s: PkgLibraryState) => Partial<PkgLibraryState>),
  ) => void,
  host: string,
): Promise<void> {
  if (!host?.trim()) return;
  // Snapshot the paths to enrich; the entries array is replaced as we go.
  const targets = get().entries.filter(
    (e) =>
      e.status === "idle" &&
      !e.appVer &&
      !metaEnrichAttempted.has(`${hostOf(host)}:${e.path}`),
  );
  for (const t of targets) {
    if (get().installing) return; // never read while the payload is swapped
    const key = `${hostOf(host)}:${t.path}`;
    metaEnrichAttempted.add(key);
    const m = await pkgMetadataConsole(transferAddr(host), t.path);
    if (!m || (!m.appVer && !m.category && !m.title)) continue;
    // Persist so a restart (and mergeListing) keep it without re-reading.
    cachePathMeta(t.path, {
      appVer: m.appVer || undefined,
      category: m.category || undefined,
    });
    set((s) => ({
      entries: s.entries.map((x) =>
        x.path === t.path
          ? {
              ...x,
              appVer: x.appVer || m.appVer || undefined,
              category: m.category || x.category,
              title: x.title || m.title || undefined,
            }
          : x,
      ),
    }));
  }
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
