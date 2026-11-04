// An archive (ZIP, 7z or RAR) that holds .pkg files (R6, #370): unpack the packages straight to the
// console over AVA1 (the archive is read from this computer, nothing is extracted to its
// disk), then queue one install per package from the console path.
//
// Rules this module keeps, each learned the hard way (see the notes in the memory index):
//  * Only the packages are unpacked ("!*.pkg" allow-list), so a RAR with a 60 GB game
//    folder and a readme costs the packages, not the folder.
//  * A patch shares its base's content id, so every install fallback tier WIPES the base.
//    A base installs first, then its patch; the patch goes through `installFromConsolePath`,
//    where the engine reads the package category and arms the safe route (the patch is never
//    sent down a fallback that replaces the base). If a base fails, its patches and DLC are
//    skipped rather than sent after a base that is not there.
//  * The unpacked folder is removed only after the WHOLE run ends with every package
//    installed (each queue item reads its file in place until then). When any install
//    fails the folder stays where it is, so the user (or a retry) can use it; the card
//    offers to remove such leftovers later. `deleteStaging` is never asked for.
//  * The free-space check before unpacking refuses only what certainly cannot fit
//    (more than the drive's allocatable bytes); "may not fit" is a warning, never a refusal.
//  * Every install is a queue item (`installFromConsolePath` enqueues it); this module
//    awaits each in turn and never re-runs one.

import { pkgConsoleProbe, rarPackages, type RarPackage } from "../api/links";
import {
  fetchVolumes,
  fsDelete,
  fsListDir,
  fsMkdir,
  jobStatus,
  volumeAllocatableBytes,
  volumeForPath,
  volumeLikelyFitsBytes,
  startTransfer7z,
  startTransferRar,
  startTransferZip,
  type JobSnapshot,
  type Volume,
} from "../api/ps5";
import { consoleAddr, transferAddr } from "../lib/addr";
import { formatBytes } from "../lib/format";
import { staticTranslator as tx, trStatic } from "../lib/trStatic";
import {
  libraryDirs,
  pkgMkdirChain,
  pkgStorageFor,
} from "../lib/pkgStorage";
import { rarPasswordProblem, type RarPasswordProblem } from "../lib/rarPassword";
import { isRarFirstVolume } from "../lib/rarVolumes";
import { splitPkgBytes, type ListedFile } from "../lib/splitPkgSize";
import { pkgLibraryStore } from "./pkgLibrary";

/** The exclude entry that keeps only packages; the engine lists with the same one. */
export const RAR_PKG_ALLOW = "!*.pkg";
export const RAR_FPKG_ALLOW = "!*.fpkg";

export { isRarFirstVolume };

export type ArchiveKind = "zip" | "7z" | "rar";

/** Which reader an archive needs, from its name. A split 7z (`.7z.001`) is not readable by
 *  the engine and is not claimed. */
export function archiveKindOf(path: string): ArchiveKind | null {
  const lower = (path.replace(/\\/g, "/").split("/").pop() ?? "").toLowerCase();
  if (lower.endsWith(".zip")) return "zip";
  if (lower.endsWith(".7z")) return "7z";
  if (lower.endsWith(".rar")) return "rar";
  return null;
}

/** True for a file an install can start from: any ZIP or 7z, and the first (or only) part
 *  of a RAR set. */
export function isArchiveFirstVolume(path: string): boolean {
  const kind = archiveKindOf(path);
  if (kind === "rar") return isRarFirstVolume(path);
  return kind !== null;
}

/** Folder name for an archive's packages: `<kind>_<stem>`, deterministic so a resumed run
 *  lands in the same place. Letters of every script stay: two archives named only in Chinese
 *  must not share a folder. */
export function unpackFolderName(archivePath: string): string {
  const name = archivePath.replace(/\\/g, "/").split("/").pop() ?? "archive";
  const kind = archiveKindOf(archivePath) ?? "rar";
  const stem = name
    .replace(/\.part0*\d+\.rar$/i, "")
    .replace(/\.(rar|zip|7z)$/i, "")
    .replace(/[^\p{L}\p{N}._-]+/gu, "_")
    .replace(/^[._]+|[._]+$/g, "");
  return `${kind}_${stem || "archive"}`.slice(0, 80);
}

/** True for a folder name `unpackFolderName` makes (`rar_…`, `zip_…`, `7z_…`). */
export function isArchiveFolderName(name: string): boolean {
  return /^(rar|zip|7z)_[\p{L}\p{N}._-]+$/u.test(name);
}

/** Bytes the packages take once unpacked: the sum of their sizes in the archive listing. */
export function archiveNeedBytes(packages: readonly RarPackage[]): number {
  return packages.reduce((n, p) => n + (p.size > 0 ? p.size : 0), 0);
}

/** The drive a folder on the console is on. Internal storage is published as `/user` on
 *  some consoles and `/data` on others (one partition), and a payload may leave `/user`
 *  out, so an internal folder takes whichever of the two is there. */
export function volumeForConsoleDir(
  volumes: readonly Volume[],
  dir: string,
): Volume | null {
  const real = volumes.filter((v) => !v.is_placeholder);
  if (dir.startsWith("/user/") || dir.startsWith("/data/")) {
    return (
      real.find((v) => v.path === "/user") ??
      real.find((v) => v.path === "/data") ??
      null
    );
  }
  const v = volumeForPath(real, dir);
  return v && v.path !== "/" ? v : null;
}

export interface ArchiveSpace {
  /** fits; tight = may not fit (a warning); no_room = certainly does not fit (refused);
   *  unknown = the drive's free space could not be read (not refused). */
  kind: "fits" | "tight" | "no_room" | "unknown";
  /** Bytes still to write (what is already in the folder from an earlier run is not
   *  counted twice). */
  need: number;
  /** Bytes the drive can take, or null when unknown. */
  room: number | null;
  /** The drive, as the console names it (`/user`, `/mnt/ext0`…), or null when unknown. */
  drive: string | null;
}

/** Will `needBytes` of packages fit in `dest`? `already` = bytes in `dest` from an earlier
 *  run (the unpack writes over them). Refuses only what certainly cannot fit — more than
 *  the drive's allocatable bytes; past the "likely fits" estimate it only warns. */
export function archiveSpaceCheck(
  volumes: readonly Volume[] | null,
  dest: string,
  needBytes: number,
  already = 0,
): ArchiveSpace {
  const need = Math.max(0, needBytes - Math.max(0, already));
  const volume = volumes ? volumeForConsoleDir(volumes, dest) : null;
  if (!volume) return { kind: "unknown", need, room: null, drive: null };
  const room = volumeAllocatableBytes(volume);
  const likely = volumeLikelyFitsBytes(volume);
  // The unpacked copy stays until the install has read it, and a game installs to internal
  // storage by default: there the install needs its room on top. That only warns.
  const internal = volume.path === "/user" || volume.path === "/data";
  const atPeak = internal ? need + Math.max(0, needBytes) : need;
  const kind = need > room ? "no_room" : atPeak > likely ? "tight" : "fits";
  return { kind, need, room, drive: volume.path };
}

/** Remove the unpacked folder only when the run installed every package. Anything else
 *  (a failure, a skip, nothing installed) keeps it so a retry can use the files. */
export function shouldRemoveUnpacked(
  outcomes: readonly { status: RarInstallStatus }[],
): boolean {
  return outcomes.length > 0 && outcomes.every((o) => o.status === "installed");
}

/** Bytes under a console folder (files only, walked a few levels deep). 0 when it is not
 *  there. */
export async function consoleFolderBytes(
  host: string,
  dir: string,
  depth = 6,
): Promise<number> {
  let entries;
  try {
    entries = await fsListDir(transferAddr(host), dir);
  } catch {
    return 0;
  }
  let total = 0;
  for (const e of entries) {
    if (e.kind === "file") total += e.size > 0 ? e.size : 0;
    else if (e.kind === "dir" && depth > 0)
      total += await consoleFolderBytes(host, `${dir}/${e.name}`, depth - 1);
  }
  return total;
}

export interface ArchiveTarget {
  /** Console folder the packages will be unpacked into. */
  dest: string;
  /** Bytes already in that folder from an earlier run. */
  already: number;
  space: ArchiveSpace;
}

/** Where an archive's packages will be unpacked on this console and whether they fit,
 *  before anything is sent. The drive is the package library's (Volumes, "Use for
 *  packages"), or internal storage. */
export async function planArchiveTarget(
  host: string,
  archivePath: string,
  packages: readonly RarPackage[],
  volumes?: Volume[] | null,
): Promise<ArchiveTarget> {
  const vols =
    volumes !== undefined
      ? volumes
      : await fetchVolumes(transferAddr(host)).catch(() => null);
  const storage = pkgStorageFor(host, vols);
  const dest = `${storage.dir}/${unpackFolderName(archivePath)}`;
  const already = await consoleFolderBytes(host, dest);
  return {
    dest,
    already,
    space: archiveSpaceCheck(vols, dest, archiveNeedBytes(packages), already),
  };
}

export interface ArchiveLeftover {
  path: string;
  bytes: number;
}

/** Unpacked-archive folders in a package library listing, minus those still in use. */
export function leftoverArchiveFolders(
  dir: string,
  entries: readonly { name: string; kind: string }[],
  inUse: (path: string) => boolean,
): string[] {
  return entries
    .filter((e) => e.kind === "dir" && isArchiveFolderName(e.name))
    .map((e) => `${dir}/${e.name}`)
    .filter((p) => !inUse(p));
}

/** Folders left by earlier archive runs that failed or were interrupted, on every package
 *  library of the console. `inUse(path)` = a run or a queued install still needs it. */
export async function findArchiveLeftovers(
  host: string,
  inUse: (path: string) => boolean,
): Promise<ArchiveLeftover[]> {
  const volumes = await fetchVolumes(transferAddr(host)).catch(() => null);
  const out: ArchiveLeftover[] = [];
  for (const dir of libraryDirs(volumes)) {
    let entries;
    try {
      entries = await fsListDir(transferAddr(host), dir);
    } catch {
      continue; // no library on that drive yet
    }
    for (const path of leftoverArchiveFolders(dir, entries, inUse)) {
      out.push({ path, bytes: await consoleFolderBytes(host, path) });
    }
  }
  return out;
}

/** Deletes an unpacked folder from the console, retrying briefly: right after an install
 *  Sony's installer can still hold a file for a moment. `mayDelete` is asked before each try
 *  (something may have started using the folder meanwhile): false leaves it, and resolves false. */
export async function removeConsoleFolder(
  host: string,
  path: string,
  wait: (ms: number) => Promise<void> = (ms) =>
    new Promise((r) => setTimeout(r, ms)),
  mayDelete: () => boolean = () => true,
): Promise<boolean> {
  let last: unknown;
  for (let attempt = 0; attempt < 3; attempt++) {
    if (attempt > 0) await wait(1000);
    if (!mayDelete()) return false;
    try {
      await fsDelete(transferAddr(host), path);
      return true;
    } catch (e) {
      last = e;
    }
  }
  throw last;
}

/** Install tier: base (0) before patch (1) before DLC (2). */
export function packageTier(category: string): number {
  if (category === "gp") return 1;
  if (category === "ac") return 2;
  return 0;
}

export interface RarPackageInfo {
  /** Path inside the archive. */
  entry: string;
  /** Where it is on the console once unpacked. */
  consolePath: string;
  /** Its size from the archive listing (0 when unknown). */
  size?: number;
  category: string;
  titleId: string;
  appVer: string;
}

/** Base -> patch -> DLC; within a tier by title, then ascending version, then path (stable). */
export function orderForInstall<T extends Pick<RarPackageInfo, "category" | "titleId" | "appVer" | "entry">>(
  items: T[],
): T[] {
  return [...items].sort(
    (a, b) =>
      packageTier(a.category) - packageTier(b.category) ||
      a.titleId.localeCompare(b.titleId) ||
      a.appVer.localeCompare(b.appVer, undefined, { numeric: true }) ||
      a.entry.localeCompare(b.entry),
  );
}

export type RarInstallStatus = "installed" | "failed" | "skipped";

export interface RarInstallOutcome {
  entry: string;
  consolePath: string;
  status: RarInstallStatus;
  message?: string;
}

export type RarPhase =
  | { phase: "listing" }
  | { phase: "unpacking"; sent: number; total: number }
  | { phase: "reading"; index: number; count: number }
  | { phase: "installing"; index: number; count: number; name: string };

export interface RarInstallResult {
  ok: boolean;
  /** Console folder the packages were unpacked into (kept in place). */
  dest: string;
  outcomes: RarInstallOutcome[];
  /** One sentence for the whole run. */
  message: string;
  /** The archive needs (or rejected) a password: ask again. */
  password?: RarPasswordProblem;
  /** The unpack's status could not be read any more: this engine job may still be writing. */
  lostJob?: string;
  /** The space check refused the run (nothing was sent). */
  space?: ArchiveSpace;
  /** Every package installed and the unpacked folder was deleted. */
  removed?: boolean;
  /** Every package installed but deleting the folder failed: why. */
  removeError?: string;
}

export interface RarInstallOptions {
  host: string;
  archivePath: string;
  password?: string | null;
  onPhase?: (p: RarPhase) => void;
  /** The unpack's engine job while it runs, so it can be stopped; null once it has ended. */
  onUnpackJob?: (jobId: string | null) => void;
}

/** How many status reads in a row may fail before an unpack is given up on. It goes on in the
 *  engine meanwhile, so one blip must not end the run and free its folder. */
const UNPACK_STATUS_FAILURES = 30;

const baseName = (p: string) => p.split("/").pop() ?? p;

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** What a RAR holds, before anything is sent: its packages, or a password problem. */
export async function listRarPackages(
  archivePath: string,
  password?: string | null,
): Promise<{ packages: RarPackage[]; password?: RarPasswordProblem; error?: string }> {
  try {
    return { packages: await rarPackages(archivePath, password) };
  } catch (e) {
    const msg = messageOf(e);
    const pw = rarPasswordProblem(null, msg);
    return { packages: [], ...(pw ? { password: pw } : {}), error: msg };
  }
}

async function unpack(
  opts: RarInstallOptions,
  dest: string,
): Promise<
  | { ok: true }
  | { ok: false; message: string; password?: RarPasswordProblem; lostJob?: string }
> {
  let jobId: string;
  try {
    const addr = consoleAddr(opts.host);
    const kind = archiveKindOf(opts.archivePath);
    // Each kind has its own reader in the engine; all three take the same allow-list. Only
    // RAR can be opened with a password.
    jobId =
      kind === "zip"
        ? await startTransferZip(opts.archivePath, dest, addr, null, [RAR_PKG_ALLOW, RAR_FPKG_ALLOW])
        : kind === "7z"
          ? await startTransfer7z(opts.archivePath, dest, addr, null, [RAR_PKG_ALLOW, RAR_FPKG_ALLOW])
          : await startTransferRar(
              opts.archivePath,
              dest,
              addr,
              opts.password ?? null,
              null,
              [RAR_PKG_ALLOW, RAR_FPKG_ALLOW],
            );
  } catch (e) {
    const msg = messageOf(e);
    const pw = rarPasswordProblem(null, msg);
    return { ok: false, message: msg, ...(pw ? { password: pw } : {}) };
  }
  opts.onUnpackJob?.(jobId);
  let failedReads = 0;
  for (;;) {
    let snap: JobSnapshot;
    try {
      snap = await jobStatus(jobId, opts.host);
      failedReads = 0;
    } catch (e) {
      failedReads += 1;
      if (failedReads >= UNPACK_STATUS_FAILURES) {
        // The folder may still be in use: the job id stays with the caller.
        return { ok: false, message: messageOf(e), lostJob: jobId };
      }
      await new Promise((r) => setTimeout(r, 1000));
      continue;
    }
    if (snap.status === "done") {
      opts.onUnpackJob?.(null);
      return { ok: true };
    }
    if (snap.status === "failed") {
      opts.onUnpackJob?.(null);
      const msg = snap.error ?? trStatic("arcpkg.unpack_failed", "The unpack failed.");
      const pw = rarPasswordProblem(snap.error_reason, msg);
      return { ok: false, message: msg, ...(pw ? { password: pw } : {}) };
    }
    opts.onPhase?.({
      phase: "unpacking",
      sent: snap.bytes_sent ?? 0,
      total: snap.total_bytes ?? 0,
    });
    await new Promise((r) => setTimeout(r, 500));
  }
}

function baseOf(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** The other listed packages in `path`'s folder of the archive, by name. */
export function siblingsOf(path: string, all: readonly RarPackage[]): ListedFile[] {
  const dir = path.slice(0, path.lastIndexOf("/") + 1);
  return all
    .filter((p) => p.path !== path && p.path.startsWith(dir) && !p.path.slice(dir.length).includes("/"))
    .map((p) => ({ name: baseOf(p.path), size: p.size }));
}

/** Unpack a RAR's packages to the console and install each, base first. */
export async function installRarPackages(
  opts: RarInstallOptions,
): Promise<RarInstallResult> {
  const { host, archivePath } = opts;
  opts.onPhase?.({ phase: "listing" });
  const listed = await listRarPackages(archivePath, opts.password);
  if (listed.error) {
    return {
      ok: false,
      dest: "",
      outcomes: [],
      message: listed.error,
      ...(listed.password ? { password: listed.password } : {}),
    };
  }
  if (listed.packages.length === 0) {
    return {
      ok: false,
      dest: "",
      outcomes: [],
      message: trStatic("rarpkg.none", "That archive has no .pkg files in it."),
    };
  }

  // The package library of the console's default drive, or internal storage: a place Sony's
  // installer can read, and the one the Library screen already lists.
  // Refuse before sending anything when the packages certainly cannot fit.
  const { dest, space } = await planArchiveTarget(host, archivePath, listed.packages);
  if (space.kind === "no_room") {
    return {
      ok: false,
      dest,
      outcomes: [],
      space,
      message: tx(
        "arcpkg.space_refused",
        { drive: space.drive ?? "", need: formatBytes(space.need), room: formatBytes(space.room ?? 0) },
        "Not enough space on {drive}: the packages need {need}, and it has {room} free. Nothing was unpacked.",
      ),
    };
  }
  for (const dir of pkgMkdirChain(dest)) {
    await fsMkdir(transferAddr(host), dir).catch(() => {});
  }

  const unpacked = await unpack(opts, dest);
  if (!unpacked.ok) {
    return {
      ok: false,
      dest,
      outcomes: [],
      message: unpacked.message,
      ...(unpacked.lostJob ? { lostJob: unpacked.lostJob } : {}),
      ...(unpacked.password ? { password: unpacked.password } : {}),
    };
  }

  // Identify each package where it now is, to put a base before its patch.
  const outcomes: RarInstallOutcome[] = [];
  const known: RarPackageInfo[] = [];
  for (const [i, p] of listed.packages.entries()) {
    opts.onPhase?.({ phase: "reading", index: i + 1, count: listed.packages.length });
    const consolePath = `${dest}/${p.path}`;
    try {
      const info = await pkgConsoleProbe(consoleAddr(host), consolePath);
      known.push({
        entry: p.path,
        consolePath,
        // A split set's lead carries its parts (same folder in the archive).
        size: splitPkgBytes(baseOf(p.path), p.size, siblingsOf(p.path, listed.packages)),
        category: info.category ?? "",
        titleId: info.title_id ?? "",
        appVer: info.app_ver ?? "",
      });
    } catch (e) {
      outcomes.push({
        entry: p.path,
        consolePath,
        status: "failed",
        message: tx(
          "arcpkg.not_readable",
          { error: messageOf(e), path: consolePath },
          "Not a readable package ({error}). It was left at {path}.",
        ),
      });
    }
  }

  const ordered = orderForInstall(known);
  const failedBases = new Set<string>();
  for (const [i, pkg] of ordered.entries()) {
    const tier = packageTier(pkg.category);
    if (tier > 0 && pkg.titleId && failedBases.has(pkg.titleId)) {
      outcomes.push({
        entry: pkg.entry,
        consolePath: pkg.consolePath,
        status: "skipped",
        message: tx(
          "arcpkg.skipped_no_base",
          { id: pkg.titleId, path: pkg.consolePath },
          "Skipped: the base game {id} did not install, and an update or DLC cannot go on without it. The file was left at {path}.",
        ),
      });
      continue;
    }
    opts.onPhase?.({
      phase: "installing",
      index: i + 1,
      count: ordered.length,
      name: baseName(pkg.entry),
    });
    // One queue item per package; never deleted afterwards (the queue item reads the file in
    // place), and an update takes the engine's safe route because it reads the category.
    const r = await pkgLibraryStore(host).getState().installFromConsolePath(pkg.consolePath, host, {
      sizeBytes: pkg.size ?? null,
    });
    if (r.ok) {
      outcomes.push({ entry: pkg.entry, consolePath: pkg.consolePath, status: "installed" });
    } else {
      if (tier === 0 && pkg.titleId) failedBases.add(pkg.titleId);
      outcomes.push({
        entry: pkg.entry,
        consolePath: pkg.consolePath,
        status: "failed",
        message: tx(
          "arcpkg.install_failed",
          { error: r.message ?? trStatic("pkg.not_complete", "The install didn't complete."), path: pkg.consolePath },
          "{error} The file was left at {path}.",
        ),
      });
    }
  }

  const done = outcomes.filter((o) => o.status === "installed").length;
  const ok = shouldRemoveUnpacked(outcomes);
  if (!ok) {
    return {
      ok,
      dest,
      outcomes,
      message: tx(
        "arcpkg.partly_done",
        { done, total: outcomes.length, dest },
        "Installed {done} of {total}. The unpacked packages are still in {dest}.",
      ),
    };
  }
  // Every install has ended (each was awaited), so nothing reads these files any more.
  const installed = tx("arcpkg.all_done", { n: done }, "Packages installed from the archive: {n}.");
  try {
    await removeConsoleFolder(host, dest);
    return {
      ok,
      dest,
      outcomes,
      removed: true,
      message: `${installed} ${trStatic("arcpkg.folder_deleted", "The unpacked folder was deleted.")}`,
    };
  } catch (e) {
    const why = messageOf(e);
    return {
      ok,
      dest,
      outcomes,
      removeError: why,
      message: `${installed} ${tx("arcpkg.folder_not_deleted", { dest, error: why }, "The unpacked folder {dest} could not be deleted ({error}).")}`,
    };
  }
}

