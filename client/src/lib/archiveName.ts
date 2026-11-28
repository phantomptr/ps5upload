/**
 * Which files the console can extract (the helper's extractor payload) and the
 * folder it extracts into by default. Shared by the PC Files screen and the
 * console page (Minion), so both offer Extract on the same names.
 *
 * A split archive is extracted from its first volume only (`.part1.rar`,
 * `.part01.rar`, `.001`); the later volumes sit beside it and are read from
 * there, so they get no Extract action of their own. A `.ffpfsc` is not here:
 * it has its own Decompress action.
 */

const SINGLE = /\.(zip|zipx|7z|rar|tar|tgz|tbz|tbz2|txz|tzst|gz|bz2|xz|zst)$/i;
const RAR_PART = /\.part(\d+)\.rar$/i;
const SPLIT = /\.(\d{3})$/;

/** `name` is an archive (or the first volume of a split one) the console can extract. */
export function isExtractableArchive(name: string): boolean {
  const part = RAR_PART.exec(name);
  if (part) return Number(part[1]) === 1;
  const split = SPLIT.exec(name);
  if (split) return split[1] === "001";
  return SINGLE.test(name);
}

/** The extensions taken off, longest first, after a `.partN.rar` / `.001` volume suffix. */
const STRIP: RegExp[] = [
  /\.part\d+\.rar$/i,
  /\.tar\.(gz|bz2|xz|zst)$/i,
  /\.(zip|zipx|7z|rar|tar|tgz|tbz|tbz2|txz|tzst|gz|bz2|xz|zst)$/i,
];

/**
 * The folder an archive extracts into by default: its name without the
 * archive's extensions (`x.tar.gz`, `x.tgz`, `x.7z.001`, `x.part01.rar` → `x`;
 * a single compressed file `x.bin.gz` → `x.bin`). "extracted" when nothing is left.
 */
export function archiveFolderName(name: string): string {
  let n = name;
  if (SPLIT.test(n)) n = n.slice(0, -4);
  for (const re of STRIP) {
    const m = re.exec(n);
    if (m) {
      n = n.slice(0, m.index);
      break;
    }
  }
  n = n.trim();
  return n && n !== "." && n !== ".." ? n : "extracted";
}

/** One plain folder name: not empty, no "/", not "." or "..", at most 255 bytes. */
export function validFolderName(name: string): boolean {
  if (!name || name === "." || name === ".." || name.includes("/") || name.includes("\0")) return false;
  return new TextEncoder().encode(name).length <= 255;
}

/** `name`, or "name (2)", "name (3)"… — the first one `taken` says is free. */
export function freeFolderName(name: string, taken: (n: string) => boolean): string {
  if (!taken(name)) return name;
  for (let i = 2; i < 10_000; i++) {
    const c = `${name} (${i})`;
    if (!taken(c)) return c;
  }
  return `${name} (${Date.now()})`;
}

/** The extract job's result ({"path","files","dirs","bytes","skipped","unsafe","links"}). */
export interface ExtractResult {
  path: string;
  files: number;
  dirs: number;
  bytes: number;
  /** Files already there (merge without replace), left as they were. */
  skipped: number;
  /** Entries refused for an unsafe path (absolute, "..", through a link). */
  unsafe: number;
  /** Links and special entries, not extracted. */
  links: number;
  /** "Delete the archive as it's unpacked": parts of it deleted, and their bytes. */
  partsFreed: number;
  bytesFreed: number;
}

/** A job result (any shape) as an ExtractResult; missing numbers are 0. */
export function toExtractResult(r: unknown): ExtractResult {
  const o = (r && typeof r === "object" ? r : {}) as Record<string, unknown>;
  const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
  return {
    path: typeof o.path === "string" ? o.path : "",
    files: num(o.files),
    dirs: num(o.dirs),
    bytes: num(o.bytes),
    skipped: num(o.skipped),
    unsafe: num(o.unsafe),
    links: num(o.links),
    partsFreed: num(o.parts_freed),
    bytesFreed: num(o.bytes_freed),
  };
}

/** How an archive on the console would be deleted as it is unpacked: the plan a listing with
 *  `free` answers, from the archive's listing alone. `parts`: each part goes once everything in
 *  it is out (a set of parts); `end`: the archive goes once the unpack is done (one file, a
 *  solid or encrypted RAR set, a single compressed stream); "" with `why` when it can't be done. */
export interface FreePlan {
  mode: "parts" | "end" | "";
  parts: number;
  /** Every part's size together. */
  archiveBytes: number;
  /** What it unpacks to (0 when the listing doesn't say). */
  unpacked: number;
  /** The most room the unpack takes at once on the archive's drive, with the parts going on the
   *  way (an estimate; the console measures it before deleting anything). */
  peak: number;
  /** The archive is on the drive it is unpacked to: only then does freeing make room there. */
  sameDrive: boolean;
  /** `unpacked` and `peak` are known. */
  known: boolean;
  /** Why it can't be done ("fs_extract_free_refused: …"), else "". */
  why: string;
}

/** A listing's `free` member as a FreePlan; null when the console did not answer one (too old). */
export function toFreePlan(v: unknown): FreePlan | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const num = (x: unknown) => (typeof x === "number" && Number.isFinite(x) && x > 0 ? x : 0);
  return {
    mode: o.mode === "parts" || o.mode === "end" ? o.mode : "",
    parts: num(o.parts),
    archiveBytes: num(o.archive_bytes),
    unpacked: num(o.unpacked),
    peak: num(o.peak),
    sameDrive: o.same_drive === true,
    known: o.known === true,
    why: typeof o.why === "string" ? o.why : "",
  };
}

/** How much less free space the unpack needs on its drive than with the archive kept: what it
 *  unpacks to, less the most it takes at once. 0 when the archive goes only at the end, is on
 *  another drive, or the listing doesn't say. */
export function freePlanSaves(p: FreePlan): number {
  return p.mode === "parts" && p.known && p.sameDrive ? Math.max(0, p.unpacked - p.peak) : 0;
}

/** The free space the unpack needs with the archive deleted as it goes, or null when not known. */
export function freePlanNeeds(p: FreePlan): number | null {
  if (!p.known) return null;
  return p.sameDrive ? p.peak : p.unpacked;
}

/** What the dialog says about a plan, in plain words: freed part by part (with how much less room
 *  it needs, when known), no room made on this drive (the archive is on another), deleted only at
 *  the end, or can't be done. */
export type FreeNote =
  | { kind: "parts"; parts: number; saves: number }
  | { kind: "parts_plain"; parts: number }
  | { kind: "other_drive" }
  | { kind: "end" }
  | { kind: "refused" };

export function freeNote(p: FreePlan): FreeNote {
  if (p.mode === "") return { kind: "refused" };
  if (!p.sameDrive) return { kind: "other_drive" };
  if (p.mode === "end") return { kind: "end" };
  const saves = freePlanSaves(p);
  return saves > 0 ? { kind: "parts", parts: p.parts, saves } : { kind: "parts_plain", parts: p.parts };
}

/** An unpack that stopped after part of its archive was deleted, from the console's
 *  "fs_extract_partly_freed: <parts deleted> <parts left> <first part left> | <folder> | <cause>". */
export interface PartlyFreed {
  freed: number;
  left: number;
  /** The first part left, renamed to end in .partial-deleted ("-" when none). */
  firstLeft: string;
  /** The folder kept with everything unpacked before it stopped. */
  folder: string;
  /** Why it stopped: the extractor's own cause (fs_extract_no_space, fs_extract_cancelled, …). */
  cause: string;
}

export function partlyFreed(raw: string): PartlyFreed | null {
  const m = /fs_extract_partly_freed:\s*(\d+)\s+(\d+)\s+(.*?)\s+\|\s+(.*?)\s+\|\s+(.*)$/s.exec(raw);
  if (!m) return null;
  return { freed: Number(m[1]), left: Number(m[2]), firstLeft: m[3], folder: m[4], cause: m[5].trim() };
}

/** The cause token of an extract failure ("fs_extract_no_space: needs …" → "fs_extract_no_space"), or null. */
export function extractCause(raw: string): string | null {
  const m = /fs_extract_[a-z_]+/i.exec(raw);
  return m ? m[0].toLowerCase() : null;
}

/** The failure asks for a (different) password: ask for one and run again. */
export function needsPassword(raw: string): boolean {
  const c = extractCause(raw);
  return c === "fs_extract_password_required" || c === "fs_extract_wrong_password";
}
