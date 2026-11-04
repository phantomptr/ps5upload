/**
 * The Games section's data: the console's installed titles, built the way the PC app's
 * engine builds its list (ps5_apps_installed in engine/crates/ps5upload-engine/src/lib.rs):
 *
 *   - every /user/appmeta/<id> folder is a title;
 *   - an app.db row counts too when its app folder exists (/user/app, /mnt/ext0/user/app,
 *     /mnt/ext1/user/app): homebrew installers on FW 13.60 leave no appmeta folder;
 *   - titles we registered from a folder or an image (APP_LIST) carry their source.
 *
 * Names come from app.db, then our registration, then the title id. The page's own tile
 * (PSUP00001) and the console's own apps (NPXS) are left out: neither is a game, and
 * neither may be uninstalled or hidden from here. Each title's version, size and last-played
 * time come from appinfo.db (the same keys the PC app's game page reads).
 */
import { compareNames } from "../../lib/collate";
import { ApiError, call } from "../api";

export const TILE_TITLE_ID = "PSUP00001";
/** A title folder's name: four capitals and five digits (the engine's looks_like_title_id). */
export const TITLE_ID_RE = /^[A-Z]{4}\d{5}$/;
/** The app folders, as the engine and the helper's uninstall check read them. */
export const APP_ROOTS = ["/user/app", "/mnt/ext0/user/app", "/mnt/ext1/user/app"] as const;
const APPMETA = "/user/appmeta";
/** fs.list's page (the helper's ceiling). */
const LIST_PAGE = 256;
/** A folder read stops after this many entries (well past any console's library). */
const LIST_CAP = 4096;
/** app.db pages read at most (500 rows each). */
const DB_PAGES_MAX = 20;
/** Titles one /api/apps/info asks about (the helper's API_APPS_INFO_MAX). */
export const INFO_CHUNK = 32;

export type Origin = "pkg" | "registered";
/** Where a title lives: a drive, or a disk image / folder we registered. */
export type Place = "internal" | "ext" | "usb" | "image" | "folder" | "unknown";

export interface Game {
  titleId: string;
  name: string;
  origin: Origin;
  imageBacked: boolean;
  /** The folder or image we registered it from ("" for a package install). */
  source: string;
  place: Place;
  /** Its home-screen icon is hidden; null when the console could not say. */
  hidden: boolean | null;
  version: string | null;
  sizeBytes: number | null;
  /** When it was last opened (ms since 1970), or null. */
  lastPlayed: number | null;
}

export interface Installed {
  games: Game[];
  /** The list may be missing titles (a listing or app.db was cut short or failed). */
  incomplete: boolean;
  /** The hidden-icon list was read (else Hide/Show are off). */
  hiddenKnown: boolean;
}

export type CallFn = <T>(route: string, body?: object) => Promise<T>;

interface ListEntry {
  name?: unknown;
  kind?: unknown;
}

/** Every folder name under `path` (folders and links), page by page. Throws the read's error. */
export async function listDirs(callFn: CallFn, path: string): Promise<{ names: string[]; whole: boolean }> {
  const names: string[] = [];
  let offset = 0;
  for (;;) {
    const r = await callFn<{ entries?: ListEntry[]; more?: boolean; next?: number } | null>("/api/fs/list", {
      path,
      offset,
      limit: LIST_PAGE,
    });
    const entries = Array.isArray(r?.entries) ? r.entries : [];
    for (const e of entries) {
      if (typeof e?.name === "string" && (e.kind === "dir" || e.kind === "link")) names.push(e.name);
    }
    if (!r?.more || !entries.length) return { names, whole: true };
    offset = typeof r.next === "number" && r.next > offset ? r.next : offset + entries.length;
    if (offset >= LIST_CAP) return { names, whole: false };
  }
}

export interface Registered {
  titleId: string;
  name: string;
  source: string;
  imageBacked: boolean;
}

/** /api/apps/registered: {"more":bool,"reply":{"apps":[{title_id,title_name,src,image_backed}]}}. */
export function parseRegistered(raw: unknown): { apps: Map<string, Registered>; whole: boolean } {
  const apps = new Map<string, Registered>();
  const r = raw as { more?: unknown; reply?: { apps?: unknown } } | null;
  const list = Array.isArray(r?.reply?.apps) ? (r.reply.apps as Record<string, unknown>[]) : [];
  for (const a of list) {
    const id = typeof a?.title_id === "string" ? a.title_id.trim() : "";
    if (!id) continue;
    apps.set(id, {
      titleId: id,
      name: typeof a.title_name === "string" ? a.title_name.trim() : "",
      source: typeof a.src === "string" ? a.src : "",
      imageBacked: a.image_backed === true,
    });
  }
  return { apps, whole: r?.more !== true };
}

/** One /api/apps/db page: {"apps":[{title_id,app_id,name}],"next":N,"more":bool} or {"err":..}. */
export function parseDbPage(raw: unknown): { rows: [string, string][]; more: boolean; next: number; ok: boolean } {
  const r = raw as { apps?: unknown; more?: unknown; next?: unknown; err?: unknown; truncated?: unknown } | null;
  const rows: [string, string][] = [];
  const list = Array.isArray(r?.apps) ? (r.apps as Record<string, unknown>[]) : [];
  for (const a of list) {
    if (typeof a?.title_id === "string") rows.push([a.title_id.trim(), typeof a.name === "string" ? a.name.trim() : ""]);
  }
  const ok = !!r && typeof r === "object" && !(typeof r.err === "string" && r.err) && r.truncated !== true;
  return { rows, more: r?.more === true, next: typeof r?.next === "number" ? r.next : 0, ok };
}

/** /api/apps/hidden: {"ok":true,"hidden":[..]}; null when it did not answer that. */
export function parseHidden(raw: unknown): Set<string> | null {
  const r = raw as { ok?: unknown; hidden?: unknown } | null;
  if (!r || r.ok !== true || !Array.isArray(r.hidden)) return null;
  return new Set(r.hidden.filter((x): x is string => typeof x === "string"));
}

export interface AppInfo {
  version: string | null;
  sizeBytes: number | null;
  lastPlayed: number | null;
}

/**
 * appinfo.db's time text as ms since 1970, or null. Seen as a plain number (seconds or ms) or as
 * a date ("2026-10-20 12:34:56", taken as UTC); zero and anything unreadable are null.
 */
export function parseInfoTime(v: string | null | undefined): number | null {
  const s = (v ?? "").trim();
  if (!s) return null;
  if (/^\d+$/.test(s)) {
    const n = Number(s);
    if (!Number.isFinite(n) || n <= 0) return null;
    return n < 1e12 ? n * 1000 : n;
  }
  const iso = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(:\d{2}(\.\d+)?)?$/.test(s) ? `${s.replace(" ", "T")}Z` : s;
  const t = Date.parse(iso);
  return Number.isFinite(t) && t > 0 ? t : null;
}

/** One title's APP_INFO_QUERY reply ({"ok":true,"rows":[{key,val}]}). */
export function parseInfo(raw: unknown): AppInfo {
  const rows = Array.isArray((raw as { rows?: unknown } | null)?.rows)
    ? ((raw as { rows: { key?: unknown; val?: unknown }[] }).rows ?? [])
    : [];
  const v = new Map<string, string>();
  for (const r of rows) {
    if (typeof r?.key === "string" && typeof r.val === "string" && r.val.trim()) v.set(r.key, r.val.trim());
  }
  const size = Number(v.get("#_size"));
  return {
    version: v.get("CONTENT_VERSION") ?? v.get("APP_VER") ?? null,
    sizeBytes: Number.isFinite(size) && size > 0 ? size : null,
    lastPlayed: parseInfoTime(v.get("#_last_access_time")),
  };
}

/** Where a title lives. A registered one by its source; a package install by its app folder. */
export function placeOf(origin: Origin, imageBacked: boolean, source: string, appRoot: string | null): Place {
  if (origin === "registered") {
    if (imageBacked) return "image";
    if (/^\/mnt\/usb\d+(\/|$)/.test(source)) return "usb";
    if (/^\/mnt\/ext\d+(\/|$)/.test(source)) return "ext";
    if (/^\/(data|user)(\/|$)/.test(source)) return "internal";
    if (source) return "folder";
  }
  if (appRoot === "/user/app") return "internal";
  if (appRoot && appRoot.startsWith("/mnt/ext")) return "ext";
  return "unknown";
}

/** Neither a game nor ours to touch: the page's own tile and the console's own apps. */
export function skipTitle(id: string): boolean {
  return !TITLE_ID_RE.test(id) || id.toUpperCase() === TILE_TITLE_ID || id.startsWith("NPXS");
}

export interface Sources {
  appmeta: string[];
  /** Title id → the first app root holding its folder. */
  appDirs: Map<string, string>;
  dbRows: [string, string][] | null;
  registered: Map<string, Registered>;
  hidden: Set<string> | null;
}

/** The installed titles from what the console reported (no I/O: tested directly). */
export function assemble(s: Sources): Game[] {
  const dbName = new Map<string, string>();
  for (const [id, name] of s.dbRows ?? []) if (name && name !== id && !dbName.has(id)) dbName.set(id, name);
  const out: Game[] = [];
  const seen = new Set<string>();
  const add = (id: string) => {
    if (seen.has(id) || skipTitle(id)) return;
    seen.add(id);
    const reg = s.registered.get(id);
    const regName = reg && reg.name && reg.name !== id ? reg.name : "";
    const origin: Origin = reg ? "registered" : "pkg";
    out.push({
      titleId: id,
      name: dbName.get(id) || regName || id,
      origin,
      imageBacked: !!reg?.imageBacked,
      source: reg?.source ?? "",
      place: placeOf(origin, !!reg?.imageBacked, reg?.source ?? "", s.appDirs.get(id) ?? null),
      hidden: s.hidden ? s.hidden.has(id) : null,
      version: null,
      sizeBytes: null,
      lastPlayed: null,
    });
  };
  for (const id of s.appmeta) add(id);
  // app.db rows count only with their app folder (preinstalled tiles and shell entries have none).
  for (const [id] of s.dbRows ?? []) if (s.appDirs.has(id)) add(id);
  return out;
}

/** Read everything the list needs (not the per-title details). Throws when /user/appmeta cannot be read. */
export async function loadSources(callFn: CallFn = call): Promise<{ sources: Sources; incomplete: boolean }> {
  let incomplete = false;
  const meta = await listDirs(callFn, APPMETA);
  if (!meta.whole) incomplete = true;
  const appDirs = new Map<string, string>();
  for (const root of APP_ROOTS) {
    try {
      const l = await listDirs(callFn, root);
      for (const n of l.names) if (!appDirs.has(n)) appDirs.set(n, root);
    } catch {
      // An extended drive that is not there: nothing installed on it.
    }
  }
  let registered = new Map<string, Registered>();
  try {
    const r = parseRegistered(await callFn<unknown>("/api/apps/registered"));
    registered = r.apps;
    if (!r.whole) incomplete = true;
  } catch {
    incomplete = true;
  }
  let dbRows: [string, string][] | null = [];
  try {
    let offset = 0;
    for (let page = 0; page < DB_PAGES_MAX; page++) {
      const p = parseDbPage(await callFn<unknown>("/api/apps/db", { offset }));
      dbRows.push(...p.rows);
      if (!p.ok) incomplete = true;
      if (!p.more || p.next <= offset) break;
      offset = p.next;
      if (page === DB_PAGES_MAX - 1) incomplete = true;
    }
  } catch {
    dbRows = null;
    incomplete = true;
  }
  let hidden: Set<string> | null;
  try {
    hidden = parseHidden(await callFn<unknown>("/api/apps/hidden"));
  } catch {
    hidden = null;
  }
  return { sources: { appmeta: meta.names, appDirs, dbRows, registered, hidden }, incomplete };
}

/** Each title's appinfo.db details, INFO_CHUNK at a time (one helper worker). Never throws. */
export async function loadInfo(callFn: CallFn, ids: string[]): Promise<Map<string, AppInfo>> {
  const out = new Map<string, AppInfo>();
  for (let i = 0; i < ids.length; i += INFO_CHUNK) {
    try {
      const r = await callFn<{ apps?: Record<string, unknown> } | null>("/api/apps/info", {
        title_ids: ids.slice(i, i + INFO_CHUNK),
      });
      for (const [id, raw] of Object.entries(r?.apps ?? {})) out.set(id, parseInfo(raw));
    } catch {
      // Details are a nicety: the list still shows without them.
    }
  }
  return out;
}

/** The whole list with details. Throws when /user/appmeta cannot be read. */
export async function loadInstalled(callFn: CallFn = call): Promise<Installed> {
  const { sources, incomplete } = await loadSources(callFn);
  const games = assemble(sources);
  const info = await loadInfo(
    callFn,
    games.map((g) => g.titleId),
  );
  for (const g of games) {
    const i = info.get(g.titleId);
    if (i) Object.assign(g, i);
  }
  return { games, incomplete, hiddenKnown: sources.hidden !== null };
}

/** Whether `titleId` is still installed, by the same rules (the uninstall's follow-up). */
export async function stillInstalled(titleId: string, callFn: CallFn = call): Promise<boolean> {
  const { sources } = await loadSources(callFn);
  return assemble(sources).some((g) => g.titleId === titleId);
}

/* ---- running games (the process list, as the PC app's runningGames.ts reads it) ---- */

export interface Proc {
  pid: number;
  name: string;
  comm: string;
  titleId: string;
  appId: number;
  memoryMib: number | null;
  threads: number | null;
  kind: "app" | "payload" | "system";
  isSelf: boolean;
}

/** /api/proc/list: {"ok":true,"procs":[{pid,name,comm,title_id,app_id,memory_mib,threads,kind,is_self}]}. */
export function parseProcesses(raw: unknown): { procs: Proc[]; truncated: boolean } {
  const list = Array.isArray((raw as { procs?: unknown } | null)?.procs)
    ? ((raw as { procs: Record<string, unknown>[] }).procs ?? [])
    : [];
  const procs: Proc[] = [];
  let truncated = false;
  for (const p of list) {
    if (p?.truncated === true) {
      truncated = true;
      continue;
    }
    if (typeof p?.pid !== "number") continue;
    const kind = p.kind === "app" || p.kind === "payload" ? p.kind : "system";
    procs.push({
      pid: p.pid,
      name: typeof p.name === "string" ? p.name : "",
      comm: typeof p.comm === "string" ? p.comm : "",
      titleId: typeof p.title_id === "string" ? p.title_id : "",
      appId: typeof p.app_id === "number" ? p.app_id : 0,
      memoryMib: typeof p.memory_mib === "number" ? p.memory_mib : null,
      threads: typeof p.threads === "number" ? p.threads : null,
      kind,
      isSelf: p.is_self === true,
    });
  }
  return { procs, truncated };
}

export interface RunningGame {
  appId: number;
  pid: number;
}

/** Title id → its running game process (kind app, not the helper; one with an app id preferred). */
export function runningGames(procs: Proc[]): Map<string, RunningGame> {
  const out = new Map<string, RunningGame>();
  for (const p of procs) {
    if (p.kind !== "app" || !p.titleId || p.isSelf) continue;
    const cur = out.get(p.titleId);
    if (!cur || (!cur.appId && p.appId)) out.set(p.titleId, { appId: p.appId, pid: p.pid });
  }
  return out;
}

/* ---- order and filter ---- */

export type SortMode = "recent" | "name";
export type Show = "all" | "hidden";

/** Running first; then (recent) the last played first, never-played after; then by name. */
export function sortGames(games: Game[], running: ReadonlySet<string>, mode: SortMode): Game[] {
  const byName = (a: Game, b: Game) =>
    compareNames(a.name, b.name) || a.titleId.localeCompare(b.titleId);
  return games.slice().sort((a, b) => {
    const ra = running.has(a.titleId) ? 0 : 1;
    const rb = running.has(b.titleId) ? 0 : 1;
    if (ra !== rb) return ra - rb;
    if (mode === "recent") {
      const la = a.lastPlayed ?? -1;
      const lb = b.lastPlayed ?? -1;
      if (la !== lb) return lb - la;
    }
    return byName(a, b);
  });
}

/** The titles matching `query` (name or title id, any case) and `show`. */
export function filterGames(games: Game[], query: string, show: Show): Game[] {
  const q = query.trim().toLowerCase();
  return games.filter(
    (g) =>
      (show === "all" || g.hidden === true) &&
      (!q || g.name.toLowerCase().includes(q) || g.titleId.toLowerCase().includes(q)),
  );
}

/* ---- errors ---- */

/** The launcher's "this console will not start non-Sony titles" refusal (no kstuff, or ShadowMount+ did not mount). */
export function isHomebrewRefusal(code: string): boolean {
  return /0x80940033/i.test(code);
}

/** An uninstall error's Sony code, from "uninstall_refused:0x8..." */
export function refusedCode(code: string): string | null {
  const m = /^uninstall_refused:(0x[0-9a-f]+)$/i.exec(code);
  return m ? m[1] : null;
}

/** The ApiError's code, or the message of anything else. */
export function codeOf(err: unknown): string {
  if (err instanceof ApiError) return err.code;
  return err instanceof Error ? err.message : String(err);
}
