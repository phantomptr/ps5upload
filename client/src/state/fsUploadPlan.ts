import { isRemotePath } from "../lib/remotePath";
import { keepBothName } from "../lib/consolePath";

/**
 * Turns a pick on the Files screen ("Add files", "Add folder", a drop from the computer) into
 * the steps `runFsUpload` (state/fsUpload) sends. Pure: the screen passes in what the folder
 * holds and asks the user through `decide`.
 *
 * Why steps, not one job per item: each AVA1 job costs the engine ~0.46 s to open and close,
 * so 5,000 small files sent one by one took 44 minutes. The engine's file-list job
 * (POST /api/transfer/file-list) sends a whole set of files as ONE job, at the console's own
 * small-file speed (~200-270 files/s). So:
 *
 * - Two or more local FILES of the pick become one `list` step (one job, one journal).
 * - A single file stays a `one` step: it is one job either way, and the single-file job
 *   (write `<name>.ava-part`, rename into place) is the path every other screen uses.
 * - A FOLDER stays a `one` step: it already goes up as ONE job (`/api/transfer/dir`), and that
 *   job does what a file list cannot: empty sub-folders, the user's excludes, the
 *   module-name case fixes, a saved server as the source, and staging a new folder whole
 *   before it appears. Batching folders would only save ~0.5 s per dropped folder.
 * - A file on a saved server (`remote://`) stays a `one` step: the file-list job reads local
 *   disk only.
 *
 * Name conflicts are settled here, BEFORE anything is sent (a running job cannot stop to ask):
 * - Replace: the file is sent and overwrites the one there (the console renames the finished
 *   file over it). For a folder that is already there, Replace merges into it, as before.
 * - Skip: the item is left out of the steps.
 * - Keep both: the item goes up under the first free "name (2).ext" (a folder keeps its dots).
 * "Apply to all" answers every later conflict the same way. Two picked items with the same name
 * conflict with each other too (they would land on the same path): Replace means the later one
 * wins, as it did when they went up one after another.
 */

export type FsConflictChoice = "replace" | "skip" | "keep";

export interface FsConflict {
  /** The name already taken in the folder. */
  name: string;
  /** The picked item is a folder. */
  pickedDir: boolean;
  /** What is there is a folder. */
  existingDir: boolean;
  /** Replace is possible: a file over a file, or a folder merged into a folder. */
  canReplace: boolean;
  /** Conflicts still to come after this one (Apply to all is offered when > 0). */
  left: number;
}

export interface FsConflictDecision {
  choice: FsConflictChoice;
  /** Answer every later conflict the same way. */
  all: boolean;
}

/** One picked path and what it is on the computer. */
export interface FsPickedItem {
  src: string;
  /** "missing"/"other" travel as a file: the job then fails with the engine's own reason. */
  kind: "file" | "folder" | "other" | "missing";
}

/** What the folder on the console already holds (its listing). */
export interface FsExistingEntry {
  name: string;
  /** "file", "dir", or anything else the listing says. */
  kind: string;
  size: number;
}

/** One file of a `list` step. */
export interface FsListFile {
  src: string;
  /** The name it gets in the destination folder. */
  name: string;
  /** Replace: the size of the file it overwrites (tells "landed" from "the old one" after a
   *  failure, see `landedOfList`). */
  replacesSize?: number;
}

export type FsUploadStep =
  /** One file or folder, as its own job (a folder is told apart by `pathKind` at run time;
   *  `asFile`: never a folder, the row action "Replace" with a picked file). `txId` is the job's
   *  id, as for a list: a retry or a resume under the same id continues the console's partial.
   *  Absent on what an older version saved; the run mints it. */
  | { kind: "one"; src: string; name: string; asFile?: boolean; txId?: string }
  /** Files sent together as ONE file-list job. `txId` is the job's id: a resume sends the same
   *  list under the same id and the console's journal skips what it already has. */
  | { kind: "list"; txId: string; files: FsListFile[] };

export interface FsUploadPlan {
  steps: FsUploadStep[];
  /** Items left out because the user chose Skip. */
  skipped: string[];
}

/** How two names are compared for a conflict: Unicode NFC, lower case. */
export function nameKey(name: string): string {
  return name.normalize("NFC").toLowerCase();
}

/** Last part of a path, whichever separator the computer uses. */
export function baseNameOf(p: string): string {
  const t = p.replace(/[\\/]+$/, "");
  return t.split(/[\\/]/).pop() || t || "file";
}

export { keepBothName };

/** The first Keep both name for `name` that is not in `taken`. */
export function freeKeepBothName(
  name: string,
  dir: boolean,
  taken: (n: string) => boolean,
): string {
  const dot = dir ? -1 : name.lastIndexOf(".");
  const m = /^(.+) \((\d+)\)$/.exec(dot > 0 ? name.slice(0, dot) : name);
  const start = m ? Math.max(2, parseInt(m[2], 10) + 1) : 2;
  for (let n = start; ; n++) {
    const candidate = keepBothName(name, n, dir);
    if (!taken(candidate)) return candidate;
  }
}

/** Whether a picked file can go in a file-list job: a file on this computer's disk. */
export function listable(item: FsPickedItem): boolean {
  return item.kind === "file" && !isRemotePath(item.src);
}

interface Planned {
  src: string;
  name: string;
  dir: boolean;
  listable: boolean;
  replacesSize?: number;
}

/**
 * Settles conflicts and groups the pick into steps. Returns null when the user cancelled at a
 * conflict (nothing is sent). `newTxId` mints the id of each job.
 */
export async function planFsUpload(
  picked: FsPickedItem[],
  existing: FsExistingEntry[],
  decide: (c: FsConflict) => Promise<FsConflictDecision | null>,
  newTxId: () => string,
): Promise<FsUploadPlan | null> {
  // Names compare case- and Unicode-form-insensitively on every drive: the console's exFAT and
  // FAT drives are case-insensitive (a.TXT lands on A.txt), a name picked on macOS can arrive
  // decomposed (NFD), and a case-only twin on /data would be a confusing near-duplicate anyway.
  // So "A.txt" here and "a.txt" picked is a conflict, and Keep both looks for a name that is
  // free under the same rule.
  const there = new Map<string, FsExistingEntry>();
  for (const e of existing) there.set(nameKey(e.name), e);
  // Names this plan has given out so far (an item later in the pick must not land on them).
  const planned = new Map<string, Planned>();
  const order: Planned[] = [];
  const skipped: string[] = [];
  let always: FsConflictChoice | null = null;

  // Conflicts still to come, for "Apply to all (N)": counted up front, so the number is
  // honest about the first question (Keep both names never clash with a later pick's name
  // in practice, and the count is only a hint).
  const seen = new Set<string>();
  let pending = 0;
  for (const p of picked) {
    const n = baseNameOf(p.src);
    const k = nameKey(n);
    if (there.has(k) || seen.has(k)) pending++;
    seen.add(k);
  }

  for (const p of picked) {
    const name = baseNameOf(p.src);
    const dir = p.kind === "folder";
    const item: Planned = { src: p.src, name, dir, listable: listable(p) };
    const earlier = planned.get(nameKey(name));
    const onDisk = there.get(nameKey(name));
    if (!earlier && !onDisk) {
      planned.set(nameKey(name), item);
      order.push(item);
      continue;
    }
    pending--;
    // What the item would collide with: an earlier pick wins over the listing (it lands later).
    const existingDir = earlier ? earlier.dir : onDisk?.kind === "dir";
    const canReplace = existingDir === dir;
    let choice: FsConflictChoice | null = always;
    if (choice === "replace" && !canReplace) choice = null;
    if (!choice) {
      const d = await decide({
        name,
        pickedDir: dir,
        existingDir,
        canReplace,
        left: Math.max(0, pending),
      });
      if (!d) return null;
      choice = d.choice === "replace" && !canReplace ? "keep" : d.choice;
      if (d.all) always = d.choice;
    }
    if (choice === "skip") {
      skipped.push(p.src);
      continue;
    }
    if (choice === "keep") {
      item.name = freeKeepBothName(
        name,
        dir,
        (n) => there.has(nameKey(n)) || planned.has(nameKey(n)),
      );
      planned.set(nameKey(item.name), item);
      order.push(item);
      continue;
    }
    // Replace. The item lands under the name that is there (its spelling), so it replaces that
    // entry on a case-sensitive drive too instead of becoming a case-only twin of it.
    if (earlier) {
      item.name = earlier.name;
      // Two picked items with one name: a file replaces the earlier file (it would have
      // overwritten it when they went one by one); folders both merge into the same folder.
      if (!dir) {
        order.splice(order.indexOf(earlier), 1);
        if (earlier.replacesSize !== undefined)
          item.replacesSize = earlier.replacesSize;
      }
    } else if (onDisk) {
      item.name = onDisk.name;
      if (onDisk.kind === "file") item.replacesSize = onDisk.size;
    }
    planned.set(nameKey(item.name), item);
    order.push(item);
  }

  // Two or more listable files go as one list job; everything else, item by item. The list takes
  // the place of its first file, so the order the user picked in is kept as far as it can be.
  const listed = order.filter((i) => i.listable && !i.dir);
  const steps: FsUploadStep[] = [];
  let listAt = -1;
  for (const i of order) {
    if (listed.length >= 2 && i.listable && !i.dir) {
      if (listAt < 0) {
        listAt = steps.length;
        steps.push({
          kind: "list",
          txId: newTxId(),
          files: listed.map((f) =>
            f.replacesSize !== undefined
              ? { src: f.src, name: f.name, replacesSize: f.replacesSize }
              : { src: f.src, name: f.name },
          ),
        });
      }
      continue;
    }
    steps.push({ kind: "one", src: i.src, name: i.name, txId: newTxId() });
  }
  return { steps, skipped };
}

/** How many of the pick a step accounts for (a list counts each of its files). */
export function stepCount(s: FsUploadStep): number {
  return s.kind === "list" ? s.files.length : 1;
}

/**
 * After a list job failed or was stopped: which of its files are in place, read from the folder's
 * listing. A file counts as landed when its name is there with the size the engine planned for
 * it. A Replace whose new file is the same size as the old one cannot be told apart, so it is
 * NOT counted (the report never claims more than it knows); a resume re-sends nothing the
 * console's journal has anyway.
 */
export function landedOfList(
  files: FsListFile[],
  listing: FsExistingEntry[] | null,
  sizes: Map<string, number> | null,
): { landed: FsListFile[]; notLanded: FsListFile[] } {
  if (!listing) return { landed: [], notLanded: [...files] };
  const at = new Map<string, FsExistingEntry>();
  for (const e of listing) at.set(nameKey(e.name), e);
  const landed: FsListFile[] = [];
  const notLanded: FsListFile[] = [];
  for (const f of files) {
    const e = at.get(nameKey(f.name));
    const want = sizes?.get(f.name);
    const ok =
      !!e &&
      e.kind === "file" &&
      (want === undefined ? f.replacesSize === undefined : e.size === want) &&
      (f.replacesSize === undefined || want === undefined || f.replacesSize !== want);
    (ok ? landed : notLanded).push(f);
  }
  return { landed, notLanded };
}

/**
 * The order the engine sends a list's files in (`upload_list_group_in` sorts by path
 * components; these are all in one folder, so by name, bytewise). Used to name the file in
 * flight from the job's file counter.
 */
export function sendOrder(files: FsListFile[]): string[] {
  const enc = new TextEncoder();
  const keyed = files.map((f) => ({ n: f.name, k: enc.encode(f.name) }));
  keyed.sort((a, b) => {
    const len = Math.min(a.k.length, b.k.length);
    for (let i = 0; i < len; i++) if (a.k[i] !== b.k[i]) return a.k[i] - b.k[i];
    return a.k.length - b.k.length;
  });
  return keyed.map((x) => x.n);
}

/**
 * What each picked path is. A picker that only returns files (or folders) says so with `hint`
 * and nothing is asked; a drop can mix both, so each path is asked about, a few at a time (a
 * drop of thousands of files must not wait on one round trip per file in a row). A path that
 * cannot be asked about counts as a file: its job then fails with the engine's own reason.
 */
export async function kindsOf(
  paths: string[],
  pathKind: (p: string) => Promise<string>,
  hint?: "file" | "folder",
  parallel = 16,
): Promise<FsPickedItem[]> {
  if (hint) return paths.map((src) => ({ src, kind: hint }));
  const out: FsPickedItem[] = paths.map((src) => ({ src, kind: "file" }));
  let next = 0;
  const worker = async () => {
    for (let i = next++; i < paths.length; i = next++) {
      const k = await pathKind(paths[i]).catch(() => "file");
      out[i].kind =
        k === "folder" || k === "missing" || k === "other" ? k : "file";
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(parallel, paths.length) }, worker),
  );
  return out;
}

/** A few names and how many more, without words to translate: "a, b, c, … (+12)". */
export function namesPreview(names: string[], shown = 6): string {
  const head = names.slice(0, shown).join(", ");
  return names.length > shown ? `${head}, … (+${names.length - shown})` : head;
}
