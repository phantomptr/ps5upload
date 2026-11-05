/**
 * Paste here: the clipboard's items go into a folder, one at a time.
 *
 * Each item's destination is <folder>/<its name>, read with /api/fs/stat
 * first. When something is already there the user chooses (never a silent
 * overwrite: rename and job.copy both refuse an existing destination):
 * - Skip: leave both alone.
 * - Keep both: the item takes the first free "name (2).ext", "name (3).ext"…
 * - Replace: a FILE replacing a file goes by a copy job with the overwrite
 *   flag (job.copy replaces files one by one through a .part + rename). A
 *   folder (or a file replacing a folder, or the other way round) goes through
 *   a temporary name beside the old item, which is deleted only once the new
 *   one has its place (replace.ts), because job.copy's overwrite MERGES folders
 *   and would leave the old folder's other files behind. A cut on the same
 *   drive is a rename, which never overwrites: it goes the same way.
 * "Apply to all" answers every later conflict of this paste the same way.
 *
 * Copy = copy job. Cut = rename on the same drive; across drives the helper
 * refuses the rename (cross_mount) and a copy job with the move flag does it
 * (a Replace copies, and deletes the original once the new item is in place).
 * A folder never goes into itself (IntoItselfError); a cut item pasted into
 * its own folder stays as it is.
 *
 * "The item itself" is decided by identity (/api/fs/stat's dev + ino), not by
 * the path's spelling: exFAT USB drives are case-insensitive, so a cut of
 * /mnt/usb0/Games/x pasted into a typed /mnt/usb0/games finds x itself at the
 * destination. Offering Replace there would delete the item, so an identical
 * entry is never replaceable, and for a cut it is "already here".
 * The destination folder is compared with each item's folder the same way. A
 * helper without ids falls back to the path compare.
 */
import { ApiError, call } from "../api";
import {
  baseName,
  each,
  isInside,
  IntoItselfError,
  joinPath,
  parentPath,
  startCopy,
  volumeOf,
  type BatchOptions,
  type BatchResult,
} from "./move";
import { renameChecked, ReplaceError, replaceItem, type Placed, type ReplaceSide } from "./replace";
import { keepBothName } from "../../lib/consolePath";

export type PasteMode = "copy" | "cut";
export type ConflictChoice = "skip" | "replace" | "keep";

export interface Conflict {
  /** The item being pasted (full path). */
  item: string;
  /** What is already there (full path). */
  dest: string;
  /** The existing one is a folder. */
  destDir: boolean;
  /** Replace is offered: false when the item itself is (inside) the existing one. */
  canReplace: boolean;
  /** Items after this one (Apply to all is offered when > 0). */
  left: number;
}

export interface Decision {
  choice: ConflictChoice;
  /** Answer every later conflict of this paste the same way. */
  all: boolean;
}

export interface PasteItem {
  path: string;
  dir: boolean;
}

export interface PasteResult extends BatchResult {
  /** Items now in the folder (moved, or their job ended done). */
  done: string[];
  /** Items whose job Stop left running (not known to be done: a cut keeps them in the clipboard). */
  left: string[];
  /** Items not pasted because Stop came while the copy for their Replace ran: what was there is as it was. */
  halted: string[];
  /** Conflicts the user skipped. */
  declined: string[];
  /** Cut items already in this folder (nothing to do). */
  here: string[];
  /** Replaces that left something to tell (a temporary or an old item that could not be deleted). */
  notes?: ReplaceError[];
}

export interface PasteDeps {
  /** Ask about one conflict (the page's dialog). */
  decide: (c: Conflict) => Promise<Decision>;
  /** The drives, to tell a same-drive cut (rename) from a cross-drive one (copy job + move). */
  volumes?: { path: string }[];
}

/** Keep both's tries before it gives up. */
const KEEP_BOTH_TRIES = 999;

export interface Stat {
  kind: string;
  /** Device and inode (decimal strings), from a helper that sends them. */
  dev?: string;
  ino?: string;
}

/** The same entry on disk (both stats carry ids and they match). */
export function sameEntry(a: Stat | null | undefined, b: Stat | null | undefined): boolean {
  return !!a && !!b && a.dev != null && a.ino != null && a.dev === b.dev && a.ino === b.ino;
}

/** The entry at `path`, or null when nothing is there (404). */
export async function statOrNull(path: string): Promise<Stat | null> {
  try {
    // missing_ok: the helper answers 200 {"missing":true} for a path that is not there (no 404
    // in the browser's console); an older helper ignores it and answers 404, read the same way.
    const st = await call<Stat | { missing: true }>("/api/fs/stat", { path, missing_ok: true });
    return "missing" in st && st.missing === true ? null : (st as Stat);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}

export { keepBothName };

/** The first free Keep both name for `name` in `dir`. */
export async function freeName(
  dir: string,
  name: string,
  isDir: boolean,
  taken: (path: string) => Promise<boolean>,
): Promise<string> {
  const dot = isDir ? -1 : name.lastIndexOf(".");
  const m = /^(.+) \((\d+)\)$/.exec(dot > 0 ? name.slice(0, dot) : name);
  const start = m ? Math.max(2, parseInt(m[2], 10) + 1) : 2;
  for (let n = start; n < start + KEEP_BOTH_TRIES; n++) {
    const candidate = keepBothName(name, n, isDir);
    if (!(await taken(joinPath(dir, candidate)))) return candidate;
  }
  throw new ApiError(409, "exists");
}

/** Why a copy job failed, when the helper can say: the no-space error a Replace reports by name. */
async function jobFailure(job: string): Promise<ApiError | null> {
  try {
    const s = await call<{ code?: unknown; message?: unknown } | null>("/api/jobs/status", { job });
    // AVA1_ERR_NO_SPACE, or the words of an older helper's message.
    if (s?.code === 10 || (typeof s?.message === "string" && /drive is full|no space/i.test(s.message))) {
      return new ApiError(507, "no_space");
    }
  } catch {
    /* it is released already, or the helper is gone: the failure stays unnamed */
  }
  return null;
}

function sameDrive(a: string, b: string, volumes?: { path: string }[]): boolean | null {
  if (!volumes?.length) return null;
  const va = volumeOf(a, volumes);
  const vb = volumeOf(b, volumes);
  return va && vb ? va.path === vb.path : null;
}

export async function pasteItems(
  items: PasteItem[],
  destDir: string,
  mode: PasteMode,
  deps: PasteDeps,
  opts?: BatchOptions,
): Promise<PasteResult> {
  const dir = destDir.replace(/\/+$/, "") || "/";
  const declined: string[] = [];
  const here: string[] = [];
  /** The items whose Replace Stop ended during the copy: they were not pasted. */
  const halted: string[] = [];
  const notes: ReplaceError[] = [];
  /** Items whose job was still running when Stop let go of it. */
  const leftRunning: string[] = [];
  const kinds = new Map(items.map((i) => [i.path, i.dir]));
  const paths = items.map((i) => i.path);
  let always: ConflictChoice | null = null;
  let current = "";
  /** Stats of folders (the destination, the items' folders), read once each. */
  const folderStats = new Map<string, Promise<Stat | null>>();
  const folderStat = (p: string) => {
    let s = folderStats.get(p);
    if (!s) {
      s = statOrNull(p).catch(() => null);
      folderStats.set(p, s);
    }
    return s;
  };
  const wait = opts?.waitJob;
  const eachOpts: BatchOptions | undefined = opts && {
    ...opts,
    waitJob: wait
      ? async (id) => {
          const end = await wait(id);
          if (end === "left") leftRunning.push(current);
          return end;
        }
      : undefined,
  };
  const side: ReplaceSide = {
    taken: async (p) => (await statOrNull(p)) != null,
    rename: async (from, to) => {
      await call("/api/fs/rename", { from, to });
    },
    remove: async (p) => {
      const { job } = await call<{ job: string }>("/api/fs/delete", { path: p });
      if (wait) await wait(job, true);
      return (await statOrNull(p)) == null;
    },
  };

  /** Replace `dest` with `item`, through a temporary name (replace.ts). */
  const replace = async (item: string, dest: string, res: BatchResult, renames: boolean): Promise<void> => {
    let renamed = false;
    const place = async (temp: string): Promise<Placed> => {
      if (renames) {
        try {
          await renameChecked(side, item, temp);
          renamed = true;
          return { end: "done", undo: () => renameChecked(side, temp, item) };
        } catch (e) {
          if (e instanceof ApiError && e.status === 401) throw e;
          // Whatever is at the temporary name after this may be the item itself: it is never cleared.
          if (!(e instanceof ApiError && e.code === "cross_mount")) return { end: "failed", why: e, notThere: true };
        }
      }
      // A cut across drives copies too: its original goes only after the new item is in place.
      const job = await startCopy(item, temp, false, res);
      const end = wait ? await wait(job) : "done";
      if (end === "done") return { end };
      if (end === "cancelled") return { end: "stopped" };
      if (end === "left") return { end };
      return { end: "failed", why: end === "failed" ? await jobFailure(job) : null };
    };
    const r = await replaceItem(side, dir, dest, place);
    const name = baseName(dest);
    if (r.end === "stopped") {
      halted.push(item);
      if (r.temp) notes.push(new ReplaceError("temp_left", name, { temp: r.temp }));
      return;
    }
    if (r.old) notes.push(new ReplaceError("old_left", name, { old: r.old }));
    if (renamed) res.moved.push(item);
    else if (mode === "cut" && !(await side.remove(item).catch(() => false))) notes.push(new ReplaceError("source_left", name));
  };

  const r = await each(paths, eachOpts, async (item, res) => {
    current = item;
    if (isInside(dir, item)) throw new IntoItselfError(item);
    if (mode === "cut") {
      const from = parentPath(item);
      if (from === dir || sameEntry(await folderStat(from), await folderStat(dir))) {
        here.push(item);
        return;
      }
    }
    const isDir = kinds.get(item) ?? false;
    let dest = joinPath(dir, baseName(item));
    let overwrite = false;
    const existing = await statOrNull(dest);
    if (existing) {
      const self = sameEntry(existing, await statOrNull(item));
      if (self && mode === "cut") {
        here.push(item);
        return;
      }
      const canReplace = !self && !isInside(item, dest);
      let choice = always;
      if (!choice) {
        const d = await deps.decide({
          item,
          dest,
          destDir: existing.kind === "dir",
          canReplace,
          left: paths.length - 1 - paths.indexOf(item),
        });
        choice = d.choice;
        if (d.all) always = d.choice;
      }
      if (choice === "skip" || (choice === "replace" && !canReplace)) {
        declined.push(item);
        return;
      }
      if (choice === "keep") {
        dest = joinPath(dir, await freeName(dir, baseName(item), isDir, async (p) => (await statOrNull(p)) != null));
      } else {
        const renames = mode === "cut" && sameDrive(item, dir, deps.volumes) !== false;
        if (isDir || existing.kind === "dir" || renames) return replace(item, dest, res, renames);
        overwrite = true;
      }
    }
    if (mode === "copy") return startCopy(item, dest, false, res, overwrite);
    if (!overwrite) {
      try {
        await call("/api/fs/rename", { from: item, to: dest });
        res.moved.push(item);
        return;
      } catch (e) {
        if (!(e instanceof ApiError && e.code === "cross_mount")) throw e;
      }
    }
    return startCopy(item, dest, true, res, overwrite);
  });

  const notDone = new Set<string>(
    declined.concat(here, halted, leftRunning, r.skipped, r.jobFailed, r.cancelled ?? [], r.failed.map((f) => f.item)),
  );
  return {
    ...r,
    skipped: halted.concat(r.skipped),
    done: paths.filter((p) => !notDone.has(p)),
    left: leftRunning,
    halted,
    declined,
    here,
    notes,
  };
}
