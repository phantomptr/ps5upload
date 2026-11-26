/**
 * What the folder view shows, as pure functions: sort, hidden files, the
 * name filter, type icons, selection (all / none / invert) and the status
 * numbers. No React here, so a 20,000-entry folder can be timed in a test.
 */
import type { Entry } from "./useDir";

export type SortKey = "name" | "size" | "date";
export type SortDir = "asc" | "desc";

export interface ViewPrefs {
  sort: SortKey;
  dir: SortDir;
  /** Show dot files. */
  hidden: boolean;
  /** Names must contain this (case-insensitive); "" = no filter. */
  filter: string;
}

export const DEFAULT_VIEW: ViewPrefs = { sort: "name", dir: "asc", hidden: false, filter: "" };

/** A folder is read in full only up to this many entries (Load all). */
export const LOAD_ALL_CAP = 20_000;

/* ---- remembered per viewer ---- */

const STORE_KEY = "ps5upload.files.view";

/** The remembered sort and hidden toggle (the filter is never remembered). */
export function loadView(): ViewPrefs {
  try {
    const raw = JSON.parse(localStorage.getItem(STORE_KEY) ?? "null") as Partial<ViewPrefs> | null;
    if (!raw || typeof raw !== "object") return DEFAULT_VIEW;
    return {
      sort: raw.sort === "size" || raw.sort === "date" ? raw.sort : "name",
      dir: raw.dir === "desc" ? "desc" : "asc",
      hidden: raw.hidden === true,
      filter: "",
    };
  } catch {
    return DEFAULT_VIEW;
  }
}

export function saveView(v: ViewPrefs): void {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify({ sort: v.sort, dir: v.dir, hidden: v.hidden }));
  } catch {
    /* private mode or blocked storage: the choice just is not remembered */
  }
}

/* ---- sort / filter ---- */

const keys = new WeakMap<Entry, string>();

/**
 * A name as a string that compares naturally with plain `<`: lower-cased,
 * every run of digits left-padded so file2 sorts before file10. (Cheaper than
 * Intl.Collator, which takes ~60 ms for 20,000 names; code-point order is
 * stable for any script, CJK included.)
 */
export function natKey(name: string): string {
  // NFD + dropped combining marks: "Éclair" sorts with "eclair", before "zebra".
  return name
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .replace(/\d+/g, (d) => (d.length >= 20 ? d : d.padStart(20, "0")));
}

/** How many sort keys were built (tests: each entry is keyed once across repeated sorts). */
export const keyStats = { built: 0 };

function keyOf(e: Entry): string {
  let k = keys.get(e);
  if (k === undefined) {
    keyStats.built++;
    k = natKey(e.name);
    keys.set(e, k);
  }
  return k;
}

/** By code point (plain `<` compares UTF-16 units, which puts astral characters before U+E000-U+FFFF). */
function cmpStr(a: string, b: string): number {
  if (a === b) return 0;
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    let x = a.charCodeAt(i);
    let y = b.charCodeAt(i);
    if (x !== y) {
      if (x >= 0xd800) x += x >= 0xe000 ? -0x800 : 0x2000;
      if (y >= 0xd800) y += y >= 0xe000 ? -0x800 : 0x2000;
      return x < y ? -1 : 1;
    }
  }
  return a.length < b.length ? -1 : 1;
}
const isDir = (e: Entry): boolean => e.kind === "dir";

/** Folders first, then by `sort` (`dir` flips files and, for name/date, folders too). */
export function sortEntries(entries: Entry[], sort: SortKey = "name", dir: SortDir = "asc"): Entry[] {
  const sign = dir === "desc" ? -1 : 1;
  const byName = (a: Entry, b: Entry) => cmpStr(keyOf(a), keyOf(b)) || cmpStr(a.name, b.name);
  const cmp = (a: Entry, b: Entry): number => {
    const fa = isDir(a);
    if (fa !== isDir(b)) return fa ? -1 : 1;
    if (sort === "name") return sign * byName(a, b);
    if (sort === "size") {
      // Folders have no size here: they stay in name order.
      if (fa) return byName(a, b);
      return sign * ((a.size || 0) - (b.size || 0)) || byName(a, b);
    }
    return sign * ((a.mtime || 0) - (b.mtime || 0)) || byName(a, b);
  };
  return entries.slice().sort(cmp);
}

export const isHidden = (e: Entry): boolean => e.name.charCodeAt(0) === 46;

/** A Mesh Drive's own sync files: never listed, even with hidden files on (deleting the
 *  marker makes the drive treat its folder as offline). */
export const MESH_MARKER = ".ps5upload-mesh";
const MESH_INTERNAL = new Set([MESH_MARKER, ".ps5upload-mesh-tmp", ".ps5upload-mesh-base"]);
export const isMeshInternal = (e: Entry): boolean => MESH_INTERNAL.has(e.name);

/** The rows to show: hidden files and the filter applied, then sorted. */
export function visibleEntries(entries: Entry[], v: ViewPrefs): Entry[] {
  const q = v.filter.trim().toLowerCase();
  let list = entries.filter((e) => !isMeshInternal(e));
  if (!v.hidden || q) {
    list = list.filter((e) => (v.hidden || !isHidden(e)) && (!q || e.name.toLowerCase().includes(q)));
  }
  return sortEntries(list, v.sort, v.dir);
}

/**
 * Why a folder with entries shows none: the filter, files hidden while Hidden files is off, or
 * only the Mesh Drive's own files (never listed): then it reads as empty. It said "Nothing matches
 * the filter." with no filter set.
 */
export function emptyReason(entries: Entry[], v: ViewPrefs): "filter" | "hidden" | "empty" {
  if (v.filter.trim()) return "filter";
  return !v.hidden && entries.some((e) => !isMeshInternal(e) && isHidden(e)) ? "hidden" : "empty";
}

/* ---- type icons ---- */

export type IconKind =
  | "folder"
  | "link"
  | "image"
  | "text"
  | "disk"
  | "package"
  | "exec"
  | "archive"
  | "video"
  | "other";

const BY_EXT: Record<string, IconKind> = {};
for (const [kind, exts] of [
  ["image", "png jpg jpeg gif webp dds bmp"],
  ["text", "txt log ini json xml cfg md"],
  ["disk", "exfat ffpkg ffpfs ffpfsc img iso"],
  ["package", "pkg"],
  ["exec", "elf bin self prx sprx"],
  ["archive", "zip 7z rar"],
  ["video", "mp4 webm mkv"],
] as const) {
  for (const x of exts.split(" ")) BY_EXT[x] = kind;
}

export function iconKind(e: Pick<Entry, "name" | "kind">): IconKind {
  if (e.kind === "dir") return "folder";
  if (e.kind === "link") return "link";
  const dot = e.name.lastIndexOf(".");
  if (dot <= 0) return "other";
  return BY_EXT[e.name.slice(dot + 1).toLowerCase()] ?? "other";
}

/* ---- selection ---- */

export const namesOf = (shown: Entry[]): string[] => shown.map((e) => e.name);

/** Everything shown joins the selection (items outside the filter stay as they were). */
export function selectAllShown(selected: string[], shown: Entry[]): string[] {
  const have = new Set(selected);
  const out = selected.slice();
  for (const e of shown) if (!have.has(e.name)) out.push(e.name);
  return out;
}

/** Shown rows flip; selected rows hidden by the filter stay selected. */
export function invertShown(selected: string[], shown: Entry[]): string[] {
  const have = new Set(selected);
  const showing = new Set<string>();
  const out: string[] = [];
  for (const e of shown) {
    showing.add(e.name);
    if (!have.has(e.name)) out.push(e.name);
  }
  for (const s of selected) if (!showing.has(s)) out.push(s);
  return out;
}

/* ---- status ---- */

export interface Status {
  /** Rows shown. */
  items: number;
  /** Rows loaded (before the hidden/filter cut). */
  total: number;
  selected: number;
  /** Bytes of the selected files (folders add nothing). */
  bytes: number;
  /** Selected folders (no size). */
  folders: number;
}

export function statusOf(shown: Entry[], total: number, selected: string[], all: Entry[]): Status {
  const byName = new Map<string, Entry>();
  for (const e of all) byName.set(e.name, e);
  let bytes = 0;
  let folders = 0;
  let n = 0;
  for (const s of selected) {
    const e = byName.get(s);
    if (!e) continue;
    n++;
    if (isDir(e)) folders++;
    else bytes += e.size || 0;
  }
  return { items: shown.length, total, selected: n, bytes, folders };
}
