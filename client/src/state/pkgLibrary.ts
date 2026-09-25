import { trStatic } from "../lib/trStatic";
import { isInstallPackagePath } from "../lib/pkgDropDedupe";
import { patchInstallFailure } from "../lib/dpiUnavailable";
import { restoreMainPayload } from "../lib/restoreMainPayload";
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
  pkgInstalledInventory,
  pkgInstallPreflight,
} from "../api/ps5";
import { processList, type ExternalPkg } from "../api/ps5";
import { formatBytes } from "../lib/format";
import {
  computeRate,
  pushRateSample,
  type RateSample,
} from "../lib/rollingRate";
import { hostOf, mgmtAddr, transferAddr } from "../lib/addr";
import { removableMountRoot } from "../lib/mountPaths";
import { humanizePs5Error } from "../lib/humanizeError";
import {
  stagingBasename,
  stagingDirectoryForPackage,
  categoryForSubdir,
  fingerprintFromStagingSubdir,
  isAddonCategory,
} from "../lib/pkgStagingPath";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { useInstallSettingsStore } from "./installSettings";
import { useConnectionStore } from "./connection";
import { log } from "./logs";
import { pushNotification } from "./notifications";
import { useActivityHistoryStore } from "./activityHistory";
import {
  useLinkInstallPrefs,
  type LinkInstallMode,
} from "./linkInstallPrefs";
import { useTaskStore } from "./tasks";
import { useToastStore } from "./toasts";
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

/** Sony accepted the request, but none of the signals we trust proved that the
 *  asynchronous install finished. This is deliberately a warning rather than
 *  success: callers must keep the source package and must not mark it installed. */
/** Toast copy for an accepted-but-unverified install (prefixed with the
 *  package name at the call site). */
export const PKG_INSTALL_UNVERIFIED_TOAST =
  "is still finishing on the PS5. Large games keep installing for a while after the transfer ends — ps5upload keeps checking, and the staged package is kept until it is confirmed.";

/** Shown when the background re-verify can never succeed for this package —
 *  no title id, and neither a fingerprint nor a size to match an installed
 *  artifact against. Saying "we keep checking" there would be a lie. */
export const PKG_REVERIFY_IMPOSSIBLE_HINT =
  "The PS5 accepted this install, but ps5upload has nothing to identify the installed package by, so it cannot confirm it. Check the PS5's Notifications for the result. The staged package was kept.";

/** Shown when the 30-minute automatic re-verify window closes without the
 *  package registering. The row stays open with a Recheck action. */
export const PKG_REVERIFY_GAVE_UP_HINT =
  "Still not confirmed after 30 minutes. A very large title can take longer — check the PS5's Notifications, or use Recheck to look again.";

export const PKG_ACCEPTED_UNVERIFIED_HINT =
  "The PS5 accepted the install request, but ps5upload couldn’t verify that installation completed. Check the PS5 home screen and Notifications / Downloads. The staged package was kept so you can retry, or use Settings → System → Debug Settings → Game → Package Installer.";

/** Shown while the app can't reach the engine that is tracking the install.
 *  Deliberately does NOT call the install failed: the engine owns the install
 *  state (and, for a Stream install, is the web server the console is pulling
 *  from), so an unreachable engine means the install is UNWATCHED, not broken. */
export const PKG_ENGINE_BLIND_HINT =
  "Lost contact with the ps5upload engine, which is the only thing that can report this install — it may still be running. Still watching; nothing has been cancelled.";

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

/** Which of the four install outcomes a result represents.
 *
 *  Pure so the mapping is testable without a console. The distinction that
 *  matters is `unverified` vs `failed`: the PS5 accepted the install and is
 *  very likely still working on it, so it must not be rendered as an error.
 *  See docs/superpowers/specs/2026-09-20-install-status-without-polling-design.md
 */
export function installOutcomeKind(r: {
  installed: boolean;
  acceptedUnverified?: boolean;
  stalled?: boolean;
}): "done" | "unverified" | "stalled" | "failed" {
  if (r.installed) return "done";
  if (r.acceptedUnverified) return "unverified";
  if (r.stalled) return "stalled";
  return "failed";
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
    : { ok: true, message: "Installed package verified on the console." };
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
  /** Absolute source path selected on the computer that uploaded this pkg.
   * Kept only in this computer's local cache; it is never written to the PS5.
   * Best-effort because another computer (or a cleared cache) cannot recover
   * the original location from the staged file. */
  sourcePath?: string;
  /** When the upload completed, as Unix milliseconds. For packages staged by
   * an older app/another computer this falls back to the PS5 file mtime. */
  uploadedAt?: number;
  /** PARAM.SFO `APP_VER` (e.g. `01.04`) — the authoritative package version,
   *  parsed at upload and cached (path → version). For a patch this is the
   *  definitive "which update is this", since updates share a ContentID and a
   *  title. Best-effort, same caveats as `originalName`. */
  appVer?: string;
  /** Stable identity for this exact pkg artifact (sampled BLAKE3). Unlike
   * ContentID/category/version it distinguishes repacks such as an optional
   * fix and a backport. Drives collision-free staging and exact installed
   * verification. */
  fingerprint?: string;
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
  /** Signing class proven by the outer package envelope. Currently available
   * for PS5 FIH packages; PS4 CNT packages remain unknown unless cryptographically
   * inspected rather than being guessed from their extension. */
  authenticity?: "fake_debug" | "retail" | "unknown";
  /** Problems the parser found in the package itself — e.g. a param.json DRM
   *  type the console will refuse to run. Advisory: shown on the row so the
   *  user learns before a long upload and install, never a reason to block. */
  warnings?: string[];
  /** Transient per-row state (never persisted; recomputed each session). */
  status: PkgStatus;
  /** Bytes transferred so far (upload) — drives the row progress bar. */
  bytes?: number;
  /** Total bytes for the active upload. */
  totalBytes?: number;
  /** Smoothed upload rate (bytes/sec) for the active upload, from the same
   *  trailing window the Upload queue uses. 0 until two samples exist. */
  bytesPerSec?: number;
  /** Outcome of the last install attempt this session, for inline feedback.
   *  `warn` renders amber for either a may-not-launch success or an accepted
   *  request whose asynchronous completion could not be verified. */
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
export function platformFromTitleId(
  titleId: string | null | undefined,
): string {
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
export function pkgEntryInstallOrder(
  e: Pick<PkgEntry, "category" | "path">,
): number {
  const cat = e.category;
  if (cat === "gp") return 1;
  if (cat === "ac") return 2;
  if (cat === "gd") return 0;
  if (/\/updates\//.test(e.path)) return 1;
  if (/\/dlc\//.test(e.path)) return 2;
  return 0;
}

/** Conflict key for mutually-exclusive package variants. Two updates for the
 * same title+version (or two DLC artifacts with the same ContentID) can both be
 * staged, but installing both automatically would just make the last one win. */
export function pkgAlternativeKey(
  e: Pick<PkgEntry, "category" | "titleId" | "contentId" | "appVer">,
): string | null {
  if (e.category === "gp") {
    const title = e.titleId || e.contentId;
    return title ? `patch:${title}:${e.appVer || "unknown"}` : null;
  }
  if (e.category === "ac" && e.contentId) {
    return `dlc:${e.contentId}`;
  }
  return null;
}

/** Stable value persisted as the user's per-console choice for an alternative
 * group. A sampled fingerprint survives path moves; legacy rows fall back to
 * their unique staged path. */
export function pkgEntryIdentity(
  e: Pick<PkgEntry, "fingerprint" | "path">,
): string {
  return e.fingerprint || e.path;
}

export type PkgAlternativeSelections = Record<string, string>;

/** Persisted marker for an explicit "leave this conflict group out" choice.
 * Fingerprints are hexadecimal and fallback identities are absolute PS5 paths,
 * so this value cannot collide with a real package identity. */
export const PKG_ALTERNATIVE_SKIP = "__ps5upload_skip__";

export interface PkgAlternativeGroup {
  key: string;
  entries: PkgEntry[];
}

/** Return only genuinely ambiguous groups (two or more exact artifacts).
 * Same-version patch variants and same-ContentID DLC repacks are alternatives;
 * different patch versions and independent DLC remain separate installables. */
export function pkgAlternativeGroups(
  entries: PkgEntry[],
): PkgAlternativeGroup[] {
  const grouped = new Map<string, PkgEntry[]>();
  for (const entry of entries) {
    const key = pkgAlternativeKey(entry);
    if (!key) continue;
    grouped.set(key, [...(grouped.get(key) ?? []), entry]);
  }
  return [...grouped.entries()]
    .filter(
      ([, group]) =>
        new Set(group.map((entry) => pkgEntryIdentity(entry))).size > 1,
    )
    .map(([key, group]) => ({ key, entries: group }));
}

export function pkgHasConflictingAlternative(
  entry: PkgEntry,
  entries: PkgEntry[],
): boolean {
  const key = pkgAlternativeKey(entry);
  if (!key) return false;
  return entries.some(
    (other) =>
      other.path !== entry.path &&
      pkgAlternativeKey(other) === key &&
      (other.fingerprint || other.path) !== (entry.fingerprint || entry.path),
  );
}

export interface PkgInstallAllPlan {
  targets: PkgEntry[];
  conflicts: PkgEntry[];
}

/** Build the safe Install-all plan. A valid per-console selection contributes
 * exactly one mutually-exclusive variant; unresolved groups remain staged and
 * are skipped. Independent DLC and different patch versions remain batchable. */
export function pkgInstallAllPlan(
  entries: PkgEntry[],
  selections: PkgAlternativeSelections = {},
): PkgInstallAllPlan {
  const staged = entries.filter((e) => e.status === "idle");
  const ready = staged.filter((e) => !e.installedHere);
  const variants = new Map<string, Set<string>>();
  // Include previously-installed sibling rows when deciding whether a ready
  // target is ambiguous. Otherwise: install OptionalFix once, add Backport,
  // then "Install all" would see only Backport as ready and silently replace
  // the active same-version patch—the exact last-one-wins bug this planner is
  // meant to prevent.
  for (const e of staged) {
    const key = pkgAlternativeKey(e);
    if (!key) continue;
    const identities = variants.get(key) ?? new Set<string>();
    identities.add(e.fingerprint || e.path);
    variants.set(key, identities);
  }
  const conflicts = ready.filter((e) => {
    const key = pkgAlternativeKey(e);
    if (!key || (variants.get(key)?.size ?? 0) <= 1) return false;
    const selected = selections[key];
    return !selected || !variants.get(key)?.has(selected);
  });
  const conflictPaths = new Set(conflicts.map((e) => e.path));
  const targets = ready
    .filter((e) => {
      if (conflictPaths.has(e.path)) return false;
      const key = pkgAlternativeKey(e);
      if (!key || (variants.get(key)?.size ?? 0) <= 1) return true;
      // A valid explicit choice resolves the group: install only that exact
      // artifact and leave every sibling safely staged.
      return pkgEntryIdentity(e) === selections[key];
    })
    .slice()
    .sort((a, b) => {
      const tier = pkgEntryInstallOrder(a) - pkgEntryInstallOrder(b);
      if (tier !== 0) return tier;
      if (a.category === "gp" && b.category === "gp") {
        return (a.appVer || "").localeCompare(b.appVer || "", undefined, {
          numeric: true,
        });
      }
      return 0;
    });
  return { targets, conflicts };
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
  entry: Pick<PkgEntry, "titleId" | "category" | "installedHere"> &
    Partial<Pick<PkgEntry, "size" | "fingerprint" | "contentId">>,
  installedTitleIds: Set<string>,
  artifacts?: Array<{
    kind: "base" | "patch" | "dlc";
    size: number;
    fingerprint: string;
    contentId: string;
  }>,
): boolean {
  const kind =
    entry.category === "gp"
      ? "patch"
      : entry.category === "ac"
        ? "dlc"
        : "base";
  if (artifacts) {
    const candidates = artifacts.filter((a) => a.kind === kind);
    const exact = candidates.some((a) => {
      if (entry.fingerprint) {
        const expected = entry.fingerprint.toLowerCase();
        const actual = a.fingerprint.toLowerCase();
        // A cold scan can initially recover only the 128-bit directory token;
        // metadata enrichment replaces it with the full sampled fingerprint.
        // Matching that bounded prefix is still exact to the identity encoded
        // in the staging path and avoids a false "not installed" flash.
        if (/^[a-f0-9]{32}$/.test(expected)) {
          return actual.startsWith(expected);
        }
        return actual === expected;
      }
      if ((entry.size ?? 0) > 0 && a.size !== entry.size) return false;
      if (kind === "dlc" && entry.contentId && a.contentId) {
        return a.contentId === entry.contentId;
      }
      return true;
    });
    if (exact) return true;
    // A successful live inventory is authoritative. In particular, a base
    // title plus a different patch variant must not be labelled Reinstall.
    if (isAddonCategory(entry.category)) return false;
  } else if (entry.installedHere) {
    // Transitional/legacy hint only while the live inventory is still loading.
    return true;
  }
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
  const lower = name.toLowerCase();
  if (lower.endsWith(".fpkg")) return name.slice(0, -5);
  return lower.endsWith(".pkg") ? name.slice(0, -4) : name;
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
// capture them at upload and cache here. Keyed by console + on-PS5 path rather
// than ContentID, because a base and its update share a ContentID and two
// consoles can have the same staged path but different upload provenance.
const PATH_META_CACHE_KEY = "ps5upload.pkg_library.pathmeta.v1";

/** Per-staged-path metadata we can't recover from a bare dir listing. */
interface PkgPathMeta {
  /** Original uploaded filename, e.g. `Jak X… [v01.04].pkg`. */
  name?: string;
  /** Original computer-side path. This stays in localStorage on the uploader
   * and is never copied into PS5-side metadata. */
  sourcePath?: string;
  /** Upload completion time in Unix milliseconds. */
  uploadedAt?: number;
  /** PARAM.SFO `APP_VER`, e.g. `01.04` — the authoritative package version. */
  appVer?: string;
  /** Sampled BLAKE3 package identity; see `PkgEntry.fingerprint`. */
  fingerprint?: string;
  /** PARAM.SFO `CATEGORY` (`gd`/`gp`/`ac`) — authoritative, vs. the directory
   *  inference. Populated when we read the staged pkg off the console. */
  category?: string;
  /** Signing class captured while the computer-side package was parsed. */
  authenticity?: "fake_debug" | "retail" | "unknown";
  /** Parser warnings captured at the same time, so the row keeps showing them
   *  after a refresh-from-disk (the console-side pkg is never re-parsed). */
  warnings?: string[];
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

function pathMetaCacheKey(host: string, path: string): string {
  return `${hostOf(host)}\u0000${path}`;
}

function pathMetaFor(
  cache: Record<string, PkgPathMeta>,
  host: string,
  path: string,
): PkgPathMeta | undefined {
  // Fall back to the pre-5.1.8 path-only entry so existing filename/version
  // metadata is retained while all new writes remain console-specific.
  return cache[pathMetaCacheKey(host, path)] ?? cache[path];
}

function cachePathMeta(host: string, path: string, meta: PkgPathMeta): void {
  if (!host?.trim() || !path) return;
  try {
    const c = loadPathMetaCache();
    // Merge the legacy path-only row too. Otherwise the first partial 5.1.8
    // write (for example a newly discovered fingerprint) would create the
    // console-scoped key and accidentally shadow the old filename/version.
    const key = pathMetaCacheKey(host, path);
    c[key] = { ...c[path], ...c[key], ...meta };
    safeSetItem(PATH_META_CACHE_KEY, JSON.stringify(c));
  } catch {
    /* best-effort */
  }
}

// Per-console choices for mutually-exclusive variants. Patch alternatives
// with the same title/version can coexist in staging but only one can be
// active, so Install all needs an explicit intent rather than relying on list
// order. Scope by bare host because a Pro and a Phat may deliberately use
// different backports for the same title.
const ALTERNATIVE_SELECTIONS_CACHE_KEY =
  "ps5upload.pkg_library.alternative_selections.v1";

function loadAllAlternativeSelections(): Record<
  string,
  PkgAlternativeSelections
> {
  if (typeof window === "undefined") return {};
  try {
    const raw = safeGetItem(ALTERNATIVE_SELECTIONS_CACHE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

export function loadPkgAlternativeSelections(
  host: string,
): PkgAlternativeSelections {
  return { ...(loadAllAlternativeSelections()[hostOf(host)] ?? {}) };
}

export function recordPkgAlternativeSelection(
  host: string,
  key: string,
  identity: string,
): void {
  if (!host?.trim() || !key || !identity || typeof window === "undefined")
    return;
  try {
    const all = loadAllAlternativeSelections();
    const consoleSelections = { ...(all[hostOf(host)] ?? {}) };
    consoleSelections[key] = identity;
    all[hostOf(host)] = consoleSelections;
    safeSetItem(ALTERNATIVE_SELECTIONS_CACHE_KEY, JSON.stringify(all));
  } catch {
    /* best-effort */
  }
}

/** Remember an explicit opt-out. Deleting the key is not enough: the Install
 * Package screen safely auto-detects the currently installed artifact when no
 * choice exists, which would immediately select the checkbox again. */
export function skipPkgAlternativeSelection(host: string, key: string): void {
  if (!host?.trim() || !key || typeof window === "undefined") return;
  try {
    const all = loadAllAlternativeSelections();
    const consoleSelections = { ...(all[hostOf(host)] ?? {}) };
    consoleSelections[key] = PKG_ALTERNATIVE_SKIP;
    all[hostOf(host)] = consoleSelections;
    safeSetItem(ALTERNATIVE_SELECTIONS_CACHE_KEY, JSON.stringify(all));
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
export function recordPkgInstalled(
  host: string,
  path: string,
  replacedAlternativePaths: string[] = [],
): void {
  if (!path || typeof window === "undefined") return;
  try {
    const h = hostOf(host);
    const c = loadInstalledCache();
    const set = new Set(c[h] ?? []);
    for (const replaced of replacedAlternativePaths) set.delete(replaced);
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
    /** Sampled BLAKE3 identity returned by the package parser. */
    fingerprint?: string;
    /** Target platform for badging: "ps4" | "ps5" | "" (unknown). Derived
     *  engine-side from the header magic + title-id prefix. */
    platform?: string;
    /** BGFT type already resolved by the engine (PS4GD/PS4DP/PS5GD/PS5DP). */
    package_type?: string;
    /** Package-envelope signing class (`fake_debug`, `retail`, or `unknown`). */
    authenticity?: "fake_debug" | "retail" | "unknown";
    warnings?: string[];
  };
}

export interface PkgUploadOptions {
  /** Override the global auto-install preference for this upload. Stream's
   * explicit fallback uses true because the user already asked to install. */
  installAfterUpload?: boolean;
  /** Treat this exact variant as an explicit choice even when an alternative
   * with the same title/version is already staged. */
  selectVariant?: boolean;
}

function pkgError(e: unknown): string {
  return humanizePs5Error(e instanceof Error ? e.message : String(e));
}

/** Where a stream install gets its bytes. A local `.pkg` is read off this
 *  computer's disk; a link is fetched from its origin by the engine (several
 *  connections at once) and re-served to the console. Everything downstream —
 *  the DPI hand-off, the transfer tracking, the completion check — is the same
 *  for both, which is why they share one code path. */
/** A package file on an SMB share, streamed straight into the installer. */
export interface SmbStreamSource {
  server: string;
  share: string;
  user: string;
  password: string;
  /** Path of the .pkg within the share. */
  path: string;
  /** Size from the share listing, for progress before the engine reports it. */
  size?: number;
}

export type StreamInstallSource =
  | string
  | { remoteUrl: string }
  | { smb: SmbStreamSource };

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
  addAndUpload: (
    localPath: string,
    host: string,
    options?: PkgUploadOptions,
  ) => Promise<void>;
  install: (path: string, host: string) => Promise<void>;
  /** Stream-install (beta, #81) a local PC-side `.pkg` WITHOUT uploading it
   *  to PS5 staging first. The engine serves the file over HTTP at
   *  `/pkg-host/{session}/` and the DPI daemon pulls it directly. Saves the
   *  staging upload (and the disk space) for the quick-install case. The
   *  pkg is NOT retained on the PS5 afterwards — nothing was staged.
   *  Shares the `installing` lock with `install()`.
   *  Returns `{ ok, message?, mayNotLaunch? }`. */
  installStream: (
    source: StreamInstallSource,
    host: string,
  ) => Promise<{
    ok: boolean;
    message?: string;
    mayNotLaunch?: boolean;
    acceptedUnverified?: boolean;
    /** True when the same package should be retried through staged/file mode,
     * which bypasses Sony's HTTP/proxy path. */
    stagedFallbackRecommended?: boolean;
    /** Sony/AppInstUtil return code from the DPI daemon, when available. */
    rc?: number;
    /** Number of HTTP requests that reached ps5upload's pkg-host listener. */
    requestsServed?: number;
  }>;
  /** Install a package straight from an HTTP(S) link. The engine fetches it
   * from the origin over several connections at once and re-serves it to the
   * console on the LAN, so the download runs at the PC's line speed instead
   * of Sony's single-stream rate, and nothing is staged on either machine.
   * Delegates to `installStream`, so it shares the `installing` lock and the
   * same hand-off + verification. */
  installUrl: (
    url: string,
    host: string,
    opts?: { mode?: LinkInstallMode },
  ) => ReturnType<PkgLibraryState["installStream"]>;
  /** Download a link to this computer's disk, then install the local file.
   *
   * The slow leg finishes before the console is involved, so an expiring
   * link, a dropped connection or a sleeping machine costs a retry of the
   * download rather than the whole install. Needs room for the package.
   * Delegates to `installStream` for the install itself, so it shares the
   * `installing` lock and the same hand-off and verification. */
  installDownloadedLink: (
    url: string,
    host: string,
    insecureTls: boolean,
  ) => ReturnType<PkgLibraryState["installStream"]>;
  /** Install every staged, not-yet-installed, idle row sequentially, in
   *  base → update → DLC order (`pkgEntryInstallOrder`). Each item runs the
   *  full readiness-gated `install()` cascade; one failure doesn't abort the
   *  rest. Ends with a single summary bell. No-op if a single install is
   *  already running. */
  installAll: (
    host: string,
    selections?: PkgAlternativeSelections,
    /** Optional live-inventory verdict from the UI. When supplied it replaces
     * legacy installedHere hints, which can be stale after one patch variant
     * replaces another. */
    installedPaths?: string[],
  ) => Promise<void>;
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
  ) => Promise<{
    ok: boolean;
    message?: string;
    mayNotLaunch?: boolean;
    acceptedUnverified?: boolean;
  }>;
  /** Install a `.pkg` the user found while browsing the File System, at an
   *  arbitrary on-console path. Removable mounts (`/mnt/usb*`, `/mnt/ext*`)
   *  route through the copy-to-internal `installExternal` flow (Sony can't
   *  read exfat directly); anything already on internal storage installs in
   *  place with no copy. Lets users install straight from where a package
   *  sits instead of going through the External Packages scan. */
  installFromConsolePath: (
    path: string,
    host: string,
  ) => Promise<{
    ok: boolean;
    message?: string;
    mayNotLaunch?: boolean;
    acceptedUnverified?: boolean;
  }>;
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
  const label = (e: PkgEntry) =>
    (e.title || e.contentId || e.name).toLowerCase();
  return [...active, ...merged].sort((a, b) =>
    label(a).localeCompare(label(b)),
  );
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
   *  progress tracker confirmed the title actually landed. This is
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
  /** Sony accepted the request, but completion could not be proven. This is a
   *  terminal warning, never success, and the staged package must be kept. */
  acceptedUnverified?: boolean;
  /** The package_type the engine actually resolved the install to, from the
   *  staged pkg's own PARAM.SFO when the caller sent none. Authoritative over
   *  the caller's own `packageType` argument, which is frequently null (e.g.
   *  the upload-queue path never carries a category) — any later re-verify
   *  against the wrong (default "gd"/base) category would filter out the
   *  real DLC/patch artifact and either strand the row forever or, worse,
   *  match an unrelated already-installed base of the same size. */
  resolvedPackageType?: string;
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
/** How long an install may stay UNWATCHED because the engine is unreachable
 *  before we stop claiming to watch it. Generous on purpose: an engine restart
 *  is seconds, and a healthy install must not be written off for one. */
const PKG_VERIFY_ENGINE_BLIND_MS = 5 * 60 * 1000;

/**
 * Tell the two shapes of a `pkg_install_status` failure apart, because only
 * one of them is bad news.
 *
 * The engine owns install state, and (for a Stream install) is also the HTTP
 * server the console pulls the package from — so it is the only thing that can
 * report on an install, and the only thing whose absence proves nothing about
 * it. An unreachable ENGINE therefore means "state unknown, keep watching";
 * an engine that answers "no install session <id>" means the state is gone for
 * good, since sessions live in engine memory and die with the process. Both
 * arrive here as strings, which is why this is a string match rather than a
 * typed error — and why both shapes are listed: the desktop app's errors come
 * from `get_json` in ps5_engine.rs ("engine request failed: …"), while the
 * browser build's come straight from `fetch` (Chrome "Failed to fetch",
 * Safari "Load failed", an AbortSignal timeout, a refused socket). A message
 * that matches neither is classified as a real error and burns the retry
 * budget, which is the safe direction: that budget only ends the watch.
 */
function classifyStatusPollError(
  raw: string,
): "engine-unreachable" | "session-gone" | "other" {
  // Checked first: a 404 body is the engine answering, whatever else it says.
  if (/no install session/i.test(raw)) return "session-gone";
  if (
    /engine request failed|failed to fetch|fetch failed|networkerror|load failed|signal timed out|timeouterror|econnrefused|connection refused/i.test(
      raw,
    )
  ) {
    return "engine-unreachable";
  }
  return "other";
}

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
/** SCE_HTTP_ERROR_PROXY — Sony's installer rejected its HTTP setup before it
 * requested the package. Staged/file install bypasses this path entirely. */
const DPI_HTTP_PROXY_RC = 0x80431084;

/** `http://ip:port` of a pkg-host URL, or null when there is none to show. */
export function originOf(url: string | undefined): string | null {
  if (!url) return null;
  try {
    return new URL(url).origin;
  } catch {
    return null;
  }
}

/**
 * A stream the PS5 never fetched a byte of. With no proxy error this is the
 * console failing to reach this computer at all (#327: 0x80431068 on a
 * network with no proxy), so the proxy is the last thing to check, not the
 * only one: a firewall blocking the engine's port inbound is the usual cause.
 */
export function streamUnreachableMessage(rcHex: string, servedFrom: string | null): string {
  const where = servedFrom ? ` at ${servedFrom}` : "";
  return (
    `The PS5 never reached this computer${where} to fetch the package (${rcHex}). ` +
    "Allow ps5upload through this computer's firewall (on Windows, for both Private and Public networks), " +
    "keep the computer and the PS5 on the same network with any VPN off, and set the PS5's Proxy Server to “Do Not Use”. " +
    "Upload & install works without this connection."
  );
}
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
/** DPI's synchronous rc=0 is only acceptance. Once the main payload is
 * restored, allow enough time for its live installed-artifact inventory to
 * observe the exact patch/DLC/base that landed. */
/** How long the post-install check waits with NO sign of the install writing
 *  before giving up. The window restarts on any growth, so this bounds a
 *  *stalled* install, never a slow one. */
const DPI_VERIFY_IDLE_MS = 3 * 60 * 1000;
/** Absolute ceiling for the post-install check, so a console that writes
 *  forever cannot pin the UI open indefinitely. Sized for a ~200-300 GB title
 *  on a slow internal copy. */
const DPI_VERIFY_MAX_MS = 4 * 60 * 60 * 1000;

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
/** Shown when the update never reached the PS5 at all because the DPI
 *  daemon — the only install path that can apply a patch once the
 *  in-process installer hits the firmware authid gate — could not be
 *  brought up. Distinct from `PKG_PATCH_REJECTED_HINT` on purpose: saying
 *  "the PS5 declined it" when the console never saw the request sends
 *  people hunting for the wrong base-game version.
 *
 *  Which of the three causes it was — no image in this build, the console's
 *  loader not answering on :9021, or the daemon never coming up on :9040 —
 *  is decided by `dpiUnavailableCopy` from the engine's machine-readable
 *  reason code. Re-exported here so existing importers keep working.
 *  See `lib/dpiUnavailable.ts` for why one message for all three was wrong. */
export {
  PKG_PATCH_DAEMON_NO_BRINGUP_HINT,
  PKG_PATCH_DAEMON_UNAVAILABLE_HINT,
  PKG_PATCH_LOADER_UNREACHABLE_HINT,
} from "../lib/dpiUnavailable";

/** Shown when an update was accepted and then silently discarded. The
 *  workaround is not guessable, so it has to be in the message. */
export const PKG_PATCH_DID_NOT_APPLY_HINT =
  "The PS5 accepted this update and then did nothing with it — the game is still on its previous version. This means the console could not match the update to the base game you have installed. Re-install the base game through ps5upload from the base package that goes with this update (choose Override), then apply the update again.";

/** Shown when re-applying an already-installed update removed it. */
export const PKG_PATCH_REGRESSED_HINT =
  "Re-applying this update removed it — the game has gone back to its base version. The PS5's installer treats a re-applied update as one to undo. Apply the update once more to return to the updated version, and avoid re-installing an update the game already has.";

export const PKG_PATCH_REJECTED_HINT =
  "This update couldn’t be applied. The PS5 itself declined it — most often because the update doesn’t match your installed version of the game, or the base game isn’t installed yet. Your base game is untouched. Check that this update is meant for the version you have installed, and that the base game is installed first.";

/**
 * One observation of an in-flight install, as the engine's status poll reports
 * it. `phase` is the engine's own state for the session:
 *
 *   `queued`   — the console hasn't asked for the package yet (Stream: nothing
 *                fetched; staged: BGFT registered but not downloading)
 *   `download` — bytes are moving. For a Stream install this is the console
 *                pulling from this computer, and `transferBytes` tracks it.
 *   `install`  — everything is transferred; Sony is writing to disk
 *   `done`     — verified complete
 *   `error`    — terminal failure, `stalled` distinguishes a flatline
 *
 * Exported so the install UI can name the state rather than guess at it.
 */
export interface InstallSample {
  phase: string;
  /** Bytes observed landing on the console (free-space drop / title-dir size). */
  installedBytes: number;
  /** Bytes the console has fetched from us. Stream installs only; 0 when staged. */
  transferBytes: number;
  total: number;
  /** `pkg-host` responses answered. 0 = the console never fetched anything. */
  servedRequests: number;
  /** Average bytes/sec on the DOWNLOAD leg of a link install (origin → this
   *  computer). Undefined for a staged install, which has no origin. Shown
   *  next to the console leg so a slow install says WHICH side is slow: the
   *  two look identical otherwise and need opposite fixes. */
  originRateBps?: number;
  stalled: boolean;
  acceptedUnverified: boolean;
  /** Replaces the derived line when set — used to say why the numbers above
   *  are no longer updating (e.g. the engine went away). */
  note?: string;
}

/** Seconds remaining as a short human string. Returns "" when there is no
 *  usable estimate, so callers can append it unconditionally. */
export function fmtEta(remainingBytes: number, bytesPerSec: number): string {
  if (!(bytesPerSec > 0) || !(remainingBytes > 0)) return "";
  const secs = remainingBytes / bytesPerSec;
  if (!Number.isFinite(secs)) return "";
  if (secs < 90) return `${Math.max(1, Math.round(secs))}s left`;
  if (secs < 5400) return `${Math.round(secs / 60)}m left`;
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  return m > 0 ? `${h}h ${m}m left` : `${h}h left`;
}

/** Bytes as a short human string: MB below a GB, GB above. */
function fmtBytes(n: number): string {
  if (n >= 1024 * 1024 * 1024)
    return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(0)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

/**
 * Name the state a live install is in, and where its progress bar should point.
 *
 * The engine reports `queued` → `download` → `install`; the client used to
 * collapse all three into one static "Installing…" string, so a Stream install
 * looked identical whether the console had not started, was pulling gigabytes at
 * 90 MB/s, or was finished transferring and writing. Exported for unit testing.
 *
 * Progress is `max(transferBytes, installedBytes)` rather than either alone:
 * both are lower bounds on the same package measured against the same total, so
 * the max never runs backwards across the download→install handover.
 */
/** The download leg of "Download through this computer": how far, how fast, how long. */
export function describeLinkDownload(
  written: number,
  total: number,
  bytesPerSec: number,
): string {
  const speed = bytesPerSec > 0 ? ` at ${fmtBytes(bytesPerSec)}/s` : "";
  if (total <= 0) {
    return `Downloading to this computer — ${fmtBytes(written)}${speed}`;
  }
  const pct = Math.min(100, Math.floor((100 * written) / total));
  const etaText = fmtEta(Math.max(0, total - written), bytesPerSec);
  return (
    `Downloading to this computer — ${pct}% (${fmtBytes(written)} of ${fmtBytes(total)})` +
    speed +
    (etaText ? ` · ${etaText}` : "")
  );
}

export function describeInstallSample(
  s: InstallSample,
  bytesPerSec = 0,
): { detail: string; current: number; pct: number } {
  const current = Math.max(s.transferBytes, s.installedBytes);
  const pct =
    s.total > 0 ? Math.min(100, Math.floor((100 * current) / s.total)) : 0;
  const speed = bytesPerSec > 0 ? ` at ${fmtBytes(bytesPerSec)}/s` : "";
  const etaText = fmtEta(Math.max(0, s.total - current), bytesPerSec);
  const eta = etaText ? ` · ${etaText}` : "";
  let detail: string;
  if (s.note) {
    // A caller-supplied explanation wins: the numbers below it are the last
    // ones we could read, and the note says why they aren't moving.
    return { detail: s.note, current, pct };
  }
  switch (s.phase) {
    case "queued":
      detail = "Waiting for the PS5 to start…";
      break;
    case "download": {
      // For a link install, name both legs. The console leg alone cannot
      // explain a slow install: the download from the origin feeds it, and
      // when that is the constraint the PS5 figure just mirrors it.
      const legs =
        s.originRateBps && s.originRateBps > 0
          ? ` · downloading ${fmtBytes(s.originRateBps)}/s` +
            (bytesPerSec > 0 ? `, sending ${fmtBytes(bytesPerSec)}/s` : "")
          : speed;
      detail =
        `Streaming to the PS5 — ${pct}% (${fmtBytes(current)} of ${fmtBytes(s.total)})${legs}` +
        eta;
      break;
    }
    case "install":
      // Show the same bytes/rate/ETA the transfer phase does. This phase is
      // the longest part of a big install and used to be a bare percentage.
      detail =
        `The PS5 is installing the package — ${pct}%` +
        (s.total > 0 ? ` (${fmtBytes(current)} of ${fmtBytes(s.total)})` : "") +
        speed +
        eta;
      break;
    default:
      detail = `Installing on the PS5… ${pct}%`;
      break;
  }
  return { detail, current, pct };
}

/** What the install tracker concludes. */
interface VerifyOutcome {
  /** Confirmed complete — the title registered (or byte-settled on
   *  unverifiable FW). The ONLY state that permits deleting the staged pkg. */
  completed: boolean;
  /** Terminal failure or stall — `completed` is false and the pkg is KEPT. */
  failed: boolean;
  /** Specifically a stall (flatlined), as opposed to a Sony-reported error. */
  stalled: boolean;
  /** The request was accepted but completion could not be proven. */
  acceptedUnverified: boolean;
  message: string;
  launchable?: boolean | null;
  /**
   * Whether the engine reported a terminal state. `false` means we stopped
   * watching while the install may still be running (safety cap, poll errors,
   * an engine too old to answer) — the caller must then NOT tear down the
   * serving session, because doing so aborts a live install.
   */
  trackedToTerminal: boolean;
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
 * `onState(sample)` is called each poll so callers can render a live % and name
 * the phase. A Stream install spends most of its wall clock in the transfer, so
 * `transferBytes` (not `installedBytes`) is what moves then.
 */
async function verifyInstallCompleted(
  session: string | undefined,
  onState?: (sample: InstallSample) => void,
): Promise<VerifyOutcome> {
  // `trackedToTerminal` is false for every give-up path: the engine never said
  // the install ended, so it may still be running and reading from the session.
  const unverified = (trackedToTerminal = false): VerifyOutcome => ({
    completed: false,
    failed: false,
    stalled: false,
    acceptedUnverified: true,
    message: PKG_ACCEPTED_UNVERIFIED_HINT,
    trackedToTerminal,
  });
  // No session id ⇒ older engine (or a test harness) that can't report status.
  // Acceptance without a status session is not proof of completion.
  if (!session) return unverified();
  // Backstop only — the ENGINE decides when an install has genuinely stalled
  // (it watches real disk progress and reports `stalled`). This deadline exists
  // so a forgotten session can't be watched forever, so it must be measured
  // from the last sign of progress rather than from the start: a 200-300 GB
  // title on a modest link legitimately runs past any fixed clock, and a fixed
  // one would stop watching a perfectly healthy install and report it
  // unverified.
  let safetyDeadline = Date.now() + PKG_VERIFY_SAFETY_CAP_MS;
  let bestProgress = 0;
  let pollErrors = 0;
  // When the engine stopped answering, and the last state we managed to read.
  // Both only matter while it is unreachable; see the catch below.
  let engineBlindSince = 0;
  let lastSample: InstallSample | null = null;
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
        accepted_unverified?: boolean;
        // Transfer counters (engine ≥ this release). Absent on older engines,
        // so the stream UI falls back to the phase alone.
        transfer_bytes?: number;
        served_requests?: number;
        origin_rate_bps?: number;
      };
      // An engine that doesn't speak status returns no `phase` — treat as
      // accepted-but-unverified, not as a manufactured success.
      if (typeof s?.phase !== "string") return unverified();
      // The engine answered, so whatever outage was in progress is over.
      engineBlindSince = 0;
      if (
        typeof s.installed_bytes === "number" &&
        typeof s.total === "number"
      ) {
        lastSample = {
          phase: s.phase,
          installedBytes: s.installed_bytes,
          transferBytes: s.transfer_bytes ?? 0,
          total: s.total,
          servedRequests: s.served_requests ?? 0,
          originRateBps:
            typeof s.origin_rate_bps === "number" ? s.origin_rate_bps : undefined,
          stalled: !!s.stalled,
          acceptedUnverified: !!s.accepted_unverified,
        };
        onState?.(lastSample);
        // Any forward movement — bytes landing on the console, or ranges
        // being pulled from us — restarts the watch window.
        const progressed = Math.max(
          lastSample.installedBytes,
          lastSample.transferBytes,
        );
        if (progressed > bestProgress) {
          bestProgress = progressed;
          safetyDeadline = Date.now() + PKG_VERIFY_SAFETY_CAP_MS;
        }
      }
      if (s.phase === "error") {
        // Distinguish a stall (flatlined, pkg kept, retry) from a Sony-reported
        // async failure — both are terminal and both KEEP the pkg.
        if (s.stalled) {
          return {
            completed: false,
            failed: true,
            stalled: true,
            acceptedUnverified: false,
            message: PKG_STALL_HINT,
            launchable: s.launchable,
            trackedToTerminal: true,
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
          acceptedUnverified: false,
          message: sony
            ? `${sony} — ${PKG_ASYNC_FAILED_HINT}`
            : PKG_ASYNC_FAILED_HINT,
          launchable: s.launchable,
          trackedToTerminal: true,
        };
      }
      if (s.phase === "done") {
        if (s.accepted_unverified) {
          // The engine reported a terminal state (it stopped tracking); this is
          // the one unverified outcome that IS terminal.
          return {
            ...unverified(true),
            launchable: s.launchable,
          };
        }
        // Confirmed terminal success — the engine only reports `done` once the
        // title actually registered (or byte-settled). Safe to delete.
        return {
          completed: true,
          failed: false,
          stalled: false,
          acceptedUnverified: false,
          message: "",
          launchable: s.launchable,
          trackedToTerminal: true,
        };
      }
      pollErrors = 0; // a clean in-progress poll resets the error streak
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      const kind = classifyStatusPollError(raw);
      // The ENGINE is the holder of install state — and for a Stream install
      // it is also the web server the console is pulling from. So an engine
      // that doesn't answer says nothing about the install: it is UNWATCHED,
      // and it is most likely still running fine. Treating this like any
      // other poll error used to abandon the watch after 5 polls (~12 s) —
      // shorter than a desktop engine restart — and report "couldn't verify"
      // for an install that then completed perfectly. Keep watching instead,
      // and say so, but only for so long: past the blind cap, holding the
      // install "in progress" in the UI forever is worse than admitting we
      // lost sight of it (the session is kept either way, so nothing that may
      // still be running gets cancelled).
      if (kind === "engine-unreachable") {
        if (!engineBlindSince) engineBlindSince = Date.now();
        if (lastSample)
          onState?.({ ...lastSample, note: PKG_ENGINE_BLIND_HINT });
        if (Date.now() - engineBlindSince >= PKG_VERIFY_ENGINE_BLIND_MS) {
          return { ...unverified(), message: PKG_ENGINE_BLIND_HINT };
        }
        continue;
      }
      // The engine answered and doesn't have this session. Sessions live in
      // engine memory, so this state is gone for good — polling on cannot
      // learn anything more, and neither can the caller cancel it.
      if (kind === "session-gone") return unverified();
      // Anything else: a real, repeating error. After a few, give up tracking
      // — but do NOT assume success. `completed:false` keeps the pkg (never
      // delete on uncertainty); the install may well have finished, so don't
      // shout error.
      if (++pollErrors >= PKG_VERIFY_MAX_POLL_ERRORS) {
        return unverified();
      }
    }
  }
  // Safety cap hit (pathological): keep the pkg, don't claim success.
  return unverified();
}

/** Confirm a DPI fallback by comparing the installed app.pkg/patch.pkg/DLC
 * artifact to the exact source identity. This closes the old gap where DPI
 * could apply a patch successfully but the UI still said "couldn't verify" —
 * or, worse, a pre-existing base/different patch could be mistaken for it. */
/** Exported for tests: the progress-aware post-install check. */
export async function verifyDpiInstalledArtifact(
  host: string,
  contentId: string | null,
  packageType: string,
  expected: PkgExpectedIdentity | undefined,
  onStatus?: (msg: string) => void,
  /** Deadline overrides. The defaults implement the growth-watching wait used
   *  right after an install (idle 3 min, ceiling 4 h). A caller that only
   *  wants a CHEAP PROBE — "is it registered right now?" — passes seconds
   *  here: the background re-verify does, because a probe that can occupy
   *  four hours at ~24 requests/min against the console's transfer port makes
   *  its own backoff schedule decorative and starves transfers. */
  opts?: { idleMs?: number; maxMs?: number },
): Promise<boolean> {
  const titleId = titleIdFromContentId(contentId || "");
  // Same predicate the re-verify scheduler checks up front, so the two can
  // never disagree about whether a probe is even possible.
  if (!titleId || !installReverifyProbeViable(contentId, expected)) return false;
  const category = packageType.endsWith("DP")
    ? "gp"
    : packageType.endsWith("AC")
      ? "ac"
      : "gd";
  // Wait on PROGRESS, not on a fixed clock. A flat 3-minute cap made this
  // size-dependent in exactly the way users reported: a small package finished
  // inside the window, while a large one was still being written when we gave
  // up, so ps5upload showed an error for an install the PS5 went on to
  // complete — and, because an unverified install must keep its staged copy,
  // left a package the user then had to delete by hand.
  //
  // Sony writes the title's files as it installs, so a growing artifact is
  // proof the install is alive. While it grows we keep waiting; we only stop
  // after the install has been completely still for `DPI_VERIFY_IDLE_MS`, or
  // at an absolute ceiling that a genuinely huge install should never reach.
  const startedAt = Date.now();
  const idleMs = opts?.idleMs ?? DPI_VERIFY_IDLE_MS;
  const hardDeadline = startedAt + (opts?.maxMs ?? DPI_VERIFY_MAX_MS);
  let idleDeadline = startedAt + idleMs;
  let bestSeenBytes = 0;
  let announcedProgress = false;
  onStatus?.("Verifying the exact installed package on the PS5…");
  while (Date.now() < idleDeadline && Date.now() < hardDeadline) {
    try {
      const artifacts = await pkgInstalledInventory(
        transferAddr(host),
        titleId,
      );
      if (
        pkgRowInstalled(
          {
            titleId,
            category,
            contentId: contentId || undefined,
            size: expected?.size,
            fingerprint: expected?.fingerprint,
          },
          new Set<string>(),
          artifacts,
        )
      ) {
        return true;
      }
      // Not the finished article yet — but is it being written? Any growth in
      // this title's files resets the idle window, so a slow install is waited
      // out instead of being called a failure.
      const seenBytes = artifacts.reduce((sum, a) => sum + (a.size || 0), 0);
      if (seenBytes > bestSeenBytes) {
        bestSeenBytes = seenBytes;
        idleDeadline = Date.now() + idleMs;
        if (expected?.size && expected.size > 0) {
          const pct = Math.min(
            99,
            Math.floor((seenBytes / expected.size) * 100),
          );
          onStatus?.(
            `The PS5 is still writing ${titleId} (${pct}%) — waiting for it to finish…`,
          );
        } else {
          onStatus?.(`The PS5 is still writing ${titleId} — waiting…`);
        }
        announcedProgress = true;
      }
    } catch {
      // The main payload is still restarting after DPI replaced it. Retry until
      // it is reachable; a transient restore gap is expected, not a failure.
      // It also must not burn the idle window — we cannot see progress while
      // the payload is down, so treat the blind period as neutral.
      idleDeadline = Math.max(idleDeadline, Date.now() + idleMs);
    }
    await sleep(PKG_VERIFY_POLL_MS);
  }
  if (announcedProgress) {
    log.info(
      "install",
      `verify gave up after ${Math.round((Date.now() - startedAt) / 1000)}s ` +
        `with ${bestSeenBytes} bytes written for ${titleId}`,
    );
  }
  return false;
}

/** Everything that must happen when an install is CONFIRMED, wherever the
 *  confirmation arrives from.
 *
 *  Two paths reach it: the Library's install action, and the background
 *  re-verify when a slow install finally registers. The re-verify used to call
 *  only `finishTask(done)`, so a late success produced a green Tasks row while
 *  the Library still showed the amber "couldn't confirm" state, still offered
 *  Install instead of Reinstall, and the staged pkg stayed on the console
 *  forever. Shared here so the two can't drift.
 *
 *  Best-effort by construction: nothing in here may throw into the caller —
 *  the install already succeeded.
 */
export async function finalizePkgInstallSuccess(args: {
  host: string;
  /** The staged pkg path on the console — the library row's key. */
  path: string;
  label: string;
  mayNotLaunch: boolean;
  /** The user's "Auto Delete after installation" preference for this install. */
  autoRemove: boolean;
}): Promise<void> {
  const { host, path, label, mayNotLaunch, autoRemove } = args;
  const store = pkgLibraryStore(host);
  const entries = () => store.getState().entries;
  const patch = (p: Partial<PkgEntry>) =>
    store.setState({
      entries: entries().map((e) => (e.path === path ? { ...e, ...p } : e)),
    });
  const entry = entries().find((e) => e.path === path);

  // Record THIS package as installed ON THIS CONSOLE (persisted) and
  // reflect it on the row, so an update/DLC that's been installed shows
  // "Reinstall" — not "Install" — even though app_list can't confirm an
  // add-on. Scoped to `host` so a sibling console with the same staged
  // file isn't wrongly marked installed.
  const installedAlternativeKey = entry ? pkgAlternativeKey(entry) : null;
  const replacedAlternativePaths = installedAlternativeKey
    ? entries()
        .filter(
          (candidate) =>
            candidate.path !== path &&
            pkgAlternativeKey(candidate) === installedAlternativeKey,
        )
        .map((candidate) => candidate.path)
    : [];
  recordPkgInstalled(host, path, replacedAlternativePaths);
  if (replacedAlternativePaths.length > 0) {
    const replaced = new Set(replacedAlternativePaths);
    store.setState({
      entries: entries().map((candidate) =>
        replaced.has(candidate.path)
          ? { ...candidate, installedHere: false }
          : candidate,
      ),
    });
  }
  patch({
    status: "idle",
    installedHere: true,
    lastResult: installedLastResult(mayNotLaunch),
  });
  // Notify on confirmed completion (the engine only reports installed
  // once the title actually registered on disk — i.e. it's ready to
  // play). Surfaces in the bell even if the user navigated away while a
  // large title finished, which is exactly when a heads-up is wanted.
  pushNotification(mayNotLaunch ? "warning" : "success", `${label} installed`, {
    body: mayNotLaunch
      ? PKG_MAY_NOT_LAUNCH_MESSAGE
      : "Installed on the PS5 and ready to play.",
  });
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
    store.setState({ entries: entries().filter((e) => e.path !== path) });
  } else {
    log.info(
      "install",
      `keeping staged pkg after install (auto-delete off): ${path}`,
    );
  }
}

/** Delay before re-verify attempt `attempt` (0-based).
 *
 *  Pure so the schedule is testable without timers. Quick at first because a
 *  small package often registers within a minute, then backing off to a
 *  five-minute floor so a multi-hour install costs only a handful of probes.
 */
export function installReverifyDelaysMs(attempt: number): number {
  const schedule = [30_000, 60_000, 120_000, 300_000];
  return schedule[Math.min(attempt, schedule.length - 1)];
}

/** Total wall time the background re-verify keeps trying before it gives up
 *  and hands the row back to the user (spec §2: "for up to 30 minutes, then
 *  stop and leave the row in the unverified state with a manual Recheck
 *  action"). Without a cap the chain re-armed forever. */
export const INSTALL_REVERIFY_MAX_MS = 30 * 60 * 1000;

/** One re-verify probe is a cheap question — "is it registered NOW?" — not the
 *  growth-watching wait the post-install check performs. Seconds, not hours:
 *  this poller runs against the console's transfer port, and an ungated
 *  long-running poller is what once dropped exfat writes from 120 MB/s to
 *  10 MB/s. */
const INSTALL_REVERIFY_PROBE_MS = 15_000;

/** How long to wait before re-trying when a transfer to the same console is in
 *  flight. The tick is POSTPONED, not spent: an install that finishes during a
 *  25 GB upload must still be picked up afterwards. */
const INSTALL_REVERIFY_BUSY_RETRY_MS = 30_000;

/** Can the artifact probe ever succeed for this package?
 *
 *  `verifyDpiInstalledArtifact` compares the installed artifact against an
 *  exact identity, so with no derivable title id, and no fingerprint or size
 *  to match, it returns false at its first line — forever. Scheduling a
 *  re-verify against that is a row that can never resolve itself, so callers
 *  check this FIRST and finish the task instead.
 */
export function installReverifyProbeViable(
  contentId: string | null,
  expected: PkgExpectedIdentity | undefined,
): boolean {
  if (!titleIdFromContentId(contentId || "")) return false;
  return !!expected?.fingerprint || !!(expected?.size && expected.size > 0);
}

/** Keep asking whether an accepted-but-unverified install has registered.
 *
 *  Bounded on three axes, each of which was previously unbounded:
 *   - VIABILITY: if the probe provably cannot succeed (no title id, or neither
 *     fingerprint nor size), don't schedule at all — finish the task with a
 *     terminal state and tell the user where to look. A row parked against an
 *     impossible probe is inert until an app reload rewrites it.
 *   - TIME: at most `INSTALL_REVERIFY_MAX_MS` of wall clock, after which the
 *     row stays `awaiting` but carries a Recheck control the user can drive.
 *   - COST: each tick is a seconds-long probe, and it is postponed entirely
 *     while a transfer to the same console is running (`transferScreenBusy`,
 *     the same gate the mgmt-port pollers use).
 *
 *  Fire-and-forget. Never throws: a failure to verify leaves the row exactly
 *  as it was, which is the honest outcome.
 */
export function scheduleInstallReverify(args: {
  taskId: string;
  host: string;
  name: string;
  contentId: string | null;
  packageType: string;
  expected: PkgExpectedIdentity | undefined;
  /** Staged pkg path + flags, so a LATE success can run the same post-install
   *  work a normal completion does (library row, notification, cleanup). */
  path?: string;
  autoRemove?: boolean;
  mayNotLaunch?: boolean;
}): void {
  const tasks = () => useTaskStore.getState();
  if (!installReverifyProbeViable(args.contentId, args.expected)) {
    // Nothing to probe against. Don't pretend to keep checking: close the row
    // with an honest message so it is dismissible rather than permanent.
    log.info(
      "install",
      `re-verify not possible for ${args.name} (no title id / no size or fingerprint) — ` +
        `leaving the outcome to the PS5's notifications`,
    );
    // `unverified`, not `cancelled`: nothing was stopped. The console took
    // the install and is very likely completing it — we just have no way to
    // confirm that from here.
    tasks().finishTask(args.taskId, "unverified", {
      detail: trStatic(
        "pkg.reverify_impossible",
        PKG_REVERIFY_IMPOSSIBLE_HINT,
      ),
    });
    return;
  }
  const giveUpAt = Date.now() + INSTALL_REVERIFY_MAX_MS;
  let attempt = 0;
  // Out of automatic attempts. Leave the row `awaiting` (the install may
  // still be running) but hand the user the Recheck control so the row is
  // theirs to resolve instead of re-arming forever.
  const giveUp = () => {
    tasks().updateTask(args.taskId, {
      detail: trStatic("pkg.reverify_gave_up", PKG_REVERIFY_GAVE_UP_HINT),
      control: { owner: "pkg-install", taskId: args.taskId },
    });
  };
  const tick = async () => {
    // Stop if the user resolved the row by hand, or the app moved on.
    const task = tasks().tasks.find((t) => t.id === args.taskId);
    if (!task || task.status !== "awaiting") return;
    // Expiry is checked before the busy gate, not after it. A console with a
    // transfer that never ends would otherwise postpone every tick forever
    // and the row would never gain its Recheck control.
    if (Date.now() >= giveUpAt) {
      giveUp();
      return;
    }
    // Never compete with a transfer for the console's single-client transfer
    // port: postpone (don't consume) this tick.
    if (transferScreenBusy(args.host)) {
      setTimeout(() => void tick(), INSTALL_REVERIFY_BUSY_RETRY_MS);
      return;
    }
    let ok: boolean;
    try {
      ok = await verifyDpiInstalledArtifact(
        args.host,
        args.contentId,
        args.packageType,
        args.expected,
        undefined,
        { idleMs: INSTALL_REVERIFY_PROBE_MS, maxMs: INSTALL_REVERIFY_PROBE_MS },
      );
    } catch {
      ok = false;
    }
    if (ok) {
      // Flip the row first — the confirmation is already in hand — then run
      // the same post-install work a normal completion does. Without it the
      // Tasks row said done while the Library still showed "couldn't confirm"
      // + Install, and the staged pkg was never cleaned up.
      tasks().finishTask(args.taskId, "done");
      if (args.path) {
        void finalizePkgInstallSuccess({
          host: args.host,
          path: args.path,
          label: args.name,
          mayNotLaunch: args.mayNotLaunch === true,
          autoRemove: args.autoRemove === true,
        }).catch(() => {});
      }
      return;
    }
    if (Date.now() >= giveUpAt) {
      giveUp();
      return;
    }
    const delay = installReverifyDelaysMs(attempt);
    attempt += 1;
    setTimeout(() => void tick(), delay);
  };
  setTimeout(() => void tick(), installReverifyDelaysMs(0));
}

/** Re-run the background re-verify for an `awaiting` pkg-install row — the
 *  Recheck action on the Tasks screen. Reads everything it needs from the
 *  task's own payload, so it works after a navigation. */
export function retryInstallReverify(task: {
  id: string;
  label: string;
  consoleId: string;
  payload: Record<string, unknown>;
}): boolean {
  const payload = task.payload || {};
  const path =
    typeof payload.localPs5Path === "string" ? payload.localPs5Path : undefined;
  const contentId =
    typeof payload.contentId === "string" ? payload.contentId : null;
  const packageType =
    typeof payload.packageType === "string" ? payload.packageType : "";
  // The engine-resolved type from the original schedule, when it was
  // persisted (see runPkgInstall's `awaiting` transition). Preferred over the
  // caller's `packageType`, which is null on the upload-queue mainline and
  // would otherwise probe with the wrong artifact type — see the module doc
  // comment on `resolvedPackageType` and pkgLibrary.reverify.test.ts.
  const resolvedPackageType =
    typeof payload.resolvedPackageType === "string"
      ? payload.resolvedPackageType
      : undefined;
  const mayNotLaunch = payload.mayNotLaunch === true;
  const expected = payload.expected as PkgExpectedIdentity | undefined;
  if (!installReverifyProbeViable(contentId, expected)) return false;
  // Clear the give-up detail and control so the row reads as live again, and
  // so a second Recheck click can't start a concurrent chain on this task
  // while this one is still running its own 30-minute window.
  useTaskStore.getState().updateTask(task.id, {
    detail: path ?? task.label,
    control: undefined,
  });
  scheduleInstallReverify({
    taskId: task.id,
    host: task.consoleId,
    name: basenameOf(path || "") || task.label,
    contentId,
    packageType: resolvedPackageType ?? packageType ?? "",
    expected,
    path,
    autoRemove: payload.deleteStaging === true,
    mayNotLaunch,
  });
  return true;
}

/** Is a payload loader visible in the console's process list?
 *
 *  Distinguishes "you never loaded one" from "it is running but not listening
 *  on :9021". The 2026-09-09 reports were all the second, and the advice for
 *  the first ("re-run your loader") reads as obviously wrong to someone
 *  looking at a running loader — which is why that user stopped.
 *
 *  Best-effort by construction: a diagnostic must never turn a failed install
 *  into a thrown error, so every failure answers "not seen".
 */
export async function loaderProcessSeen(host: string): Promise<boolean> {
  try {
    const { processes } = await processList(mgmtAddr(host));
    return processes.some((p) => /elfldr|pldmgr|etahen/i.test(p.name ?? ""));
  } catch {
    return false;
  }
}

async function runDpiInstall(
  host: string,
  localPs5Path: string,
  onStatus?: (msg: string) => void,
  // Identity of the package being installed. The engine cannot parse a file
  // that lives on the console, so it needs these to check afterwards whether
  // an update actually took effect. Optional: absent simply skips the check.
  verify?: { titleId?: string; packageAppVer?: string },
): Promise<{
  ok: boolean;
  ambiguous: boolean;
  errMessage: string;
  daemonFailed: boolean;
  /** Machine-readable cause when `daemonFailed` — see `dpiUnavailableCopy`. */
  daemonReason?: string;
  rc: number;
  patchVerdict?: string;
  appVerBefore?: string;
  appVerAfter?: string;
  /** The link was too long for the PS5's installer, so the engine handed it
   *  a short alias on this computer that redirects to the link. The console
   *  re-resolves it during the install, so this computer must stay up. */
  shortened?: boolean;
}> {
  const ip = hostOf(host);
  // dpi_ensure sends the DPI ELF to the loader port (:9021). Whether that
  // displaces the running helper is LOADER-dependent, not ours: we never evict
  // for a companion image, and measured on FW 5.10 and FW 9.60 the helper's
  // :9113/:9114 stayed up while :9040 came online beside them. A
  // single-payload loader would still replace it, which is why `sent` drives a
  // restore below. Log around it either way: issue #152's "helper dies ~4s
  // after a rejected update" reports land right here, and the next bundle will
  // show whether dpi_ensure succeeded, timed out, or never returned.
  log.info(
    "install",
    `DPI ensure: bringing up daemon on ${ip}:9040 (loads via :9021)`,
  );
  let ens: {
    ok?: boolean;
    error?: string;
    listening?: boolean;
    sent?: boolean;
    reason?: string;
  };
  try {
    ens = (await invoke("dpi_ensure", { ip })) as typeof ens;
  } catch (e) {
    // The bridge can lose the response after the loader already replaced the
    // helper. Restore defensively; no transfer is active while this lock runs.
    await restoreMainPayload(ip);
    return {
      ok: false,
      ambiguous: false,
      daemonFailed: true,
      // The bridge, not the console, is what failed here — no reason code
      // applies, so the neutral fallback copy is the honest one.
      daemonReason: undefined,
      rc: 0,
      errMessage: `couldn't start the DPI daemon: ${pkgError(e)}`,
    };
  }
  log.info(
    "install",
    `DPI ensure result: ok=${ens.ok} listening=${ens.listening ?? "?"} sent=${ens.sent ?? "?"}` +
      (ens.reason ? ` reason=${ens.reason}` : "") +
      (ens.error ? ` error="${ens.error}"` : ""),
  );
  if (!ens.ok) {
    // A sent-but-not-listening daemon has already displaced the main payload.
    // Leaving it that way was the cause of the apparent post-install disconnect.
    if (ens.sent) await restoreMainPayload(ip);
    return {
      ok: false,
      ambiguous: false,
      daemonFailed: true,
      daemonReason: ens.reason,
      rc: 0,
      errMessage: ens.error || "the DPI daemon didn't come up on :9040",
    };
  }
  // Send the install, retrying the transient "console busy" rejection
  // (0x80020002) — it clears once the console settles, so we gate each retry on
  // the readiness probe instead of failing the way a single attempt used to.
  let resp: {
    ok?: boolean;
    rc?: number;
    err_message?: string;
    ambiguous?: boolean;
    patch_verdict?: string;
    app_ver_before?: string;
    app_ver_after?: string;
    shortened?: boolean;
  } = {};
  try {
    for (let attempt = 1; attempt <= DPI_MAX_ATTEMPTS; attempt++) {
      try {
        resp = (await invoke("pkg_dpi_install", {
          ps5Addr: mgmtAddr(host),
          localPs5Path,
          titleId: verify?.titleId,
          packageAppVer: verify?.packageAppVer,
        })) as typeof resp;
      } catch (e) {
        resp = {
          ok: false,
          ambiguous: true,
          rc: -1,
          err_message: pkgError(e),
        };
      }
      const rcNow = (resp.rc ?? 0) >>> 0;
      if (
        resp.ok ||
        rcNow !== DPI_TRANSIENT_BUSY_RC ||
        attempt === DPI_MAX_ATTEMPTS
      ) {
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
  } finally {
    // Always restore, including a thrown readiness probe or DPI bridge error.
    await restoreMainPayload(ip);
  }
  const ok = !!resp.ok;
  const rc = (resp.rc ?? 0) >>> 0;
  return {
    ok,
    ambiguous: !!resp.ambiguous,
    daemonFailed: false,
    rc,
    errMessage: ok
      ? ""
      : resp.err_message ||
        `Install was rejected (0x${rc.toString(16).padStart(8, "0")}).`,
    patchVerdict: resp.patch_verdict,
    appVerBefore: resp.app_ver_before,
    appVerAfter: resp.app_ver_after,
    shortened: !!resp.shortened,
  };
}

/**
 * Streaming/direct install (beta, #81): hand the DPI daemon the engine's
 * `/pkg-host/` URL for an existing session instead of a staged PS5 path.
 * The daemon pulls the pkg over HTTP — no staging copy is uploaded to
 * the PS5 first. Mirrors `runDpiInstall`'s daemon-bring-up + restore
 * dance, but sends a session_id rather than a local path.
 *
 * The engine prefers an etaHEN / elf-arsenal "DPI v2" bridge if one is already
 * listening on the console (:12800). When it is, nothing is sent to the payload
 * loader at all and the user's running payloads are left alone; our own DPI ELF
 * — which does replace the main payload for the duration — is only used when no
 * bridge is there. `bridge` says which one ran.
 *
 * The caller MUST have already registered the session with the engine
 * (via `pkg_install_start` with `localPs5Path: null` and a PC-side file
 * path) so `/pkg-host/{session}/` is serving bytes when the daemon comes
 * asking. Returns `daemonFailed:true` distinctly so callers can treat a
 * "daemon never came up" dead-end separately from a Sony-side reject.
 *
 * Like `runDpiInstall`, `ok:true` here is the daemon's word that
 * `InstallByPackage` accepted — callers should confirm with
 * the engine's registration/byte-settle tracker before claiming a real install.
 */
async function runDpiDirectInstall(
  host: string,
  sessionId: string,
  onStatus?: (msg: string) => void,
): Promise<{
  ok: boolean;
  ambiguous: boolean;
  errMessage: string;
  daemonFailed: boolean;
  daemonReason?: string;
  rc: number;
  requestsServed: number;
  bytesServed: number;
  /** "dpiv2" = an existing etaHEN/Arsenal bridge did it, no payload swap.
   *  "ps5upload" = our own DPI daemon. Undefined on failures before hand-off. */
  bridge?: string;
}> {
  const ip = hostOf(host);
  log.info(
    "install",
    `DPI ensure (direct): bringing up daemon on ${ip}:9040 (loads via :9021)`,
  );
  let ens: {
    ok?: boolean;
    error?: string;
    listening?: boolean;
    sent?: boolean;
    reason?: string;
  };
  try {
    ens = (await invoke("dpi_ensure", { ip })) as typeof ens;
  } catch (e) {
    await restoreMainPayload(ip);
    return {
      ok: false,
      ambiguous: false,
      daemonFailed: true,
      daemonReason: undefined,
      rc: 0,
      requestsServed: 0,
      bytesServed: 0,
      errMessage: `couldn't start the DPI daemon: ${pkgError(e)}`,
    };
  }
  log.info(
    "install",
    `DPI ensure (direct) result: ok=${ens.ok} listening=${ens.listening ?? "?"} sent=${ens.sent ?? "?"}` +
      (ens.reason ? ` reason=${ens.reason}` : "") +
      (ens.error ? ` error="${ens.error}"` : ""),
  );
  if (!ens.ok) {
    if (ens.sent) await restoreMainPayload(ip);
    return {
      ok: false,
      ambiguous: false,
      daemonFailed: true,
      daemonReason: ens.reason,
      rc: 0,
      requestsServed: 0,
      bytesServed: 0,
      errMessage: ens.error || "the DPI daemon didn't come up on :9040",
    };
  }
  let resp: {
    ok?: boolean;
    rc?: number;
    err_message?: string;
    ambiguous?: boolean;
    requests_served?: number;
    bytes_served?: number;
    bridge?: string;
  } = {};
  try {
    for (let attempt = 1; attempt <= DPI_MAX_ATTEMPTS; attempt++) {
      try {
        resp = (await invoke("pkg_dpi_direct_install", {
          ps5Addr: mgmtAddr(host),
          sessionId,
        })) as typeof resp;
      } catch (e) {
        resp = {
          ok: false,
          ambiguous: true,
          rc: -1,
          err_message: pkgError(e),
        };
      }
      const rcNow = (resp.rc ?? 0) >>> 0;
      if (
        resp.ok ||
        rcNow !== DPI_TRANSIENT_BUSY_RC ||
        attempt === DPI_MAX_ATTEMPTS
      ) {
        break;
      }
      onStatus?.(
        `PS5 is busy finishing the last install — waiting for it to be ready (attempt ${attempt}/${DPI_MAX_ATTEMPTS})…`,
      );
      await waitForConsoleReady(host, {
        onWait: () => onStatus?.("Waiting for the PS5 to be ready…"),
      });
    }
  } finally {
    // Only restore what we actually displaced. When an etaHEN / elf-arsenal
    // bridge was already listening, `dpi_ensure` sent nothing, so the main
    // payload is still running — re-sending it here would cause the very
    // disruption using the bridge is meant to avoid.
    if (ens.sent) {
      await restoreMainPayload(ip);
    } else {
      log.info(
        "install",
        "DPI: nothing was sent to the loader, leaving the running payloads alone",
      );
    }
  }
  const ok = !!resp.ok;
  const rc = (resp.rc ?? 0) >>> 0;
  return {
    ok,
    ambiguous: !!resp.ambiguous,
    daemonFailed: false,
    daemonReason: undefined,
    rc,
    bridge: resp.bridge,
    requestsServed: resp.requests_served ?? 0,
    bytesServed: resp.bytes_served ?? 0,
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
 * launchable on newer firmware), then restore the main payload. A DPI rc=0 is
 * only acceptance and returns `acceptedUnverified`; `installed` is reserved for
 * registration/byte-settle proof from the main engine's status session.
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
export function pkgTypeForCategory(
  category?: string | null,
  platform?: string | null,
): string | null {
  const prefix = platform === "ps5" ? "PS5" : "PS4";
  switch (category) {
    case "gd":
      return `${prefix}GD`; // full game
    case "gp":
      return `${prefix}DP`; // patch (shares the base content_id — guarded)
    case "ac":
      return `${prefix}AC`; // add-on / DLC
    case "gde":
      return `${prefix}GDE`;
    case "la":
      return `${prefix}LA`;
    default:
      return null;
  }
}

export interface PkgExpectedIdentity {
  size?: number;
  fingerprint?: string;
}

async function runPkgInstallCore(
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
  // Called each status poll with the install's live state, so the UI can render
  // a real % and name the phase (Sony's BGFT progress isn't meaningful on the
  // file:// staging path). Optional — no-op if omitted.
  onProgress?: (sample: InstallSample) => void,
  // Called with a human-readable status line while the install waits on the
  // console-readiness gate (pre-install + DPI transient retry). Lets the caller
  // surface "Waiting for the PS5 to be ready…" instead of a frozen UI.
  onStatus?: (msg: string) => void,
  expected?: PkgExpectedIdentity,
  /** `APP_VER` the package declares (from its PARAM.SFO). Lets the engine
   *  check afterwards that an update actually raised the installed version —
   *  Sony returns success for an update it silently discards. */
  packageAppVer?: string,
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
  let acceptedUnverified = false;
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
  // Local name for the messages below. NOT the bare identifier `name`, which
  // resolves to `window.name` in this scope (and is deprecated) — there is no
  // local one here, since the caller owns the display label.
  const pkgLabel = basenameOf(localPs5Path) || contentId || "package";
  try {
    // Say what the console already has, using the engine's own verdict, before
    // the install frame is sent. The engine's guards cover some of this (a
    // staged re-install of a full game is refused outright, a patch with no base
    // likewise), but nothing reported the ordinary case — "this exact package is
    // already here" — and a client-side check cannot see a PS5 debug package at
    // all, whose console artifact is the inner image rather than the container
    // we hold. Informational: a re-install is a legitimate repair.
    if (contentId) {
      const pre = await pkgInstallPreflight(host, contentId, {
        packageType,
        size: expected?.size,
        fingerprint: expected?.fingerprint,
      });
      if (pre?.state === "installed") {
        onStatus?.(
          `${pkgLabel} is already installed on the PS5${pre.installedVersion ? ` (version ${pre.installedVersion})` : ""} — reinstalling over it…`,
        );
      } else if (pre?.state === "different_version_installed") {
        onStatus?.(
          `${pkgLabel}: ${pre.detail} — installing this build over it…`,
        );
      }
    }
    const r = (await invoke("pkg_install_start", {
      ps5Addr: mgmtAddr(host),
      path: null,
      splitRoot: null,
      packageTypeOverride: packageType,
      localPs5Path,
      contentId: contentId || null,
      expectedSize: expected?.size ?? null,
      // Only a complete sampled identity may override the engine's own remote
      // sampling. A cold library scan initially knows the 32-hex directory
      // token; passing that as though it were a full fingerprint would make
      // completion verification impossible.
      packageFingerprint: /^[a-f0-9]{64}$/i.test(expected?.fingerprint || "")
        ? expected?.fingerprint
        : null,
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
        acceptedUnverified = verdict.acceptedUnverified;
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
    const dpi = await runDpiInstall(host, localPs5Path, onStatus, {
      titleId: titleIdFromContentId(contentId ?? "") ?? undefined,
      packageAppVer,
    });
    log.info(
      "install",
      `DPI fallback result: ok=${dpi.ok} daemonFailed=${dpi.daemonFailed} ` +
        `rc=0x${(dpi.rc >>> 0).toString(16).padStart(8, "0")}` +
        (dpi.errMessage ? ` err="${dpi.errMessage}"` : ""),
    );
    if (dpi.daemonFailed) {
      // DPI couldn't even come up. For a patch, give update-specific guidance
      // (base is safe; try the PS5's Package Installer) rather than a raw
      // daemon error — but say that the console never saw the update, which
      // is a different problem from "the PS5 declined it" and has a
      // different fix. WHICH guidance depends on why the daemon didn't
      // start: a missing image in this build, a console loader that isn't
      // answering on :9021, or a daemon that was sent and never came up.
      // Those have nothing in common but the symptom.
      // Is a loader PROCESS there? Four reports on 2026-09-09 had elfldr.elf
      // running while :9021 refused, and "re-run your loader" reads as wrong
      // to someone looking at a running loader. Best-effort: never let this
      // diagnostic turn a failed install into a thrown error.
      const loaderProcessRunning = await loaderProcessSeen(host);
      log.error(
        "install",
        `update installer never started: reason=${dpi.daemonReason ?? "unknown"} ` +
          `loader_process=${loaderProcessRunning} main=${mainErr} ` +
          `err="${dpi.errMessage}"`,
      );
      if (resolvedType.endsWith("DP")) {
        return {
          installed: false,
          mayNotLaunch,
          // Carries BOTH halves: why the console refused the install, and why
          // the fallback could not be delivered. The first used to be dropped
          // here, so the code naming the real problem never reached the user.
          errMessage: patchInstallFailure({
            mainErr,
            reason: dpi.daemonReason,
            dpiErr: dpi.errMessage,
            loaderProcessRunning,
            translate: (key, fallback) => trStatic(key, fallback),
          }),
          stalled,
          resolvedPackageType: resolvedType,
        };
      }
      throw new Error(
        `Main-payload install failed (${mainErr}) and ${dpi.errMessage}`,
      );
    }
    if (dpi.ambiguous) {
      // FW 9.60 can close the DPI connection, or return the daemon's
      // 0xffffffff sentinel, after the package has already landed. A missing
      // acknowledgement is not a Sony rejection: restore happened in
      // runDpiInstall, so ask the main payload for the exact category/size/
      // fingerprint before saying anything failed.
      installed = await verifyDpiInstalledArtifact(
        host,
        contentId,
        resolvedType,
        expected,
        onStatus,
      );
      acceptedUnverified = !installed;
      mainErr = installed
        ? ""
        : "The installer connection ended before the PS5 acknowledged the result. ps5upload could not find the exact package afterward, so the outcome is still unverified. The staged package was kept for a safe retry.";
    } else if (dpi.patchVerdict === "regressed") {
      // Re-applying an already-installed update makes the console DELETE it:
      // hardware-observed on FW 5.10, /user/patch/<TID>/ removed and the title
      // back from 01.09 to 01.00. Never report that as success.
      log.error(
        "install",
        `update was removed by re-applying it: ${dpi.appVerBefore ?? "?"} -> ${dpi.appVerAfter ?? "?"}`,
      );
      return {
        installed: false,
        stalled: false,
        acceptedUnverified: false,
        mayNotLaunch: false,
        errMessage: trStatic("pkg.patch_regressed", PKG_PATCH_REGRESSED_HINT),
        resolvedPackageType: resolvedType,
      };
    } else if (dpi.patchVerdict === "did_not_apply") {
      // The engine watched APP_VER and it never moved: Sony accepted the
      // package and copied nothing. Proven on hardware — the same patch that
      // no-ops over a base installed by another tool applies in 150 s once
      // the base is re-installed through ps5upload. `dpi.errMessage` carries
      // that workaround.
      log.error(
        "install",
        `update did not apply: ${dpi.appVerBefore ?? "?"} -> ${dpi.appVerAfter ?? "?"} ` +
          `(package declares ${packageAppVer ?? "?"})`,
      );
      return {
        installed: false,
        stalled: false,
        acceptedUnverified: false,
        mayNotLaunch: false,
        // Use our own translated copy rather than the engine's English
        // prose; the engine's text is for logs and bug reports.
        errMessage: trStatic(
          "pkg.patch_did_not_apply",
          PKG_PATCH_DID_NOT_APPLY_HINT,
        ),
        resolvedPackageType: resolvedType,
      };
    } else if (dpi.ok) {
      // DPI's rc=0 proves only that Sony accepted InstallByPackage. Confirm the
      // exact category-specific artifact after the main payload comes back.
      // This distinguishes same-version alternatives (Optional Fix/Backport)
      // and independent DLC instead of checking only the shared base title.
      installed = await verifyDpiInstalledArtifact(
        host,
        contentId,
        resolvedType,
        expected,
        onStatus,
      );
      acceptedUnverified = !installed;
      mainErr = installed ? "" : PKG_ACCEPTED_UNVERIFIED_HINT;
    } else {
      // DPI ran but the PS5 declined the install. An update that can't apply
      // (wrong base version, or no base) gets the update-specific guidance;
      // everything else keeps DPI's own error.
      mainErr = resolvedType.endsWith("DP")
        ? PKG_PATCH_REJECTED_HINT
        : dpi.errMessage || mainErr;
    }
  }
  return {
    installed,
    mayNotLaunch,
    errMessage: mainErr,
    stalled,
    acceptedUnverified,
    resolvedPackageType: resolvedType,
  };
}

/**
 * Public install entrypoint. In addition to running the safety-gated install
 * cascade, mirror its lifecycle into the unified Tasks surface so a long PKG
 * install remains visible after the user navigates away from its source page.
 */
export async function runPkgInstall(
  host: string,
  localPs5Path: string,
  contentId: string | null,
  packageType: string | null,
  deleteStaging: boolean,
  onProgress?: (sample: InstallSample) => void,
  onStatus?: (msg: string) => void,
  expected?: PkgExpectedIdentity,
  /** `APP_VER` the package declares. Enables the post-install check that an
   *  update actually raised the installed version. */
  packageAppVer?: string,
): Promise<PkgInstallOutcome> {
  const name = basenameOf(localPs5Path) || contentId || "package";
  const tasks = useTaskStore.getState();
  const taskId = tasks.registerTask({
    kind: "pkg-install",
    origin: "pkg.install",
    label: `Installing ${name}`,
    detail: localPs5Path,
    consoleId: host,
    payload: { localPs5Path, contentId, packageType, deleteStaging, expected },
    status: "running",
  });
  let latestProgress:
    { current: number; total: number; unit: "bytes" } | undefined;
  try {
    const result = await runPkgInstallCore(
      host,
      localPs5Path,
      contentId,
      packageType,
      deleteStaging,
      (sample) => {
        latestProgress = {
          current: sample.installedBytes,
          total: sample.total,
          unit: "bytes",
        };
        useTaskStore
          .getState()
          .updateTask(taskId, { progress: latestProgress });
        onProgress?.(sample);
      },
      (message) => {
        useTaskStore.getState().updateTask(taskId, { detail: message });
        onStatus?.(message);
      },
      expected,
      packageAppVer,
    );

    const kind = installOutcomeKind(result);
    if (kind === "done") {
      useTaskStore.getState().finishTask(taskId, "done", {
        progress: latestProgress
          ? { ...latestProgress, current: latestProgress.total }
          : undefined,
        detail: localPs5Path,
      });
    } else if (kind === "unverified") {
      // NOT terminal and NOT failed: the PS5 took the install and is probably
      // still writing it. `awaiting` keeps the row live so the background
      // re-verify (see scheduleInstallReverify) can flip it to done.
      //
      // Persist the engine-resolved package type and mayNotLaunch into the
      // payload alongside the caller's original args: `retryInstallReverify`
      // (the Recheck action) rebuilds its args from this payload alone, and
      // without these two fields it falls back to the caller's (often null)
      // packageType — silently re-probing with the wrong artifact type and
      // losing the launch caution. See pkgLibrary.reverify.test.ts.
      const awaitingTask = useTaskStore.getState().getTask(taskId);
      useTaskStore.getState().updateTask(taskId, {
        status: "awaiting",
        progress: latestProgress,
        detail: localPs5Path,
        lastError: undefined,
        payload: {
          ...(awaitingTask?.payload ?? {}),
          resolvedPackageType: result.resolvedPackageType ?? packageType ?? "",
          mayNotLaunch: result.mayNotLaunch,
        },
      });
      // Only promise "ps5upload keeps checking" when the background re-verify
      // can actually run. When there is nothing to identify the package by,
      // scheduleInstallReverify closes the row on its first act, and that
      // toast would be sitting next to an already-closed row telling the user
      // to wait for a check that will never happen.
      if (installReverifyProbeViable(contentId, expected)) {
        showInstallUnverifiedToast(name);
      } else {
        showInstallUnconfirmableToast(name);
      }
      scheduleInstallReverify({
        taskId,
        host,
        name,
        contentId,
        packageType: result.resolvedPackageType ?? packageType ?? "",
        expected,
        // A late success must do everything a normal completion does.
        path: localPs5Path,
        autoRemove: deleteStaging,
        mayNotLaunch: result.mayNotLaunch,
      });
    } else {
      useTaskStore.getState().finishTask(taskId, "failed", {
        progress: latestProgress,
        detail: localPs5Path,
        lastError: {
          code: kind === "stalled" ? "INSTALL_STALLED" : "INSTALL_FAILED",
          message: result.errMessage || "Install was not confirmed.",
          recoverable: true,
        },
      });
      showInstallFailureToast(
        name,
        result.errMessage || "Install was not confirmed.",
      );
    }
    return result;
  } catch (error) {
    const message = pkgError(error);
    useTaskStore.getState().finishTask(taskId, "failed", {
      progress: latestProgress,
      detail: localPs5Path,
      lastError: {
        code: "INSTALL_ERROR",
        message,
        recoverable: true,
      },
    });
    showInstallFailureToast(name, message);
    throw error;
  }
}

/** Install failures must remain visible when the user has navigated away from
 * the originating screen. The task contains full diagnostics; this sticky
 * alert provides the missing foreground signal and a direct recovery path. */
function showInstallFailureToast(name: string, detail: string): void {
  const compact = detail.length > 260 ? `${detail.slice(0, 257)}…` : detail;
  useToastStore.getState().push({
    tone: "critical",
    message: `${name} was not verified as installed. ${compact}`,
    action: {
      label: "Open Tasks",
      onClick: () => {
        window.history.pushState({}, "", "/tasks");
        window.dispatchEvent(new PopStateEvent("popstate"));
      },
    },
  });
}

/** The PS5 accepted the install but we could not confirm completion yet.
 *  Informational, never critical — a large install routinely outlives the
 *  engine's grace window while the console is still copying files. */
function showInstallUnverifiedToast(name: string): void {
  useToastStore.getState().push({
    tone: "info",
    message: `${name} ${trStatic("pkg.install_unverified_toast", PKG_INSTALL_UNVERIFIED_TOAST)}`,
    action: {
      label: trStatic("pkg.open_tasks", "Open Tasks"),
      onClick: () => {
        window.history.pushState({}, "", "/tasks");
        window.dispatchEvent(new PopStateEvent("popstate"));
      },
    },
  });
}

/** The PS5 accepted the install but nothing identifies the package, so no
 *  background check is possible. Says so, rather than promising a re-verify
 *  that `scheduleInstallReverify` is about to decline. */
function showInstallUnconfirmableToast(name: string): void {
  useToastStore.getState().push({
    tone: "info",
    message: `${name}: ${trStatic("pkg.reverify_impossible", PKG_REVERIFY_IMPOSSIBLE_HINT)}`,
    action: {
      label: trStatic("pkg.open_tasks", "Open Tasks"),
      onClick: () => {
        window.history.pushState({}, "", "/tasks");
        window.dispatchEvent(new PopStateEvent("popstate"));
      },
    },
  });
}

/** AppShell and the Install page can both observe one native drag event while
 * navigation is settling. Deduplicate before the asynchronous header parse so
 * two callers cannot race past the later destination-path check and start two
 * writers for one staged file. */
const pkgAddsInFlight = new Set<string>();

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
            if (e.kind !== "file" || !isInstallPackagePath(e.name)) {
              continue;
            }
            const contentId = contentIdFromName(e.name);
            const path = `${dir}/${e.name}`;
            const cached = pathMetaFor(pathMeta, host, path);
            entries.push({
              name: e.name,
              path,
              size: e.size,
              contentId,
              title: titles[contentId],
              originalName: cached?.name,
              sourcePath: cached?.sourcePath,
              uploadedAt:
                cached?.uploadedAt ??
                (typeof e.mtime === "number" && e.mtime > 0
                  ? e.mtime * 1000
                  : undefined),
              appVer: cached?.appVer,
              fingerprint:
                cached?.fingerprint ?? fingerprintFromStagingSubdir(subdir),
              installedHere: isPkgInstalledHere(host, path),
              titleId: titleIdFromContentId(contentId) ?? undefined,
              // Authoritative category (read off the console) when we have it,
              // else the directory inference (updates/ → gp, dlc/ → ac).
              category: cached?.category ?? categoryForSubdir(subdir),
              platform: platformFromTitleId(titleIdFromContentId(contentId)),
              authenticity: cached?.authenticity,
              warnings: cached?.warnings,
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
          const categoryList = await listOne(dir, false);
          // Legacy rows live directly in updates/ or dlc/. New rows keep the
          // canonical `<ContentID>.pkg` basename one level deeper, under their
          // exact package fingerprint, so same-version variants coexist.
          addFrom(categoryList, subdir, dir);
          for (const instance of categoryList.filter((e) => e.kind === "dir")) {
            const instanceDir = `${dir}/${instance.name}`;
            addFrom(
              await listOne(instanceDir, false),
              `${subdir}/${instance.name}`,
              instanceDir,
            );
          }
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

    async addAndUpload(localPath, host, options) {
      if (!host?.trim()) return;
      const addKey = `${hostOf(host)}\u0000${localPath}`;
      if (pkgAddsInFlight.has(addKey)) return;
      pkgAddsInFlight.add(addKey);
      try {
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
        const fingerprint = meta.head?.fingerprint || undefined;
        const authenticity = meta.head?.authenticity;
        const parseWarnings = meta.head?.warnings?.length
          ? meta.head.warnings
          : undefined;

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
        const stagingDir = stagingDirectoryForPackage(category, fingerprint);
        const destPath = stagingDir
          ? `${PKG_LIBRARY_DIR}/${stagingDir}/${basename}`
          : `${PKG_LIBRARY_DIR}/${basename}`;
        // Remember the filename + version for this staged path so the row can show
        // them (survives refresh-from-disk and app restarts via localStorage).
        cachePathMeta(host, destPath, {
          name: originalName,
          sourcePath: localPath,
          appVer,
          fingerprint,
          authenticity,
          warnings: parseWarnings,
        });

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
          // Native drag events can be observed once by AppShell during navigation
          // and once by this screen after it mounts. Treat an exact duplicate as
          // idempotent: the first transfer owns the row; the second event is not a
          // user-facing error and must never start a competing writer.
          return;
        }

        // Re-adding an exact artifact that is already staged is also idempotent.
        // Distinct variants have distinct fingerprint directories, so this only
        // suppresses a genuine duplicate — never an optional-fix/backport pair.
        if (
          get().entries.some(
            (e) =>
              e.path === destPath &&
              e.status === "idle" &&
              e.size === totalBytes,
          )
        ) {
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
          sourcePath: localPath,
          appVer,
          fingerprint,
          titleId: titleIdFromContentId(contentId) ?? undefined,
          category,
          platform,
          authenticity,
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
          if (stagingDir) {
            try {
              // mkdir is one-level only. Create the category parent first, then
              // the fingerprint child used by the new multi-variant layout.
              const parts = stagingDir.split("/");
              let current = PKG_LIBRARY_DIR;
              for (const part of parts) {
                current = `${current}/${part}`;
                await fsMkdir(transferAddr(host), current);
              }
            } catch (e) {
              patch({
                status: "idle",
                bytes: undefined,
                lastResult: {
                  ok: false,
                  message: `Couldn't create the ${stagingDir} folder on the PS5: ${pkgError(e)}`,
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
          // Same 2 s trailing window as the Upload queue (500 ms polls × 4).
          // An instantaneous "bytes since last poll" rate swings between
          // hundreds of MiB/s and zero because the payload commits in shard
          // bursts; the window bridges the empty ticks.
          const rateSamples: RateSample[] = [];
          patch({ bytesPerSec: 0 });
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
              const now = Date.now();
              pushRateSample(rateSamples, now, js.bytes_sent);
              patch({
                bytes: js.bytes_sent,
                totalBytes: js.total_bytes ?? totalBytes,
                bytesPerSec: computeRate(rateSamples, now),
              });
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
          // Settle to idle and record the completion time before refreshing, so
          // the authoritative row immediately shows where/when it came from.
          const uploadedAt = Date.now();
          cachePathMeta(host, destPath, { uploadedAt });
          patch({ status: "idle", bytes: undefined, uploadedAt });
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
            const shouldInstall =
              options?.installAfterUpload ?? s.autoInstallAfterUpload;
            log.info(
              "install",
              `staged pkg uploaded: ${destPath} — install-after-upload=${shouldInstall}, auto-delete=${s.autoRemoveAfterInstall}`,
            );
            if (shouldInstall) {
              const uploaded = get().entries.find((e) => e.path === destPath);
              if (
                uploaded &&
                pkgHasConflictingAlternative(uploaded, get().entries) &&
                !options?.selectVariant
              ) {
                patch({
                  lastResult: {
                    ok: false,
                    warn: true,
                    message:
                      "Staged as an alternative variant. Choose which same-version patch/DLC matches this firmware; it was not auto-installed.",
                  },
                });
                pushNotification("info", "Package variant kept staged", {
                  body: "Another same-version patch or DLC variant is already in the library. Install the intended one from its row; ps5upload will not silently make the last upload win.",
                });
              } else {
                if (uploaded && options?.selectVariant) {
                  const key = pkgAlternativeKey(uploaded);
                  if (key) {
                    recordPkgAlternativeSelection(
                      host,
                      key,
                      pkgEntryIdentity(uploaded),
                    );
                  }
                }
                await get().install(destPath, host);
              }
            }
          }
        } catch (e) {
          // Drop the optimistic row and surface the error.
          set({
            entries: get().entries.filter((e2) => e2.path !== destPath),
            error: `Upload failed: ${pkgError(e)}`,
          });
        }
      } finally {
        pkgAddsInFlight.delete(addKey);
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
          entries: get().entries.map((e) =>
            e.path === path ? { ...e, ...p } : e,
          ),
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

        // High-firmware heads-up. AppInst can reject this path with
        // 0x80B2116F even when connectivity, kstuff, and initialization all
        // look healthy. We still attempt the verified path, but never call an
        // rc=0/zero-byte fallback or label a black screen as normal.
        {
          const rt =
            useConnectionStore.getState().runtimeByHost[hostOf(host)] ?? null;
          const fw = parsePS5Firmware(rt?.ps5Kernel ?? null);
          const major = fw ? parseFloat(fw) : 0;
          if (major >= 12) {
            set({
              busyNotice:
                "Installing on FW 12.x… ps5upload will only report success after console-side verification. If PlayGo rejects the install, the package stays staged for the PS5's Debug Settings Package Installer.",
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
          patch({
            status: "idle",
            lastResult: { ok: false, message: spaceWarn },
          });
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
        const {
          installed,
          mayNotLaunch,
          errMessage: mainErr,
          stalled,
          acceptedUnverified,
        } = await runPkgInstall(
          host,
          path,
          entry?.contentId || null,
          pkgTypeForCategory(entry?.category, entry?.platform),
          autoRemove,
          // Live install %: a large title installs over minutes — feed both the
          // inline notice and the global Activity bar so progress shows
          // everywhere. Guarded so a 0 total can't divide.
          ({ installedBytes, total }) => {
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
          { size: entry?.size, fingerprint: entry?.fingerprint },
          // Lets the engine confirm an update actually raised APP_VER instead
          // of trusting Sony's return code, which is 0 either way.
          entry?.appVer,
        );
        useActivityHistoryStore
          .getState()
          .finish(
            actId,
            installed
              ? "done"
              : acceptedUnverified || stalled
                ? "stopped"
                : "failed",
          );

        if (installed) {
          await finalizePkgInstallSuccess({
            host,
            path,
            label,
            mayNotLaunch,
            autoRemove,
          });
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
            lastResult: {
              ok: false,
              warn: acceptedUnverified,
              message: mainErr || "Install was rejected.",
            },
          });
          // Surface failures in the bell too (success already notifies above).
          // Without this a failed item — an update or DLC especially — was silent
          // if the user navigated away from the Library tab mid-install.
          pushNotification(
            stalled || acceptedUnverified ? "warning" : "error",
            acceptedUnverified
              ? `${label} install accepted; verify on PS5`
              : `${label} install ${stalled ? "didn’t finish" : "failed"}`,
            {
              body: mainErr || "The PS5 didn’t confirm the install. Try again.",
            },
          );
        }
      } catch (e) {
        const message = pkgError(e);
        patch({
          status: "idle",
          lastResult: { ok: false, message },
        });
        const entry = get().entries.find(
          (candidate) => candidate.path === path,
        );
        const label = entry?.title || entry?.contentId || basenameOf(path);
        pushNotification("error", `${label} install failed`, { body: message });
      } finally {
        set({ installing: false, busyNotice: null, installPending: false });
      }
    },

    async installAll(host, selections, installedPaths) {
      if (!host?.trim()) return;
      // Don't start a batch on top of a single in-flight install (or another
      // batch) — the inner install() would early-return and we'd silently skip
      // rows. The button is disabled in this state too; this is the guard.
      if (get().installing || get().installingAll) return;

      // Snapshot the not-yet-installed, idle rows and order them base → update →
      // DLC so an add-on never installs before its base. `installedHere` is the
      // authoritative per-package signal (see pkgRowInstalled); a row mid-upload
      // or queued is skipped — installAll only drives ready staged packages.
      const liveInstalled = installedPaths
        ? new Set(installedPaths)
        : undefined;
      const planningEntries = liveInstalled
        ? get().entries.map((entry) => ({
            ...entry,
            installedHere: liveInstalled.has(entry.path),
          }))
        : get().entries;
      const { targets, conflicts } = pkgInstallAllPlan(
        planningEntries,
        selections,
      );

      if (targets.length === 0) {
        pushNotification(
          "info",
          conflicts.length ? "Choose an update variant" : "Nothing to install",
          {
            body: conflicts.length
              ? "Conflicting same-version patch/DLC variants were kept staged. Install the one intended for this firmware from its row."
              : "Every staged package is already installed on this console.",
          },
        );
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
          if (
            !cur ||
            cur.status !== "idle" ||
            (liveInstalled ? liveInstalled.has(cur.path) : cur.installedHere)
          )
            continue;
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
        failed === 0 && conflicts.length === 0 ? "success" : "warning",
        failed === 0 && conflicts.length === 0
          ? `Installed ${ok} package${ok === 1 ? "" : "s"}`
          : conflicts.length > 0 && failed === 0
            ? `Installed ${ok}; skipped ${conflicts.length} conflicting variant${conflicts.length === 1 ? "" : "s"}`
            : `Installed ${ok} of ${ok + failed}; ${failed} failed`,
        {
          body:
            failed === 0 && conflicts.length === 0
              ? "All staged packages installed."
              : conflicts.length > 0 && failed === 0
                ? "Choose the patch or DLC variant intended for this firmware and install it from its row."
                : "Some packages didn't install — check the rows marked failed and retry them.",
        },
      );
    },

    async installDownloadedLink(url, host, insecureTls) {
      // Two legs, reported separately: people need to know which one is slow.
      // The download is the fragile one; once it finishes, the install is an
      // ordinary local-file install at LAN speed.
      //
      // The download leg is a task of its own, with a bar, a rate, an ETA and
      // a Cancel. Without one this mode sat silent for as long as the download
      // took — minutes for a big package — and read as "nothing is happening"
      // (reported from Discord: "waited 5 minutes, it doesn't appear on the PS5").
      let name = "package";
      try {
        name = basenameOf(new URL(url).pathname) || "package";
      } catch {
        /* the caller validated the URL; keep the generic name */
      }
      const tasks = useTaskStore.getState();
      const taskId = tasks.registerTask({
        kind: "download",
        origin: "pkg.url-download",
        label: `Downloading ${name} to this computer`,
        detail: "Connecting to the link…",
        consoleId: host,
        // Never record the URL: an install link can carry a signed token.
        payload: { remote: true },
        status: "running",
      });
      const fail = (message: string, cancelled = false) => {
        set({ busyNotice: null });
        useTaskStore
          .getState()
          .finishTask(taskId, cancelled ? "cancelled" : "failed", {
            detail: message,
            ...(cancelled
              ? {}
              : { lastError: { code: "LINK_DOWNLOAD_FAILED", message, recoverable: true } }),
          });
        return { ok: false, message };
      };

      let started: { download_id?: string; path?: string; total?: number };
      try {
        started = (await invoke("pkg_remote_download_start", {
          url,
          insecureTls,
          destDir: null,
        })) as { download_id?: string; path?: string; total?: number };
      } catch (e) {
        return fail(pkgError(e));
      }
      const id = started.download_id;
      const path = started.path;
      if (!id || !path) {
        return fail("The engine did not start a download for that link.");
      }
      useTaskStore.getState().updateTask(taskId, {
        engineJobId: id,
        control: { owner: "link-download", downloadId: id },
      });

      const total = started.total ?? 0;
      log.info(
        "install",
        `downloading the link to this computer first (${fmtBytes(total)})`,
      );

      // Poll until it finishes. Deliberately no timeout on the transfer as a
      // whole: a 100 GB package over a slow link legitimately takes hours,
      // and the engine reports an error the moment one actually occurs.
      const rateSamples: RateSample[] = [{ ts: Date.now(), bytes: 0 }];
      for (;;) {
        await new Promise((r) => setTimeout(r, 1000));
        let st: {
          written?: number;
          total?: number;
          done?: boolean;
          cancelled?: boolean;
          error?: string | null;
        };
        try {
          st = (await invoke("pkg_remote_download_status", { id })) as typeof st;
        } catch (e) {
          return fail(pkgError(e));
        }
        if (st.error) {
          return fail(`The download failed: ${st.error}`);
        }
        if (st.cancelled) {
          return fail("The download was cancelled.", true);
        }
        const written = st.written ?? 0;
        const size = st.total && st.total > 0 ? st.total : total;
        const now = Date.now();
        pushRateSample(rateSamples, now, written);
        const bytesPerSec = computeRate(rateSamples, now);
        const detail = describeLinkDownload(written, size, bytesPerSec);
        // The install popup and the Install screen show this line, not the task list.
        set({ busyNotice: detail });
        useTaskStore.getState().updateTask(taskId, {
          detail,
          ...(size > 0
            ? { progress: { current: written, total: size, unit: "bytes" as const } }
            : {}),
          ...(bytesPerSec > 0 ? { rate: { bytesPerSec } } : {}),
          ...(bytesPerSec > 0 && size > written
            ? { eta: (size - written) / bytesPerSec }
            : {}),
        });
        if (st.done) break;
      }

      useTaskStore.getState().finishTask(taskId, "done", {
        detail: "Downloaded. Installing on the PS5 from this computer…",
      });
      log.info("install", "download finished; installing from the local file");
      // From here it is a local file, so the link can expire freely.
      return get().installStream(path, host);
    },
    async installUrl(url, host, opts) {
      const trimmed = url.trim();
      let parsed: URL;
      try {
        parsed = new URL(trimmed);
      } catch {
        return {
          ok: false,
          message: "Enter a valid HTTP or HTTPS package URL.",
        };
      }
      // The engine re-validates (it is also a plain HTTP API), but rejecting
      // here keeps the user's mistake a local message instead of a round trip.
      const hasControlChars = [...trimmed].some((ch) => {
        const code = ch.charCodeAt(0);
        return code < 32 || code === 127;
      });
      if (
        !["http:", "https:"].includes(parsed.protocol) ||
        !parsed.hostname ||
        parsed.hash ||
        hasControlChars ||
        new TextEncoder().encode(trimmed).length > 4093
      ) {
        return {
          ok: false,
          message:
            "Enter an HTTP(S) package URL with no fragment or control characters (max 4093 bytes).",
        };
      }
      const mode =
        opts?.mode ?? useLinkInstallPrefs.getState().modeFor(host);
      const insecureTls = useLinkInstallPrefs.getState().insecureFor(host);

      // Download first, then install the local file. The only mode that
      // survives a link dying mid-install: the transfer finishes on its own
      // and the install afterwards never touches the network.
      if (mode === "download") {
        return get().installDownloadedLink(trimmed, host, insecureTls);
      }

      // Direct: hand the URL to the console's own installer via the DPI
      // daemon and get out of the way — this computer serves nothing and may
      // then sleep or close. Measured at ~90 MB/s from a LAN origin on FW
      // 5.10, against 108 for streaming, because the console opens only two
      // connections; on a slow or distant source that gap widens. Its
      // installer also refuses a link longer than 127 bytes.
      if (mode === "direct") {
        // Through runDpiInstall, not a bare pkg_dpi_install. The bare call
        // had three faults, each enough on its own to break this mode:
        //  - it never started the DPI daemon, so it failed whenever :9040
        //    was not already up from an earlier install;
        //  - it ignored the result — a refusal arrives as HTTP 200 with
        //    ok:false — so the user was told "Sent to the PS5, you can close
        //    ps5upload" while nothing was installing;
        //  - a link over the installer's 127-byte limit was always refused.
        //    The engine now swaps such a link for a short alias (see
        //    shorten_for_installer), which is why `shortened` matters below.
        const res = await runDpiInstall(host, trimmed);
        if (res.ok) {
          // The console downloads this itself, so no byte of it passes through here and
          // there is no rate to show. Record it anyway, so the install is in the task list
          // with an honest note instead of leaving no trace at all.
          let name = "package";
          try {
            name = basenameOf(parsed.pathname) || "package";
          } catch {
            /* keep the generic name */
          }
          const directTask = useTaskStore.getState().registerTask({
            kind: "pkg-dpi-install",
            origin: "pkg.url-direct",
            label: `PS5 downloading ${name}`,
            consoleId: host,
            payload: { remote: true, direct: true },
            status: "running",
          });
          useTaskStore.getState().finishTask(directTask, "done", {
            detail:
              "Handed to the PS5, which downloads and installs it on its own. " +
              "Its progress and speed show on the PS5 under Downloads, not here.",
          });
          return {
            ok: true,
            message: res.shortened
              ? "Sent to the PS5 — it downloads the package from the link itself. " +
                "The link is longer than the PS5 accepts, so it goes through a short " +
                "address on this computer: keep ps5upload running until the PS5 finishes."
              : "Sent to the PS5. It downloads and installs on its own from here — " +
                "watch progress on the console. You can close ps5upload.",
          };
        }
        // Name the reason and carry on. Streaming needs neither the DPI
        // daemon nor a console-reachable link, so a refusal here is not the
        // end of the install — but silently switching would leave someone
        // wondering why their computer is suddenly busy.
        log.info(
          "install",
          `the PS5 could not fetch that link itself (${res.errMessage}); downloading through this computer instead`,
        );
      }
      return get().installStream({ remoteUrl: trimmed }, host);
    },
    async installStream(source, host) {
      if (!host?.trim()) {
        return { ok: false, message: "No PS5 host selected." };
      }
      if (get().installing) {
        return { ok: false, message: "Another install is in progress." };
      }
      // A link and a local file differ only in where the bytes come from and
      // what we call them; the install itself is one path.
      const remoteUrl =
        typeof source === "object" && "remoteUrl" in source ? source.remoteUrl : null;
      const smb = typeof source === "object" && "smb" in source ? source.smb : null;
      const localPcPath = typeof source === "string" ? source : null;
      const sourceName = remoteUrl
        ? basenameOf(new URL(remoteUrl).pathname) || "package"
        : smb
          ? basenameOf(smb.path.replace(/\\/g, "/")) || "package"
          : basenameOf(localPcPath ?? "") || "package";
      const tasks = useTaskStore.getState();
      const taskId = tasks.registerTask({
        kind: "pkg-dpi-install",
        origin: remoteUrl ? "pkg.url-install" : "pkg.stream-install",
        label: `Stream-installing ${sourceName}`,
        detail: "Waiting to prepare the package…",
        consoleId: host,
        // Never record the URL: an install link can carry a signed token and
        // task payloads reach the diagnostic bundle.
        // Never the SMB password: task payloads are shown and persisted.
        payload: remoteUrl
          ? { remote: true }
          : smb
            ? { smb: { server: smb.server, share: smb.share, path: smb.path } }
            : { localPcPath },
        status: "queued",
      });
      let taskFinished = false;
      const finishStreamTask = <T extends { ok: boolean; message?: string }>(
        result: T,
        cancelled = false,
      ): T => {
        if (taskFinished) return result;
        taskFinished = true;
        if (result.ok) {
          const progress = useTaskStore.getState().getTask(taskId)?.progress;
          useTaskStore.getState().finishTask(taskId, "done", {
            detail: "Installed and verified on the PS5.",
            progress: progress
              ? { ...progress, current: progress.total }
              : undefined,
          });
        } else if (cancelled) {
          useTaskStore.getState().finishTask(taskId, "cancelled", {
            detail: result.message || "Cancelled.",
          });
        } else {
          const message = result.message || "The install didn't complete.";
          useTaskStore.getState().finishTask(taskId, "failed", {
            detail: message,
            lastError: {
              code: "STREAM_INSTALL_FAILED",
              message,
              recoverable: true,
            },
          });
        }
        return result;
      };
      set({ installing: true, busyNotice: null, installPending: false });
      const clearBusy = () =>
        set({ installing: false, busyNotice: null, installPending: false });
      let servingSession: string | null = null;
      // Whether the serving session may be torn down at the end. Defaults to
      // yes: every early return from the hand-off (daemon never came up, Sony
      // refused) leaves nothing that could still be reading. It is cleared only
      // where the install MIGHT be running — the ambiguous-acknowledgement path,
      // and a verify that stopped watching before the engine said it was done.
      let releaseServingSession = true;
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
            if (!get().installing) {
              return finishStreamTask(
                { ok: false, message: "Cancelled." },
                true,
              );
            }
            await sleep(400);
          }
          set({ installPending: false, busyNotice: null });
        }
        set({ installPending: false });
        useTaskStore.getState().updateTask(taskId, {
          status: "running",
          detail: "Reading package metadata…",
        });

        // 1. Parse the PC-side pkg header for content_id + category. The
        //    engine needs the content_id to canonicalise the pkg-host URL
        //    filename (Sony's installer cross-checks it against the header).
        // For a link the engine identifies the package by reading a handful
        // of byte ranges from the origin — the same proxy the install will
        // then stream through — so a 100 GB URL is named in one round trip
        // and an HTML share page is rejected before anything is committed.
        let head: {
          content_id?: string;
          title?: string;
          category?: string;
          platform?: string;
          package_type?: string;
          fingerprint?: string;
        };
        let totalBytes: number;
        if (smb) {
          // The engine reads the header off the share itself when the install
          // starts (the same ranges it then serves), so there is nothing to
          // probe here — a second read of the share would only add latency.
          head = {};
          totalBytes = smb.size ?? 0;
        } else if (remoteUrl) {
          try {
            const probe = (await invoke("pkg_remote_probe", {
              url: remoteUrl,
            })) as {
              total_size?: number;
              content_id?: string;
              title?: string;
              category?: string;
              platform?: string;
              package_type?: string;
              fingerprint?: string;
            };
            head = probe;
            totalBytes = probe.total_size ?? 0;
          } catch (e) {
            return finishStreamTask({
              ok: false,
              message: `Couldn't read a package from that link: ${pkgError(e)}`,
            });
          }
        } else {
          let meta: SplitParseResponse;
          try {
            meta = (await invoke("pkg_metadata_split", {
              path: localPcPath,
            })) as SplitParseResponse;
          } catch (e) {
            return finishStreamTask({
              ok: false,
              message: `Couldn't read .pkg header: ${pkgError(e)}`,
            });
          }
          if ((meta.parts?.length ?? 1) > 1) {
            return finishStreamTask({
              ok: false,
              message:
                "Split .pkg sets aren't supported by the streaming installer — pick the single lead .pkg.",
            });
          }
          head = meta.head ?? {};
          totalBytes = meta.total_size ?? 0;
        }
        const contentId = head.content_id ?? "";
        const label = head.title || contentId || sourceName;
        const resolvedPackageType =
          head.package_type || pkgTypeForCategory(head.category, head.platform);
        useTaskStore.getState().updateTask(taskId, {
          label: `Stream-installing ${label}`,
          detail: "Preparing the PS5 installer…",
          progress:
            totalBytes > 0
              ? { current: 0, total: totalBytes, unit: "bytes" }
              : undefined,
        });

        set({
          busyNotice: remoteUrl
            ? `Installing ${label} from the link (beta) — this computer downloads it over several connections at once and feeds the PS5, nothing is staged…`
            : `Stream-installing ${label} (beta) — the PS5 pulls the pkg directly over HTTP, no staging upload…`,
        });

        // 2. Register the session with the engine. Passing `localPs5Path:
        //    null` + a PC `path` makes the engine create a pkg-host serving
        //    session WITHOUT expecting a staged file on the PS5. The URL
        //    the engine builds is what the DPI daemon will fetch.
        const onStatus = (msg: string) => {
          set({ busyNotice: msg });
          useTaskStore.getState().updateTask(taskId, { detail: msg });
        };

        // Ask the console what it already has, before moving a byte. This is the
        // engine's own verdict, so it is the one that agrees with the completion
        // check at the end — and it is the only way to know for a PS5 debug
        // package, whose console-side artifact is the inner image and therefore
        // never matches our fingerprint. Informational only: a re-install is a
        // legitimate repair, so a hit is reported, not refused.
        const pre = contentId
          ? await pkgInstallPreflight(host, contentId, {
              packageType: resolvedPackageType,
              size: totalBytes,
              fingerprint: head.fingerprint,
            })
          : null;
        if (pre?.state === "installed") {
          onStatus(
            `${label} is already installed on the PS5${pre.installedVersion ? ` (version ${pre.installedVersion})` : ""} — reinstalling over it…`,
          );
        } else if (pre?.state === "different_version_installed") {
          onStatus(`${label}: ${pre.detail} — installing this build over it…`);
        }
        const startResp = (await invoke("pkg_install_start", {
          ps5Addr: mgmtAddr(host),
          path: localPcPath,
          splitRoot: null,
          // Exactly one of these is set. With remoteUrl the engine fetches the
          // package from the origin in parallel and serves it from the same
          // pkg-host session a local file would use; with smb it reads the
          // share the same way. Nothing is copied or staged for either.
          remoteUrl,
          smb: smb
            ? {
                server: smb.server,
                share: smb.share,
                user: smb.user,
                password: smb.password,
                path: smb.path,
              }
            : null,
          packageTypeOverride: resolvedPackageType,
          localPs5Path: null,
          contentId: contentId || null,
          // For SMB the size is the listing's, not a parsed header's: leave
          // it to the engine, which opened the file and knows it exactly.
          expectedSize: smb ? null : totalBytes || null,
          packageFingerprint: head.fingerprint ?? null,
          // No staging file is created, so deleteStaging is moot — pass
          // false so the engine doesn't record a staging_path to clean up.
          deleteStaging: false,
          // Per-host "skip the certificate check". This governs only what
          // THIS COMPUTER accepts while fetching from the origin; the
          // console's own handshake in direct mode is not ours to relax.
          insecureTls: useLinkInstallPrefs
            .getState()
            .insecureFor(host),
          // Serve-only: create the /pkg-host/ session but DON'T run the
          // in-process InstallByPackage. The standalone DPI process owns Sony's
          // HTTP installer state and returns its real proxy/network error without
          // tying up the main payload RPC thread. It performs the install in step
          // 3 (runDpiDirectInstall).
          serveOnly: true,
        })) as {
          err_code?: number;
          session_id?: string;
          url?: string;
          err_message?: string;
          may_not_launch?: boolean;
        };

        const rc = (startResp.err_code ?? 0) >>> 0;
        const sessionId = startResp.session_id;
        const servedFrom = originOf(startResp.url);
        // The engine creates a pkg-host session even when BGFT register
        // rejects (rc != 0) — but without a session_id there's nothing for
        // the daemon to fetch, so this is a hard fail.
        if (!sessionId) {
          return finishStreamTask({
            ok: false,
            message:
              startResp.err_message ||
              `The engine wouldn't start a serving session (0x${rc.toString(16).padStart(8, "0")}).`,
          });
        }
        servingSession = sessionId;
        useTaskStore.getState().updateTask(taskId, {
          engineJobId: sessionId,
          detail: "The PS5 is fetching the package from this computer…",
        });

        // 3. Hand the session's pkg-host URL to the DPI daemon. The daemon
        //    pulls the pkg over HTTP; no staging copy lands on the PS5.
        const dpi = await runDpiDirectInstall(host, sessionId, onStatus);
        if (dpi.daemonFailed) {
          const loaderUnavailable =
            dpi.daemonReason === "loader_unreachable" ||
            dpi.daemonReason === "loader_send_failed";
          return finishStreamTask({
            ok: false,
            message: loaderUnavailable
              ? `${dpi.errMessage}. Reload the payload loader on the PS5, then retry Stream install — or run etaHEN or elf-arsenal, whose installer bridge ps5upload will use instead, leaving your loaded payloads untouched. Uploading the package to staging cannot repair a closed loader and may only repeat the same failure.`
              : `${dpi.errMessage}. Upload & install can still try the PS5-local staged path instead.`,
            // A closed :9021 is a prerequisite failure, not an HTTP-path
            // failure. Offering staging here caused a reporter to upload a
            // multi-GB package only to hit the same DPI hand-off again.
            stagedFallbackRecommended: !loaderUnavailable,
            rc: dpi.rc,
            requestsServed: dpi.requestsServed,
          });
        }
        if (dpi.ambiguous) {
          useTaskStore.getState().updateTask(taskId, {
            detail:
              "The installer acknowledgement was lost; verifying the exact package on the PS5…",
          });
          const exactInstalled = await verifyDpiInstalledArtifact(
            host,
            contentId || null,
            resolvedPackageType ||
              (head.platform === "ps5" ? "PS5GD" : "PS4GD"),
            {
              size: totalBytes || undefined,
              fingerprint: head.fingerprint || undefined,
            },
          );
          if (exactInstalled) {
            pushNotification("success", `Installed ${label}`, {
              body: "The installer acknowledgement was lost, but the exact package was verified on the PS5.",
            });
            return finishStreamTask({ ok: true, mayNotLaunch: false });
          }
          // The acknowledgement was lost and the artifact isn't on disk, so the
          // install could still be running and pulling from this session.
          releaseServingSession = false;
          return finishStreamTask({
            ok: false,
            acceptedUnverified: true,
            stagedFallbackRecommended: true,
            rc: dpi.rc,
            requestsServed: dpi.requestsServed,
            message:
              "The PS5 fetched the package, but its installer connection ended before acknowledgement and ps5upload could not verify the exact installed artifact. Upload & install can retry from PS5-local staging.",
          });
        }
        if (!dpi.ok) {
          const rcHex = `0x${dpi.rc.toString(16).padStart(8, "0")}`;
          const blockedBeforeFetch = dpi.requestsServed === 0;
          const proxyRejected = dpi.rc === DPI_HTTP_PROXY_RC;
          const detail = proxyRejected
            ? `The PS5's proxy setting blocked the stream (${rcHex}, SCE_HTTP_ERROR_PROXY). In the PS5 network's Advanced Settings, set Proxy Server to “Do Not Use”, or use Upload & install, which reads the package from PS5-local storage.`
            : blockedBeforeFetch
              ? streamUnreachableMessage(rcHex, servedFrom)
              : `${dpi.errMessage} (${rcHex}) after ${dpi.requestsServed} package request${dpi.requestsServed === 1 ? "" : "s"} reached this computer. Upload & install uses the more reliable PS5-local staged path.`;
          return finishStreamTask({
            ok: false,
            message: detail,
            stagedFallbackRecommended: true,
            rc: dpi.rc,
            requestsServed: dpi.requestsServed,
          });
        }

        // 4. Track the install to a real terminal state (DPI's `ok` alone
        //    isn't proof — the daemon reports InstallByPackage's rc, not the
        //    async install result). The session is still alive for this, then
        //    the engine GCs it.
        //
        //    This is also the only place the transfer is observable: the
        //    hand-off in step 3 returns as soon as Sony *queues* the install,
        //    and the console then pulls the package for however long that
        //    takes. Polling the session's own counters is what turns that
        //    window into a live bar and a rate instead of one frozen sentence.
        const rateSamples: RateSample[] = [{ ts: Date.now(), bytes: 0 }];
        let lastDetail = "";
        const verdict0 = await verifyInstallCompleted(sessionId, (sample) => {
          const now = Date.now();
          // Feed the SAME counter the progress line reads. Sampling
          // transferBytes alone made the rate go dead the moment the transfer
          // finished and the console started writing: installedBytes is what
          // moves during the install phase, so "at X/s" sat at 0 for the
          // longest part of a big install.
          pushRateSample(
            rateSamples,
            now,
            Math.max(sample.transferBytes, sample.installedBytes),
          );
          const bytesPerSec = computeRate(rateSamples, now);
          const { detail, current } = describeInstallSample(
            sample,
            bytesPerSec,
          );
          if (detail !== lastDetail) {
            lastDetail = detail;
            set({ busyNotice: detail });
          }
          useTaskStore.getState().updateTask(taskId, {
            detail,
            ...(sample.total > 0
              ? {
                  progress: {
                    current,
                    total: sample.total,
                    unit: "bytes" as const,
                  },
                }
              : {}),
            ...(bytesPerSec > 0 ? { rate: { bytesPerSec } } : {}),
            ...(bytesPerSec > 0 && sample.total > current
              ? { eta: (sample.total - current) / bytesPerSec }
              : {}),
          });
        });
        const verdict = verdict0;
        releaseServingSession = verdict.trackedToTerminal;
        if (verdict.completed) {
          const viaBridge =
            dpi.bridge === "dpiv2"
              ? " Installed through the console's existing DPI bridge, so your loaded payloads were left running."
              : "";
          pushNotification("success", `Installed ${label}`, {
            body:
              (remoteUrl
                ? "Link install complete. The package was downloaded from the link and fed straight to the console — nothing was staged on this computer or the PS5."
                : "Stream-install complete. The pkg was fetched over HTTP — nothing was staged on the PS5.") +
              viaBridge,
          });
          return finishStreamTask({ ok: true, mayNotLaunch: false });
        }
        // Stall / async failure. Nothing was staged on the PS5 so there's
        // no pkg to keep — the pkg-host session is engine-side only.
        return finishStreamTask({
          ok: false,
          acceptedUnverified: verdict.acceptedUnverified,
          stagedFallbackRecommended: true,
          rc: dpi.rc,
          requestsServed: dpi.requestsServed,
          message: verdict.acceptedUnverified
            ? "The PS5 accepted the stream-install request, but ps5upload couldn’t verify completion. Check the PS5 home screen and Notifications / Downloads; the original package on your computer is unchanged."
            : verdict.message || "The install didn't complete.",
        });
      } catch (e) {
        // An unexpected failure leaves the console's state unknown — a call that
        // threw (a client timeout mid-hand-off) can still have queued a real
        // install. Hold the session rather than hang up on it; the engine age-GCs
        // it either way.
        releaseServingSession = false;
        return finishStreamTask({ ok: false, message: pkgError(e) });
      } finally {
        // Cancel the serving session ONLY when it reached a real terminal state.
        // `pkg_install_cancel` sets the session `cancelled`, and serve_handler
        // then answers every subsequent fetch with 410 Gone — so cancelling a
        // session whose install is still running hangs up on the console
        // mid-transfer and turns a live install into a failed one. That is
        // reachable any time `verifyInstallCompleted` gives up early (its
        // safety cap, or five consecutive poll errors), and it was the default
        // path for every stream install before this guard.
        if (servingSession && releaseServingSession) {
          try {
            await invoke("pkg_install_cancel", { session: servingSession });
          } catch {
            /* best-effort; the engine also age-GCs sessions */
          }
        }
        if (!taskFinished) {
          finishStreamTask({
            ok: false,
            message: "The stream install ended without a result.",
          });
        }
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
        try {
          const m = await pkgMetadataConsole(transferAddr(host), pkg.path);
          if (m?.contentId) contentId = m.contentId;
        } catch {
          // Best-effort enrichment: the engine still parses the completed
          // internal copy before installing it.
        }
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
      let installAttempted = false;
      // Live install %, shared by the direct-from-USB and the copy-fallback paths.
      const onProgress = (sample: InstallSample) => {
        if (sample.total > 0) {
          const pct = Math.min(
            99,
            Math.floor((sample.installedBytes / sample.total) * 100),
          );
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

        installAttempted = true;
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
          undefined,
          // The USB scan knows the pkg's size; without it the background
          // re-verify has nothing to match an installed artifact against and
          // could never confirm this install.
          pkg.size > 0 ? { size: pkg.size } : undefined,
        );
        // Only a CONFIRMED completion authorizes deletion. A request that Sony
        // merely accepted can still be installing (or can fail asynchronously),
        // and deleting here was the USB equivalent of the large-PKG data-loss
        // bug. The original USB file is untouched, and the internal copy remains
        // available for a retry until confirmed or the payload's stale sweep.
        if (viaCopy.installed) {
          await fsDelete(mgmtAddr(host), internalPath).catch(() => {});
        }

        return viaCopy.installed
          ? { ok: true, mayNotLaunch: viaCopy.mayNotLaunch }
          : {
              ok: false,
              acceptedUnverified: viaCopy.acceptedUnverified,
              message:
                (viaCopy.errMessage || "Install was rejected.") +
                ` Internal staging was kept at ${internalPath}.`,
            };
      } catch (e) {
        // A partial copy is disposable, but once an install call has begun its
        // outcome may be unknown. Preserve the complete staging copy in that
        // case instead of risking deletion underneath Sony's async installer.
        if (!installAttempted) {
          await fsDelete(mgmtAddr(host), internalPath).catch(() => {});
        }
        return {
          ok: false,
          message:
            pkgError(e) +
            (installAttempted
              ? ` Internal staging was kept at ${internalPath}.`
              : ""),
        };
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
        const { installed, mayNotLaunch, errMessage, acceptedUnverified } =
          await runPkgInstall(
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
          : {
              ok: false,
              acceptedUnverified,
              message: errMessage || "Install was rejected.",
            };
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
      (!e.appVer || (e.fingerprint?.length ?? 0) < 64) &&
      !metaEnrichAttempted.has(`${hostOf(host)}:${e.path}`),
  );
  for (const t of targets) {
    if (get().installing) return; // never read while the payload is swapped
    const key = `${hostOf(host)}:${t.path}`;
    metaEnrichAttempted.add(key);
    const m = await pkgMetadataConsole(transferAddr(host), t.path, t.size);
    if (!m || (!m.appVer && !m.category && !m.title && !m.fingerprint))
      continue;
    // Persist so a restart (and mergeListing) keep it without re-reading.
    cachePathMeta(host, t.path, {
      appVer: m.appVer || undefined,
      category: m.category || undefined,
      fingerprint: m.fingerprint || undefined,
      authenticity: m.authenticity,
    });
    set((s) => ({
      entries: s.entries.map((x) =>
        x.path === t.path
          ? {
              ...x,
              appVer: x.appVer || m.appVer || undefined,
              category: m.category || x.category,
              title: x.title || m.title || undefined,
              fingerprint: m.fingerprint || x.fingerprint,
              authenticity: m.authenticity,
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
