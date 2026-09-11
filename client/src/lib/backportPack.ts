/** Installing a community "backport pack".
 *
 *  A pack is how backports are actually distributed, and it is a superset of
 *  what the library-set flow does: alongside `fakelib/` it carries an
 *  `eboot.bin` ALREADY patched to the backport SDK pair, plus replacement
 *  `sce_module/` modules. Installing one is therefore a file copy and NOT an
 *  SDK patch — patching a pre-patched eboot would rewrite a field that is
 *  already correct and invalidate the fake-signature the pack shipped with.
 *
 *  This deliberately does not reuse `BackportRecord`/`undoBackport`. That undo
 *  restores `fakelib/` only and then calls `sdk/restore`, which reverts from
 *  the `.bak` files the SDK patch leaves behind — a pack install writes files
 *  outside `fakelib/` and creates no `.bak`, so reusing it would silently fail
 *  to put back the eboot it overwrote.
 */

import type { InstalledTitle } from "../api/ps5";

/** Where per-title backups live. Same root the library flow uses, so a user
 *  looking for "what did this tool leave on my console" finds one place. */
export const PACK_STASH_ROOT = "/data/ps5upload/backport";

export interface PackFile {
  /** Path relative to the pack root, always with `/` separators. */
  relPath: string;
  size: number;
}

export interface BackportPack {
  isPack: boolean;
  /** Title id read out of the folder name. A HINT, never an authority. */
  titleIdHint: string | null;
  libraries: PackFile[];
  eboot: PackFile | null;
  sceModules: PackFile[];
  /** The game's own engine plugins (`prx/`). Title-specific, so never corpus
   *  material — but a pack that ships them expects them installed. */
  gamePrx: PackFile[];
  /** `sce_sys/about/` — present in every pack seen so far. */
  sceSys: PackFile[];
  other: PackFile[];
  totalBytes: number;
}

export interface PackCopy {
  /** Absolute path on the user's computer. */
  from: string;
  /** Absolute path on the console. */
  to: string;
  size: number;
}

export interface PackInstallPlan {
  target: InstalledTitle;
  packRoot: string;
  copies: PackCopy[];
  /** Live console files this install would overwrite, and where each original
   *  gets stashed. Only files that actually exist appear here. */
  stashed: { live: string; stash: string }[];
  stashDir: string;
  /** The pack names a different title than the one selected. Not fatal — the
   *  name is only a folder string — but the user must confirm, because the
   *  alternative is overwriting a 256 MB eboot with another game's. */
  titleMismatch: boolean;
  totalBytes: number;
}

export interface PackInstallRecord {
  titleId: string;
  targetSource: string;
  packRoot: string;
  copiedPaths: string[];
  stashed: { live: string; stash: string }[];
  stashDir: string;
  /** False when apply threw partway. Undo must still be offered: a title with
   *  half a pack installed launches and aborts. */
  complete: boolean;
}

export interface PackTransport {
  mkdirConsole(path: string): Promise<void>;
  copyConsole(from: string, to: string): Promise<void>;
  uploadHost(from: string, to: string): Promise<void>;
  remove(path: string): Promise<void>;
}

export class PackInstallError extends Error {
  record: PackInstallRecord;
  constructor(message: string, record: PackInstallRecord) {
    super(message);
    this.name = "PackInstallError";
    this.record = record;
  }
}

/** The basename of a pack-relative path. */
function base(relPath: string): string {
  return relPath.split("/").pop() ?? relPath;
}

/** A pack is third-party data, so every relative path is re-checked here even
 *  though the engine already classified it. The engine and the client can be
 *  different versions — a browser talking to an older engine is a supported
 *  configuration — and this one is cheap. */
function isSafeRelPath(relPath: string): boolean {
  if (!relPath || relPath.startsWith("/") || relPath.includes("\\")) return false;
  const parts = relPath.split("/");
  if (parts.some((p) => p === "" || p === "." || p === "..")) return false;
  return true;
}

/** What a pack install would do, without doing any of it. */
export function planPackInstall(
  pack: BackportPack,
  target: InstalledTitle,
  packRoot: string,
  existing: {
    fakelib: string[];
    sceModule: string[];
    gamePrx: string[];
    sceSys: string[];
    eboot: boolean;
  },
): PackInstallPlan {
  if (!pack.isPack) {
    throw new Error("This folder has no fakelib/, so it is not a backport pack");
  }
  if (!target.source) {
    throw new Error(`${target.titleId} has no source folder on the console`);
  }
  const root = packRoot.replace(/\/+$/, "");
  const dest = target.source.replace(/\/+$/, "");
  const stashDir = `${PACK_STASH_ROOT}/${target.titleId}`;

  const copies: PackCopy[] = [];
  const stashed: { live: string; stash: string }[] = [];

  const add = (file: PackFile, consolePath: string, alreadyThere: boolean, stashName: string) => {
    if (!isSafeRelPath(file.relPath)) {
      throw new Error(`Unsafe path in pack: ${file.relPath}`);
    }
    copies.push({ from: `${root}/${file.relPath}`, to: consolePath, size: file.size });
    if (alreadyThere) stashed.push({ live: consolePath, stash: `${stashDir}/${stashName}` });
  };

  for (const lib of pack.libraries) {
    const name = base(lib.relPath);
    add(lib, `${dest}/fakelib/${name}`, existing.fakelib.includes(name), `fakelib__${name}`);
  }
  for (const mod of pack.sceModules) {
    const name = base(mod.relPath);
    add(mod, `${dest}/sce_module/${name}`, existing.sceModule.includes(name), `sce_module__${name}`);
  }
  // Found by inspecting a second real pack: the first one shipped none of
  // these, and a classifier built from that single sample dropped all 26 of
  // the next pack's engine plugins on the floor.
  for (const plugin of pack.gamePrx) {
    const name = base(plugin.relPath);
    add(plugin, `${dest}/prx/${name}`, existing.gamePrx.includes(name), `prx__${name}`);
  }
  for (const f of pack.sceSys) {
    const name = base(f.relPath);
    add(f, `${dest}/sce_sys/about/${name}`, existing.sceSys.includes(name), `sce_sys_about__${name}`);
  }
  // The eboot goes LAST for the same reason the library flow patches the SDK
  // last: if anything before it fails, the title still has its original eboot
  // and still runs exactly as it did. Swapping the eboot first and then
  // failing leaves a game that launches into a crash.
  if (pack.eboot) {
    add(pack.eboot, `${dest}/eboot.bin`, existing.eboot, "eboot.bin");
  }

  return {
    target,
    packRoot: root,
    copies,
    stashed,
    stashDir,
    titleMismatch: !!pack.titleIdHint && pack.titleIdHint !== target.titleId,
    totalBytes: copies.reduce((sum, c) => sum + c.size, 0),
  };
}

export async function applyPackInstall(
  plan: PackInstallPlan,
  transport: PackTransport,
): Promise<PackInstallRecord> {
  const record: PackInstallRecord = {
    titleId: plan.target.titleId,
    targetSource: plan.target.source,
    packRoot: plan.packRoot,
    copiedPaths: [],
    stashed: [],
    stashDir: plan.stashDir,
    complete: false,
  };
  try {
    if (plan.stashed.length > 0) await transport.mkdirConsole(plan.stashDir);
    // Back everything up BEFORE writing anything, so a failure midway through
    // the copies still has every original available to undo from.
    for (const item of plan.stashed) {
      await transport.copyConsole(item.live, item.stash);
      record.stashed.push(item);
    }
    for (const dir of ["fakelib", "sce_module", "prx", "sce_sys", "sce_sys/about"]) {
      await transport.mkdirConsole(`${plan.target.source}/${dir}`);
    }
    for (const copy of plan.copies) {
      await transport.uploadHost(copy.from, copy.to);
      record.copiedPaths.push(copy.to);
    }
  } catch (error) {
    throw new PackInstallError(
      error instanceof Error ? error.message : String(error),
      record,
    );
  }
  record.complete = true;
  return record;
}

export async function undoPackInstall(
  record: PackInstallRecord,
  transport: PackTransport,
): Promise<void> {
  const restored = new Set(record.stashed.map((s) => s.live));
  // Anything we added that was not displacing an original is pure addition —
  // remove it. Reverse order so the eboot goes first, mirroring install.
  for (const path of [...record.copiedPaths].reverse()) {
    if (!restored.has(path)) await transport.remove(path);
  }
  // Put the originals back over the top of what we wrote.
  for (const item of record.stashed) {
    await transport.remove(item.live);
    await transport.copyConsole(item.stash, item.live);
  }
  // Non-semantic: once the originals are back, a leftover backup folder is
  // recoverable and far safer than reporting a failed Undo.
  try {
    await transport.remove(record.stashDir);
  } catch {
    /* keep the backup rather than fail the undo */
  }
}
