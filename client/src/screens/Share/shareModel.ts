// Share's rules without React: which app copies can be shared, what POST /api/share gets from
// the panel's choices, and the history's lines. The engine checks everything again.

import type { CollectionLocation } from "../../api/collection";
import type { ShareCreate, ShareItemSpec } from "../../api/share";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";

/** How long a link works: 1 hour, 24 hours (the default), 7 days. */
export const EXPIRY_CHOICES = [3600, 24 * 3600, 7 * 24 * 3600] as const;
export const DEFAULT_EXPIRY = 24 * 3600;
export const NOTE_MAX = 2000;

const NAME_KEY = "ps5upload.share_name.v1";

/** The name a friend sees, as last typed on this device. */
export function savedShareName(): string {
  return safeGetItem(NAME_KEY) ?? "";
}

export function saveShareName(name: string): void {
  safeSetItem(NAME_KEY, name.trim().slice(0, 40));
}

/** Where the app itself can be shared from. */
export type AppSource =
  | { kind: "console"; path: string; size?: number }
  | { kind: "copy"; path: string; label: string; format: string; size?: number };

/** The formats the engine can read an app's identity from (not an archive). */
const SHAREABLE_FORMATS = ["folder", "pkg", "mount.exfat", "mount.ffpkg", "mount.ffpfs", "mount.ffpfsc"];

/** A console path that is a game folder (not an image the console mounts). */
export function isConsoleFolder(path: string | null | undefined): path is string {
  return !!path && path.startsWith("/") && !/\.(exfat|ffpkg|ffpfs|ffpfsc|pkg)$/i.test(path);
}

/** The app sources a game offers: its folder on the connected console, then each Collection
 *  copy in a format the engine reads (base copies only: an update or add-on is never shared). */
export function appSources(consolePath: string | null, copies: CollectionLocation[]): AppSource[] {
  const out: AppSource[] = [];
  if (isConsoleFolder(consolePath)) out.push({ kind: "console", path: consolePath });
  for (const c of copies) {
    if (!SHAREABLE_FORMATS.includes(c.type)) continue;
    const k = c.pkg?.kind;
    if (k === "patch" || k === "dlc") continue;
    out.push({ kind: "copy", path: c.absolute_path, label: c.name, format: c.type, size: c.size_bytes });
  }
  return out;
}

/** What the panel's choices are. */
export interface ShareChoices {
  from: string;
  title: string;
  titleId: string;
  /** The console's address, for console items. */
  console: string | null;
  app: AppSource | null;
  /** Save folders on the console. */
  saves: string[];
  /** Screenshots and clips on the console. */
  media: string[];
  /** Which cheats are on, and the list. */
  cheats: { name: string; on: boolean }[] | null;
  controls: unknown | null;
  note: string;
  expiresInS: number;
  oneUse: boolean;
}

/** The app item alone, for the engine's check. */
export function appItem(app: AppSource, console: string | null): ShareItemSpec {
  return app.kind === "console"
    ? { kind: "app", console: console ?? undefined, path: app.path }
    : { kind: "app", path: app.path };
}

/** POST /api/share's body for the choices. */
export function createBody(c: ShareChoices): ShareCreate {
  const items: ShareItemSpec[] = [];
  if (c.app) items.push(appItem(c.app, c.console));
  if (c.console) {
    for (const folder of c.saves) items.push({ kind: "save", console: c.console, folder });
    if (c.media.length > 0) items.push({ kind: "media", console: c.console, paths: c.media });
  }
  if (c.cheats && c.cheats.length > 0) {
    items.push({
      kind: "cheats",
      data: { title_id: c.titleId, on: c.cheats.filter((m) => m.on).map((m) => m.name), all: c.cheats.map((m) => m.name) },
    });
  }
  if (c.controls) items.push({ kind: "controls", data: c.controls });
  const note = c.note.trim().slice(0, NOTE_MAX);
  if (note) items.push({ kind: "note", text: note });
  return {
    from: c.from.trim() || undefined,
    title: c.title,
    title_id: c.titleId,
    expires_in_s: c.expiresInS,
    one_use: c.oneUse,
    items,
  };
}

/** Screenshots and clips of one game: the PS5 files them in a folder per game (named by its
 *  Title ID, or the game's name). */
export function mediaOf(paths: { path: string }[], titleId: string, title: string): string[] {
  const tid = titleId.toUpperCase();
  const name = title.trim().toLowerCase();
  return paths
    .map((p) => p.path)
    .filter((p) => {
      const parts = p.split("/");
      return parts.some((s) => s.toUpperCase() === tid || (!!name && s.toLowerCase() === name));
    });
}

/** How many of a game's media are screenshots and how many are clips (clips live under
 *  `/video/`). */
export function mediaCounts(paths: string[]): { shots: number; clips: number } {
  const clips = paths.filter((p) => /\/video\//i.test(p) || /\.(webm|mp4)$/i.test(p)).length;
  return { shots: paths.length - clips, clips };
}

/** "23 h 59 min", "12 min", "under a minute": what is left of a link's life. */
export function timeLeft(seconds: number): { h: number; m: number } {
  const s = Math.max(0, Math.floor(seconds));
  return { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) };
}

/** Whole percent of a transfer, 0 when nothing is known. */
export function percent(done: number, total: number): number {
  if (!total || total <= 0) return 0;
  return Math.max(0, Math.min(100, Math.floor((done / total) * 100)));
}
