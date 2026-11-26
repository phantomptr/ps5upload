// One listing for the places the app can read besides the PS5 it is on: this computer (in the web
// UI, the computer the engine runs on), a saved server, and another console. The in-app file
// browser (LocalPathPicker) and Files' second pane both list through here.

import { localFs } from "../api/localFs";
import { remoteApi } from "../api/remote";
import { fsListDir } from "../api/ps5";
import { transferAddr } from "./addr";
import { remotePath } from "./remotePath";

export type Place =
  | { kind: "computer" }
  | { kind: "server"; id: string }
  | { kind: "console"; host: string };

export interface PlaceEntry {
  name: string;
  /** This computer: the full path. A server or a console: the path there ("/games/a.pkg"). */
  path: string;
  is_dir: boolean;
  size: number;
}

export interface PlacePage {
  entries: PlaceEntry[];
  /** Where the next page starts, when the folder has more. */
  more: string | null;
}

/** Where a game can live on a console: internal storage and the external drives. */
export const CONSOLE_ROOTS = ["/data", "/mnt/ext0", "/mnt/ext1", "/mnt/usb0", "/mnt/usb1"];

/** `dir` + `name` on a server or a console ("/" paths). */
export function joinPlacePath(dir: string, name: string): string {
  return `${dir.replace(/\/+$/, "")}/${name}`;
}

/** "/games/ps5" → "/games"; "/" has no parent. A server's and a console's paths. */
export function serverParent(path: string): string | null {
  const t = path.replace(/\/+$/, "");
  if (!t) return null;
  const i = t.lastIndexOf("/");
  return i <= 0 ? "/" : t.slice(0, i);
}

/** This computer's parent folder, with "/" or "\" (Windows) between names; null at a root. */
export function localParent(path: string): string | null {
  const t = path.replace(/[\\/]+$/, "");
  if (!t || /^[A-Za-z]:$/.test(t)) return null;
  const i = Math.max(t.lastIndexOf("/"), t.lastIndexOf("\\"));
  if (i < 0) return null;
  if (i === 0) return "/";
  const head = t.slice(0, i);
  // "C:\Users" → "C:\", not "C:".
  return /^[A-Za-z]:$/.test(head) ? `${head}\\` : head;
}

export function placeParent(place: Place, path: string): string | null {
  return place.kind === "computer" ? localParent(path) : serverParent(path);
}

/** The path a transfer names for a folder in `place`: a server's travels as `remote://`. */
export function placeTransferPath(place: Place, path: string): string {
  return place.kind === "server" ? remotePath(place.id, path) : path;
}

/** Same place (the same console counts once whatever its port). */
export function samePlace(a: Place | null, b: Place | null): boolean {
  if (!a || !b || a.kind !== b.kind) return false;
  if (a.kind === "server" && b.kind === "server") return a.id === b.id;
  if (a.kind === "console" && b.kind === "console") return a.host === b.host;
  return true;
}

/** One page of a folder in `place`, from `more` (a previous page's `more`) or the start. */
export async function listPlace(place: Place, path: string, more?: string | null): Promise<PlacePage> {
  if (place.kind === "console") {
    const list = await fsListDir(transferAddr(place.host), path);
    return {
      entries: list.map((e) => ({
        name: e.name,
        path: joinPlacePath(path, e.name),
        is_dir: e.kind === "dir",
        size: e.size ?? 0,
      })),
      more: null,
    };
  }
  if (place.kind === "server") {
    const page = await remoteApi.listDir(remotePath(place.id, path), more ?? undefined);
    return {
      entries: page.entries.map((e) => ({
        name: e.name,
        path: joinPlacePath(path, e.name),
        is_dir: e.is_dir,
        size: e.size,
      })),
      more: page.next_cursor,
    };
  }
  const page = await localFs.listDirPage(path, more ? Number(more) : 0);
  return { entries: page.entries, more: page.next === null ? null : String(page.next) };
}

/** The entry called `name` in a folder of `place`, reading page after page; null when absent. */
export async function findInPlace(
  place: Place,
  dir: string,
  name: string,
  list: typeof listPlace = listPlace,
): Promise<PlaceEntry | null> {
  let more: string | null = null;
  for (let i = 0; i < 1000; i++) {
    const page: PlacePage = await list(place, dir, more);
    const hit = page.entries.find((e) => e.name === name);
    if (hit) return hit;
    if (!page.more) return null;
    more = page.more;
  }
  return null;
}
