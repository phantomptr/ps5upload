// What ps5upload itself keeps on a console, and how big it is.
//
// The app writes to a handful of folders of its own: the package library and its temp folder
// (on whichever drive holds packages), save backups, and test files. They are easy to
// forget and can be large, so Volumes lists them with their sizes and offers to empty the
// ones that hold nothing the user would miss. Sizes are measured by listing the folders
// through the helper, the same way the Disk usage screen does; nothing new runs on the PS5.

import { INTERNAL_PKG_DIR, isInternalVolume, pkgDirOnVolume } from "./pkgStorage";

export interface DirEntry {
  name: string;
  kind: string;
  size: number;
}

/** Lists one folder completely. Throws when the folder does not exist. */
export type ListDir = (path: string) => Promise<DirEntry[]>;

export interface FolderSize {
  exists: boolean;
  bytes: number;
  files: number;
  /** The walk stopped at its limit: `bytes` and `files` are a floor. */
  truncated: boolean;
}

/** Adds up every file under `path`, visiting at most `maxFiles` files. */
export async function measureFolder(list: ListDir, path: string, maxFiles = 50_000): Promise<FolderSize> {
  const out: FolderSize = { exists: false, bytes: 0, files: 0, truncated: false };
  const todo = [path];
  let first = true;
  while (todo.length > 0) {
    const dir = todo.pop() as string;
    let entries: DirEntry[];
    try {
      entries = await list(dir);
    } catch {
      // The top folder missing means "nothing kept here"; a subfolder that vanished
      // mid-walk (a clean-up running, a file being moved) is just skipped.
      if (first) return out;
      continue;
    }
    first = false;
    out.exists = true;
    for (const e of entries) {
      if (e.kind === "dir") {
        todo.push(`${dir}/${e.name}`);
        continue;
      }
      if (out.files >= maxFiles) {
        out.truncated = true;
        return out;
      }
      out.files += 1;
      out.bytes += e.size || 0;
    }
  }
  return out;
}

export type KeptKey = "pkg_library" | "pkg_temp" | "backups" | "tests";

export interface KeptFolder {
  key: KeptKey;
  /** The drive it is on, as Volumes names it (`/data`, `/mnt/ext0`). */
  drive: string;
  path: string;
  /** Holds nothing the user would miss: offered for clean-up. The package library and save
   *  backups are the user's own files, so they are shown and never offered. */
  cleanable: boolean;
}

/** The folders ps5upload owns, for these drives. */
export function keptFolders(drives: string[]): KeptFolder[] {
  const out: KeptFolder[] = [];
  for (const drive of drives) {
    const library = isInternalVolume(drive) ? INTERNAL_PKG_DIR : pkgDirOnVolume(drive);
    out.push({ key: "pkg_library", drive, path: library, cleanable: false });
    out.push({
      key: "pkg_temp",
      drive,
      path: library.replace(/\/pkg_library$/, "/pkg_temp"),
      cleanable: true,
    });
  }
  const internal = drives.find((d) => isInternalVolume(d));
  if (internal) {
    out.push({ key: "backups", drive: internal, path: "/data/ps5upload/backups", cleanable: false });
    out.push({ key: "tests", drive: internal, path: "/data/ps5upload/tests", cleanable: true });
  }
  return out;
}

export interface UsageRow extends KeptFolder, FolderSize {}

/** Every folder ps5upload keeps on these drives that exists and holds something. */
export async function ps5uploadUsage(drives: string[], list: ListDir): Promise<UsageRow[]> {
  const rows: UsageRow[] = [];
  for (const folder of keptFolders(drives)) {
    const size = await measureFolder(list, folder.path);
    if (size.exists && size.files > 0) rows.push({ ...folder, ...size });
  }
  return rows;
}
