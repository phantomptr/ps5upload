/**
 * The game Library (#498): games on the saved network shares and on this console's drives, as the
 * SMB daemon's scan found them (payload/smb/smbd_library.h). A scan is started per source
 * (/api/library/scan {source}: a share's id, or "console") and runs as a daemon job; its result
 * stays on the console as LIB_DIR/<source>.json with the covers in LIB_DIR/icons, so the grid opens
 * from that cache at once and a rescan is only on demand.
 */
import { ApiError, call, callBlob } from "../api";

export const LIB_DIR = "/data/ps5upload/library";
/** Where a package from a share is copied before it is installed (the helper's staging folder: the install removes it). */
export const STAGE_DIR = "/user/data/ps5upload/pkg_temp";
export const CONSOLE_SOURCE = "console";

export interface LibItem {
  kind: "pkg" | "folder" | "image";
  /** Share-relative for a share, absolute on the console. */
  path: string;
  /** The file or folder name. */
  name: string;
  title: string;
  title_id: string;
  content_id: string;
  version: string;
  type: "game" | "patch" | "dlc" | "app" | "other";
  platform: string;
  fw: string;
  /** The SDK it was built with ("4.50", param.json sdkVersion / PUBTOOLINFO), "" when unknown (older helpers). */
  sdk: string;
  /** A folder holding a fakelib/ folder: a backported copy (older helpers never say). */
  backport: boolean;
  fake: boolean;
  size: number;
  /** A file name in LIB_DIR/icons, "" without a cover. */
  icon: string;
}

export interface LibScan {
  scanned: number;
  complete: boolean;
  /** Why the walk ended early ("items", "entries", "time"), or null. */
  stopped: string | null;
  items: LibItem[];
}

export interface LibSource {
  id: string;
  name: string;
}

/** One game of the grid. */
export interface Entry extends LibItem {
  key: string;
  source: string;
  sourceName: string;
  /** The path Files and the copy routes take: "smb://<id>/<path>" on a share. */
  at: string;
}

const KINDS = new Set(["pkg", "folder", "image"]);
const TYPES = new Set(["game", "patch", "dlc", "app", "other"]);
const str = (v: unknown): string => (typeof v === "string" ? v : "");

/** A cached scan's JSON, checked field by field; null when it is not one. */
export function parseScan(text: string): LibScan | null {
  let j: unknown;
  try {
    j = JSON.parse(text);
  } catch {
    return null;
  }
  if (!j || typeof j !== "object" || !Array.isArray((j as { items?: unknown }).items)) return null;
  const o = j as Record<string, unknown>;
  const items: LibItem[] = [];
  for (const raw of o.items as unknown[]) {
    if (!raw || typeof raw !== "object") continue;
    const r = raw as Record<string, unknown>;
    const kind = str(r.kind);
    if (!KINDS.has(kind) || !str(r.path)) continue;
    const type = str(r.type);
    items.push({
      kind: kind as LibItem["kind"],
      path: str(r.path),
      name: str(r.name),
      title: str(r.title),
      title_id: str(r.title_id),
      content_id: str(r.content_id),
      version: str(r.version),
      type: (TYPES.has(type) ? type : "other") as LibItem["type"],
      platform: str(r.platform),
      fw: str(r.fw),
      sdk: str(r.sdk),
      backport: r.backport === true,
      fake: r.fake === true,
      size: typeof r.size === "number" ? r.size : 0,
      icon: /^[\w-]+\.png$/.test(str(r.icon)) ? str(r.icon) : "",
    });
  }
  return {
    scanned: typeof o.scanned === "number" ? o.scanned : 0,
    complete: o.complete === true,
    stopped: typeof o.stopped === "string" ? o.stopped : null,
    items,
  };
}

/** The cached scan of `source`; null when it was never scanned. */
export async function readScan(source: string): Promise<LibScan | null> {
  try {
    const b = await callBlob("/api/fs/read", { path: `${LIB_DIR}/${source}.json` });
    // Response, not Blob.text(): the page's safari13 floor has no Blob.text.
    return parseScan(await new Response(b).text());
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}

export async function startScan(source: string): Promise<string> {
  const r = await call<{ job?: unknown }>("/api/library/scan", { source });
  if (!r || typeof r.job !== "string") throw new ApiError(500, "bad_reply");
  return r.job;
}

/** What the grid calls it: the title, else the file name without its extension. */
export function displayName(i: Pick<LibItem, "title" | "name" | "title_id">): string {
  if (i.title) return i.title;
  const base = i.name.replace(/\.(pkg|exfat|ffpkg|ffpfsc?|)$/i, "");
  return base || i.title_id || "?";
}

export function entriesOf(sources: LibSource[], scans: ReadonlyMap<string, LibScan | null>): Entry[] {
  const out: Entry[] = [];
  for (const s of sources) {
    const scan = scans.get(s.id);
    if (!scan) continue;
    for (const it of scan.items)
      out.push({
        ...it,
        key: `${s.id}:${it.path}`,
        source: s.id,
        sourceName: s.name,
        at: s.id === CONSOLE_SOURCE ? it.path : `smb://${s.id}/${it.path}`,
      });
  }
  return out.sort((a, b) => displayName(a).localeCompare(displayName(b)));
}

/** Where a copy of `e` goes on drive `drive` ("/data", "/mnt/usb0"…): its homebrew folder, where ShadowMount+ and the PC app look. */
export function copyDest(drive: string, e: Pick<Entry, "name" | "path">): string {
  const name = e.name || e.path.split("/").pop() || "game";
  return `${drive === "/" ? "" : drive}/homebrew/${name}`;
}

/** A fresh name in the staging folder for a package copied from a share. */
export function stagePath(now = Date.now()): string {
  return `${STAGE_DIR}/library-${now.toString(36)}.pkg`;
}
