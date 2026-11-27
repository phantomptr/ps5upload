/**
 * The Library's games (#526): every copy, update, DLC and backport a scan found, grouped into one
 * entry per game the way the PC app's Collection groups its index (one game per base title id,
 * engine/crates/ps5upload-engine/src/collection): the same game on two shares, as a folder and
 * as a package, or with its update and DLC next to it is ONE card, and its detail lists the parts.
 *
 * No I/O here (tested directly). The key is the platform and the base title id ("PPSA01234");
 * an item without one (a disk image named without its id) groups by its name instead.
 */
import { CONSOLE_SOURCE, displayName, type Entry } from "./model";

/** What a part is to its game. */
export type Role = "base" | "update" | "dlc" | "backport" | "app" | "other";

/** How a part is kept. */
export type Format = "folder" | "pkg" | "fpkg" | "exfat" | "ffpkg" | "ffpfs" | "image";

export interface Part {
  entry: Entry;
  role: Role;
  format: Format;
}

export interface GameGroup {
  key: string;
  title: string;
  titleId: string;
  platform: string;
  /** The part whose cover the card shows (the base game's when it has one), or null. */
  cover: Entry | null;
  /** Base copies first, then updates (newest first), backports, DLC, the rest. */
  parts: Part[];
  /** The game's size: its largest base copy (all parts added up when it has none), as known so far. */
  size: number;
  /** A part that counts towards `size` has no size yet (a folder is 0 until counted). */
  sizeUnknown: boolean;
  /** The highest system software any part asks for ("7.50"), "" when none says. */
  fw: string;
  /** The SDK a part was built with ("4.50"), "" when none says. */
  sdk: string;
  formats: Format[];
  /** Where its parts were found (source names, in order). */
  places: string[];
  hasBase: boolean;
  updates: number;
  dlc: number;
  backports: number;
  fake: boolean;
  /**
   * No part names the game and a disk image among them could not be read on the console (#963):
   * the card shows the title id (or the file name) and says the title was not read. The first such
   * image's cause ("img_compressed", ...), "" when the title is known or nothing says why.
   */
  titleUnread: string;
}

const TITLE_ID = /^[A-Z]{4}\d{5}$/;
const BACKPORT = /back[\s._-]?port/i;

export function formatOf(e: Pick<Entry, "kind" | "name" | "fake">): Format {
  if (e.kind === "folder") return "folder";
  if (e.kind === "pkg") return e.fake ? "fpkg" : "pkg";
  const ext = /\.([a-z]+)$/i.exec(e.name)?.[1]?.toLowerCase() ?? "";
  if (ext === "exfat") return "exfat";
  if (ext === "ffpkg") return "ffpkg";
  if (ext === "ffpfs" || ext === "ffpfsc") return "ffpfs";
  return "image";
}

export function roleOf(e: Pick<Entry, "type" | "name" | "path" | "backport">): Role {
  if (e.backport || BACKPORT.test(e.name) || BACKPORT.test(e.path)) return "backport";
  if (e.type === "patch") return "update";
  if (e.type === "dlc") return "dlc";
  if (e.type === "app") return "app";
  if (e.type === "game") return "base";
  return "other";
}

/** "PPSA01234" for an item, from its title id or else its content id ("UP9000-PPSA01234_00-…"). */
export function baseTitleId(e: Pick<Entry, "title_id" | "content_id">): string {
  const id = e.title_id.trim().toUpperCase();
  if (TITLE_ID.test(id)) return id;
  const m = /^[A-Z]{2}\d{4}-([A-Z]{4}\d{5})_/.exec(e.content_id.trim().toUpperCase());
  return m ? m[1] : "";
}

/** A name to group by when there is no title id: letters and digits only, version words dropped. */
export function nameKey(name: string): string {
  return name
    .toLowerCase()
    .replace(/\.(pkg|exfat|ffpkg|ffpfsc?)$/, "")
    .replace(/\b(v\d+(\.\d+)*|update|patch|backport(ed)?|dlc|fpkg|ps[45])\b/g, " ")
    .replace(/[^a-z0-9]+/g, "");
}

export function groupKey(e: Entry): string {
  const id = baseTitleId(e);
  const plat = e.platform || (id.startsWith("CUSA") ? "ps4" : id.startsWith("PPSA") ? "ps5" : "");
  return id ? `${plat}:${id}` : `name:${nameKey(displayName(e)) || e.key}`;
}

const ROLE_ORDER: Record<Role, number> = { base: 0, update: 1, backport: 2, dlc: 3, app: 4, other: 5 };

/** "01.020.000" vs "01.003": by number per dotted part. */
export function compareVersions(a: string, b: string): number {
  const pa = a.split(/[^0-9]+/).filter(Boolean).map(Number);
  const pb = b.split(/[^0-9]+/).filter(Boolean).map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

function sortParts(parts: Part[]): Part[] {
  return parts.slice().sort((x, y) => {
    const r = ROLE_ORDER[x.role] - ROLE_ORDER[y.role];
    if (r) return r;
    if (x.role === "update") {
      const v = compareVersions(y.entry.version, x.entry.version);
      if (v) return v;
    }
    // a copy on this console first, then by place and name
    const c = Number(y.entry.source === CONSOLE_SOURCE) - Number(x.entry.source === CONSOLE_SOURCE);
    if (c) return c;
    return x.entry.key.localeCompare(y.entry.key);
  });
}

/**
 * A file name as a title: "007 First Light - PPSA11386 - v1.18.0 - US.exfat" → "007 First Light",
 * "PPSA09999 Venus.exfat" → "Venus" (the title id and what follows a " - " after it dropped).
 */
export function titleFromFileName(name: string, titleId: string): string {
  let n = name.replace(/\.(pkg|exfat|ffpkg|ffpfsc?)$/i, "").trim();
  if (titleId) {
    const at = n.toUpperCase().indexOf(titleId.toUpperCase());
    if (at >= 0) {
      const before = n.slice(0, at);
      // the name before the id ("Game - PPSA01234 - v1.00"), else after it ("[PPSA01234] Game")
      n = /[\p{L}\p{N}]/u.test(before) ? before : n.slice(at + titleId.length);
    }
  }
  n = n.replace(/^[\s\-_[\]().]+|[\s\-_[(]+$/g, "").trim();
  return n || name;
}

function bestTitle(parts: Part[]): string {
  const base = parts.find((p) => p.role === "base" && p.entry.title);
  const any = base ?? parts.find((p) => p.entry.title && p.role !== "dlc") ?? parts.find((p) => p.entry.title);
  if (any) return any.entry.title;
  const e = parts[0].entry;
  const id = baseTitleId(e);
  const fromName = titleFromFileName(e.name, id);
  // a file named by its title id alone ("PPSA23012.exfat") shows as the id, not as the file name
  if (id && fromName === e.name && nameKey(e.name) === id.toLowerCase()) return id;
  return fromName || displayName(e);
}

/** Why the game's title is not known: an unread disk image's cause (GameGroup.titleUnread). */
function unreadCause(parts: Part[]): string {
  if (parts.some((p) => p.entry.title)) return "";
  const img = parts.find((p) => p.entry.kind === "image" && p.entry.meta && p.entry.meta !== "read");
  return img ? img.entry.meta : "";
}

const maxFw = (a: string, b: string) => (!a ? b : !b ? a : compareVersions(a, b) >= 0 ? a : b);

/** One entry per game, sorted by title. */
export function groupEntries(entries: Entry[]): GameGroup[] {
  const by = new Map<string, Part[]>();
  for (const e of entries) {
    const k = groupKey(e);
    const list = by.get(k);
    const part: Part = { entry: e, role: roleOf(e), format: formatOf(e) };
    if (list) list.push(part);
    else by.set(k, [part]);
  }
  const out: GameGroup[] = [];
  for (const [key, raw] of Array.from(by.entries())) {
    const parts = sortParts(raw);
    const withIcon = parts.find((p) => p.role === "base" && p.entry.icon) ?? parts.find((p) => p.role !== "dlc" && p.entry.icon) ?? parts.find((p) => p.entry.icon);
    const formats: Format[] = [];
    const places: string[] = [];
    let fw = "";
    let sdk = "";
    for (const p of parts) {
      if (!formats.includes(p.format)) formats.push(p.format);
      if (!places.includes(p.entry.sourceName)) places.push(p.entry.sourceName);
      if (p.role !== "dlc") fw = maxFw(fw, p.entry.fw);
      if (p.role === "base" || p.role === "backport") sdk = maxFw(sdk, p.entry.sdk);
    }
    const first = parts[0].entry;
    const { bytes: size, partial: sizeUnknown } = gameSize(parts, new Map());
    out.push({
      key,
      title: bestTitle(parts),
      titleId: baseTitleId(first) || parts.map((p) => baseTitleId(p.entry)).find(Boolean) || "",
      platform: parts.map((p) => p.entry.platform).find(Boolean) ?? "",
      cover: withIcon ? withIcon.entry : null,
      parts,
      size,
      sizeUnknown,
      fw,
      sdk,
      formats,
      places,
      hasBase: parts.some((p) => p.role === "base" || p.role === "backport"),
      updates: parts.filter((p) => p.role === "update").length,
      dlc: parts.filter((p) => p.role === "dlc").length,
      backports: parts.filter((p) => p.role === "backport").length,
      fake: parts.some((p) => p.entry.fake),
      titleUnread: unreadCause(parts),
    });
  }
  return out.sort((a, b) => a.title.localeCompare(b.title));
}

/**
 * What the game takes: its largest base copy (a backport counts as one), with folder sizes from
 * `counted` (by entry key) where the scan left them at 0; all parts added up when there is no base.
 * `partial`: a part that counts has no size yet.
 */
export function gameSize(parts: Part[], counted: ReadonlyMap<string, { bytes: number; done: boolean }>): { bytes: number; partial: boolean } {
  const sizeOf = (p: Part) => {
    if (p.entry.size > 0) return { bytes: p.entry.size, known: true };
    const c = counted.get(p.entry.key);
    return { bytes: c?.bytes ?? 0, known: !!c?.done };
  };
  const bases = parts.filter((p) => p.role === "base" || p.role === "backport");
  if (bases.length) {
    let bytes = 0;
    let partial = false;
    for (const p of bases) {
      const s = sizeOf(p);
      bytes = Math.max(bytes, s.bytes);
      if (!s.known) partial = true;
    }
    // a known copy at least as big as any uncounted one could be is not waited for
    return { bytes, partial: partial && !bases.some((p) => p.entry.size > 0) };
  }
  let bytes = 0;
  let partial = false;
  for (const p of parts) {
    const s = sizeOf(p);
    bytes += s.bytes;
    if (!s.known) partial = true;
  }
  return { bytes, partial };
}

export function filterGroups(list: GameGroup[], query: string): GameGroup[] {
  const q = query.trim().toLowerCase();
  if (!q) return list;
  return list.filter((g) => `${g.title} ${g.titleId} ${g.parts.map((p) => p.entry.name).join(" ")}`.toLowerCase().includes(q));
}

/** The newest update among the parts (its version), "" when there is none. */
export function newestUpdate(g: GameGroup): string {
  return g.parts.find((p) => p.role === "update")?.entry.version ?? "";
}
