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
//  * Nothing is deleted. The unpacked files stay where they are when an install fails, so
//    the user (or a retry) can use them; `deleteStaging` is never asked for.
//  * Every install is a queue item (`installFromConsolePath` enqueues it); this module
//    awaits each in turn and never re-runs one.

import { pkgConsoleProbe, rarPackages, type RarPackage } from "../api/links";
import {
  fetchVolumes,
  fsMkdir,
  jobStatus,
  startTransfer7z,
  startTransferRar,
  startTransferZip,
  type JobSnapshot,
} from "../api/ps5";
import { consoleAddr, transferAddr } from "../lib/addr";
import { pkgMkdirChain, pkgStorageFor } from "../lib/pkgStorage";
import { rarPasswordProblem, type RarPasswordProblem } from "../lib/rarPassword";
import { pkgLibraryStore } from "./pkgLibrary";

/** The exclude entry that keeps only packages; the engine lists with the same one. */
export const RAR_PKG_ALLOW = "!*.pkg";

/** True for the first (or only) volume of a RAR set: `.rar`, `.part1.rar`, `.part01.rar`. */
export function isRarFirstVolume(path: string): boolean {
  const name = path.replace(/\\/g, "/").split("/").pop() ?? "";
  const lower = name.toLowerCase();
  if (!lower.endsWith(".rar")) return false;
  const part = /\.part0*(\d+)\.rar$/.exec(lower);
  return part ? Number(part[1]) === 1 : true;
}

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
 *  lands in the same place. */
export function archiveFolderName(archivePath: string): string {
  const name = archivePath.replace(/\\/g, "/").split("/").pop() ?? "archive";
  const kind = archiveKindOf(archivePath) ?? "rar";
  const stem = name
    .replace(/\.part0*\d+\.rar$/i, "")
    .replace(/\.(rar|zip|7z)$/i, "")
    .replace(/[^A-Za-z0-9._-]+/g, "_")
    .replace(/^[._]+|[._]+$/g, "");
  return `${kind}_${stem || "archive"}`.slice(0, 80);
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
}

export interface RarInstallOptions {
  host: string;
  archivePath: string;
  password?: string | null;
  onPhase?: (p: RarPhase) => void;
}

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
): Promise<{ ok: true } | { ok: false; message: string; password?: RarPasswordProblem }> {
  let jobId: string;
  try {
    const addr = consoleAddr(opts.host);
    const kind = archiveKindOf(opts.archivePath);
    // Each kind has its own reader in the engine; all three take the same allow-list. Only
    // RAR can be opened with a password.
    jobId =
      kind === "zip"
        ? await startTransferZip(opts.archivePath, dest, addr, null, [RAR_PKG_ALLOW])
        : kind === "7z"
          ? await startTransfer7z(opts.archivePath, dest, addr, null, [RAR_PKG_ALLOW])
          : await startTransferRar(
              opts.archivePath,
              dest,
              addr,
              opts.password ?? null,
              null,
              [RAR_PKG_ALLOW],
            );
  } catch (e) {
    const msg = messageOf(e);
    const pw = rarPasswordProblem(null, msg);
    return { ok: false, message: msg, ...(pw ? { password: pw } : {}) };
  }
  for (;;) {
    let snap: JobSnapshot;
    try {
      snap = await jobStatus(jobId, opts.host);
    } catch (e) {
      return { ok: false, message: messageOf(e) };
    }
    if (snap.status === "done") return { ok: true };
    if (snap.status === "failed") {
      const msg = snap.error ?? "The unpack failed.";
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
      message: "That archive has no .pkg files in it.",
    };
  }

  // The package library of the console's default drive, or internal storage: a place Sony's
  // installer can read, and the one the Library screen already lists.
  const volumes = await fetchVolumes(transferAddr(host)).catch(() => null);
  const storage = pkgStorageFor(host, volumes);
  const dest = `${storage.dir}/${archiveFolderName(archivePath)}`;
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
        category: info.category ?? "",
        titleId: info.title_id ?? "",
        appVer: info.app_ver ?? "",
      });
    } catch (e) {
      outcomes.push({
        entry: p.path,
        consolePath,
        status: "failed",
        message: `Not a readable package (${messageOf(e)}). It was left at ${consolePath}.`,
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
        message: `Skipped: the base game ${pkg.titleId} did not install, and an update or DLC cannot go on without it. The file was left at ${pkg.consolePath}.`,
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
    const r = await pkgLibraryStore(host).getState().installFromConsolePath(pkg.consolePath, host);
    if (r.ok) {
      outcomes.push({ entry: pkg.entry, consolePath: pkg.consolePath, status: "installed" });
    } else {
      if (tier === 0 && pkg.titleId) failedBases.add(pkg.titleId);
      outcomes.push({
        entry: pkg.entry,
        consolePath: pkg.consolePath,
        status: "failed",
        message: `${r.message ?? "The install did not complete."} The file was left at ${pkg.consolePath}.`,
      });
    }
  }

  const done = outcomes.filter((o) => o.status === "installed").length;
  const ok = outcomes.length > 0 && done === outcomes.length;
  return {
    ok,
    dest,
    outcomes,
    message: ok
      ? `Installed ${done} package${done === 1 ? "" : "s"} from the archive.`
      : `Installed ${done} of ${outcomes.length}. The unpacked packages are still in ${dest}.`,
  };
}
