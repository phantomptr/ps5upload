/**
 * Bulk rename: the new names for a set of entries of one folder (find and
 * replace, a prefix, a suffix before the extension, or numbering), checked
 * before anything is sent, then the renames one at a time.
 *
 * The plan is refused as a whole when any new name is empty, holds "/", is
 * "." / "..", is longer than a name can be (255 bytes), is unchanged, is
 * already used in the folder, or is given to two items. Names compare
 * without case: exFAT drives (USB) treat "A.txt" and "a.txt" as one name,
 * and a rename onto an existing name is refused by the helper anyway (it
 * never overwrites); the planner refuses it first, with the reason.
 */
import type { BatchResult } from "./move";
import { renameEntry } from "./move";

export type RenameMode =
  | { kind: "replace"; find: string; replace: string; caseSensitive: boolean }
  | { kind: "prefix"; text: string }
  | { kind: "suffix"; text: string }
  /** `template` with "{n}" where the number goes; empty: each name + " 01", " 02"… */
  | { kind: "number"; template: string; start: number };

export interface RenameItem {
  name: string;
  dir: boolean;
}

/** Why a new name cannot be used (checked in this order). */
export type RenameProblem = "empty" | "slash" | "dots" | "long" | "unchanged" | "exists" | "twice";

export interface RenameRow {
  from: string;
  to: string;
  problem: RenameProblem | null;
}

export interface RenamePlan {
  rows: RenameRow[];
  /** Every row can be renamed. */
  ok: boolean;
  /** The first row with a problem (what the dialog explains). */
  first: RenameRow | null;
}

/** The longest name, in UTF-8 bytes (the console's file systems). */
export const NAME_MAX_BYTES = 255;

/** "name.ext" → ["name", ".ext"]; a folder, a dot-file (".hidden") or a name without a dot has no extension. */
export function splitExt(name: string, dir: boolean): [string, string] {
  const dot = dir ? -1 : name.lastIndexOf(".");
  return dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, ""];
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Every `find` in `name` replaced by `repl` (literal text, never a pattern). */
export function replaceAll(name: string, find: string, repl: string, caseSensitive: boolean): string {
  if (!find) return name;
  if (caseSensitive) return name.split(find).join(repl);
  // A function replacement: "$&" or "$1" in the typed text stay as typed.
  return name.replace(new RegExp(escapeRegExp(find), "gi"), () => repl);
}

/** The number for item `i` of `total` from `start`, zero-padded to at least 2 digits (all the same width). */
export function numberText(i: number, total: number, start: number): string {
  const last = String(start + Math.max(total, 1) - 1);
  const width = Math.max(2, last.length);
  const n = String(start + i);
  return n.length >= width ? n : `${"0".repeat(width - n.length)}${n}`;
}

/** The new name of `item`, the `i`th of `total`. */
export function renamed(item: RenameItem, i: number, total: number, mode: RenameMode): string {
  const [stem, ext] = splitExt(item.name, item.dir);
  switch (mode.kind) {
    case "replace":
      return replaceAll(item.name, mode.find, mode.replace, mode.caseSensitive);
    case "prefix":
      return `${mode.text}${item.name}`;
    case "suffix":
      return `${stem}${mode.text}${ext}`;
    case "number": {
      const n = numberText(i, total, mode.start);
      const tpl = mode.template;
      if (tpl.indexOf("{n}") >= 0) return `${tpl.split("{n}").join(n)}${ext}`;
      return `${tpl === "" ? stem : tpl} ${n}${ext}`;
    }
  }
}

function utf8Bytes(s: string): number {
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
      n += 4;
      i++;
    } else n += 3;
  }
  return n;
}

const key = (s: string) => s.toLowerCase();

/**
 * The plan for renaming `items` (in this order: numbering follows it) in a
 * folder holding `existing` (every name there, the items' own included).
 */
export function planRenames(items: RenameItem[], existing: string[], mode: RenameMode): RenamePlan {
  const taken = new Set(existing.map(key));
  const rows: RenameRow[] = items.map((it, i) => ({ from: it.name, to: renamed(it, i, items.length, mode), problem: null }));
  const count = new Map<string, number>();
  for (const r of rows) count.set(key(r.to), (count.get(key(r.to)) ?? 0) + 1);
  let first: RenameRow | null = null;
  for (const r of rows) {
    const k = key(r.to);
    let p: RenameProblem | null = null;
    if (r.to === "") p = "empty";
    else if (r.to.indexOf("/") >= 0 || r.to.indexOf("\0") >= 0) p = "slash";
    else if (r.to === "." || r.to === "..") p = "dots";
    else if (utf8Bytes(r.to) > NAME_MAX_BYTES) p = "long";
    else if (r.to === r.from) p = "unchanged";
    // The item's own name does not count: "a.txt" → "A.txt" only changes the case.
    else if (taken.has(k) && k !== key(r.from)) p = "exists";
    else if ((count.get(k) ?? 0) > 1) p = "twice";
    r.problem = p;
    if (p && !first) first = r;
  }
  return { rows, ok: rows.length > 0 && !first, first };
}

export interface RenameResult extends BatchResult {
  /** The renames that were made, as [from, to]. */
  done: [string, string][];
  /** The rename that failed (the run stopped there). */
  failedRow: RenameRow | null;
}

/**
 * Rename each row of `rows` (a plan that is ok) in `dir`, one at a time,
 * stopping at the first failure (the rest are not sent) or when `stopped()`.
 */
export async function runRenames(
  dir: string,
  rows: RenameRow[],
  opts: { onProgress?: (at: number, total: number) => void; stopped?: () => boolean } = {},
  rename: (dir: string, from: string, to: string) => Promise<void> = renameEntry,
): Promise<RenameResult> {
  const r: RenameResult = { started: [], moved: [], failed: [], jobFailed: [], skipped: [], done: [], failedRow: null };
  for (let i = 0; i < rows.length; i++) {
    if (opts.stopped?.()) {
      r.skipped = rows.slice(i).map((x) => x.from);
      break;
    }
    opts.onProgress?.(i + 1, rows.length);
    const row = rows[i];
    try {
      await rename(dir, row.from, row.to);
    } catch (error) {
      r.failed.push({ item: row.from, error });
      r.failedRow = row;
      r.skipped = rows.slice(i + 1).map((x) => x.from);
      break;
    }
    r.moved.push(row.from);
    r.done.push([row.from, row.to]);
  }
  return r;
}
