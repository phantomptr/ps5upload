// The Collection screen's filtering, searching and sorting: pure, so it is tested without a
// screen. Filters and sorts are PS Game Library's, plus platform.

import { compareText } from "./collate";
import type {
  CollectionGame,
  CollectionLocation,
  GameConsoleState,
} from "../api/collection";
import { firmwareShortfall, shrinkCandidate } from "./collectionInsights";

export type CollectionFilter =
  | "all"
  | "duplicates"
  | "cheats"
  | "smaller"
  | "pkg"
  | "mount"
  | "folder"
  | "rar"
  | "7z"
  | "zip";

export const COLLECTION_FILTERS: CollectionFilter[] = [
  "all",
  "duplicates",
  "cheats",
  "smaller",
  "pkg",
  "mount",
  "folder",
  "rar",
  "7z",
  "zip",
];

export type CollectionPlatform = "all" | "PS5" | "PS4" | "PS3" | "other";

export type CollectionSort =
  | "title-asc"
  | "title-desc"
  | "id-asc"
  | "id-desc"
  | "added-desc"
  | "added-asc"
  | "size-desc"
  | "size-asc"
  | "locations-desc"
  | "played-desc";

export const COLLECTION_SORTS: CollectionSort[] = [
  "title-asc",
  "title-desc",
  "id-asc",
  "id-desc",
  "added-desc",
  "added-asc",
  "size-desc",
  "size-asc",
  "locations-desc",
  "played-desc",
];

/** What a game is to the selected console. `addons`: installed, and this computer has a newer
 *  update or DLC the console lacks (one filter: "Update available" and "DLC missing" read as
 *  the console's own store state, which they are not); `drives`: it has game files on the
 *  console's drives (a folder, or an image it comes from); `unplayed`: installed and never seen
 *  played. */
export type ConsoleFilter =
  | "any"
  | "installed"
  | "missing"
  | "addons"
  | "firmware"
  | "drives"
  | "unplayed";

const CONSOLE_FILTERS: ConsoleFilter[] = [
  "any",
  "installed",
  "missing",
  "addons",
  "firmware",
  "drives",
  "unplayed",
];

/** A console filter by name, for links: the old `update` and `dlc` are `addons` now; anything
 *  unknown is null. */
export function consoleFilterOf(
  v: string | null | undefined,
): ConsoleFilter | null {
  if (v === "update" || v === "dlc") return "addons";
  return CONSOLE_FILTERS.find((f) => f === v) ?? null;
}

/** Per-filter counts for the chips, and the two halves of `addons` for the catch-up banner. */
export type ConsoleCounts = Record<Exclude<ConsoleFilter, "any">, number> & {
  /** Installed games with a newer update here. */
  update: number;
  /** Installed games with DLC here the console lacks. */
  dlc: number;
};

/** What the filters and sorts look at beyond the games themselves. */
export interface CollectionContext {
  /** Title IDs (upper case) the cheat index has cheats for; null until it is read. */
  cheatIds?: Set<string> | null;
  /** The selected console's firmware ("9.60"), when known. */
  consoleFw?: string | null;
  /** When a game was last seen being played on the selected console (ms). */
  lastPlayed?: (gameId: string) => number | undefined;
  /** Title IDs (upper case) with game files on the selected console's drives. */
  driveIds?: Set<string> | null;
  /** Seconds a game was seen played on the selected console. */
  playSeconds?: (gameId: string) => number | undefined;
}

export interface CollectionQuery extends CollectionContext {
  search: string;
  filter: CollectionFilter;
  platform: CollectionPlatform;
  sort: CollectionSort;
  console?: ConsoleFilter;
  /** The selected console's state per game; absent when no console is connected. */
  consoleStates?: Map<string, GameConsoleState>;
}

export function matchesConsole(
  g: CollectionGame,
  f: ConsoleFilter | undefined,
  states: Map<string, GameConsoleState> | undefined,
  consoleFw?: string | null,
  ctx: Pick<CollectionContext, "driveIds" | "playSeconds"> = {},
): boolean {
  if (f === "firmware")
    return !!consoleFw && firmwareShortfall(g, consoleFw) !== null;
  // Not read yet (no console): no filtering, as for the others.
  if (f === "drives")
    return !ctx.driveIds || ctx.driveIds.has(g.game_id.toUpperCase());
  if (!f || f === "any" || !states) return true;
  const st = states.get(g.game_id);
  if (!st) return false;
  if (f === "installed") return st.installed;
  if (f === "unplayed")
    return st.installed && !((ctx.playSeconds?.(g.game_id) ?? 0) > 0);
  if (f === "missing") return !st.installed;
  return st.installed && (!!st.update || st.dlc_missing.length > 0);
}

export function consoleCounts(
  games: CollectionGame[],
  states: Map<string, GameConsoleState> | undefined,
  consoleFw?: string | null,
  ctx: Pick<CollectionContext, "driveIds" | "playSeconds"> = {},
): ConsoleCounts {
  const out: ConsoleCounts = {
    installed: 0,
    missing: 0,
    addons: 0,
    update: 0,
    dlc: 0,
    firmware: 0,
    drives: 0,
    unplayed: 0,
  };
  if (consoleFw)
    for (const g of games)
      if (matchesConsole(g, "firmware", states, consoleFw)) out.firmware += 1;
  if (ctx.driveIds)
    for (const g of games)
      if (matchesConsole(g, "drives", states, consoleFw, ctx)) out.drives += 1;
  if (!states) return out;
  for (const g of games)
    if (matchesConsole(g, "unplayed", states, consoleFw, ctx))
      out.unplayed += 1;
  for (const g of games) {
    if (matchesConsole(g, "installed", states)) out.installed += 1;
    if (matchesConsole(g, "missing", states)) out.missing += 1;
    if (matchesConsole(g, "addons", states)) out.addons += 1;
    const st = states.get(g.game_id);
    if (st?.installed && st.update) out.update += 1;
    if (st?.installed && st.dlc_missing.length > 0) out.dlc += 1;
  }
  return out;
}

function matchesFilter(
  g: CollectionGame,
  f: CollectionFilter,
  ctx: CollectionContext,
): boolean {
  if (f === "all") return true;
  if (f === "duplicates") return g.is_duplicate;
  if (f === "cheats") return !!ctx.cheatIds?.has(g.game_id.toUpperCase());
  if (f === "smaller") return shrinkCandidate(g) !== null;
  return g.locations.some((l) => l.type.split(".")[0] === f);
}

function matchesPlatform(g: CollectionGame, p: CollectionPlatform): boolean {
  if (p === "all") return true;
  const plat = g.platform.toUpperCase();
  if (p === "other") return !["PS5", "PS4", "PS3"].includes(plat);
  return plat === p;
}

/** Title, Game ID, content IDs, file names and paths, case-insensitively. */
function matchesSearch(g: CollectionGame, q: string): boolean {
  const s = q.trim().toLowerCase();
  if (!s) return true;
  if (g.title.toLowerCase().includes(s) || g.game_id.toLowerCase().includes(s))
    return true;
  return g.locations.some(
    (l) =>
      l.path.toLowerCase().includes(s) ||
      (l.pkg?.content_id ?? "").toLowerCase().includes(s) ||
      (l.pkg?.region ?? "").toLowerCase() === s,
  );
}

function compare(
  a: CollectionGame,
  b: CollectionGame,
  sort: CollectionSort,
  ctx: CollectionContext,
): number {
  const byTitle = compareText(a.title, b.title);
  switch (sort) {
    case "title-asc":
      return byTitle;
    case "title-desc":
      return -byTitle;
    case "id-asc":
      return a.game_id.localeCompare(b.game_id);
    case "id-desc":
      return b.game_id.localeCompare(a.game_id);
    case "added-desc":
      return b.added_ts - a.added_ts || byTitle;
    case "added-asc":
      return a.added_ts - b.added_ts || byTitle;
    case "size-desc":
      return b.total_size_bytes - a.total_size_bytes || byTitle;
    case "size-asc":
      return a.total_size_bytes - b.total_size_bytes || byTitle;
    case "locations-desc":
      return b.locations.length - a.locations.length || byTitle;
    case "played-desc":
      // Never seen played sorts last, by title.
      return (
        (ctx.lastPlayed?.(b.game_id) ?? 0) -
          (ctx.lastPlayed?.(a.game_id) ?? 0) || byTitle
      );
  }
}

export function viewGames(
  games: CollectionGame[],
  q: CollectionQuery,
): CollectionGame[] {
  return games
    .filter(
      (g) =>
        matchesFilter(g, q.filter, q) &&
        matchesPlatform(g, q.platform) &&
        matchesConsole(g, q.console, q.consoleStates, q.consoleFw, q) &&
        matchesSearch(g, q.search),
    )
    .sort((a, b) => compare(a, b, q.sort, q));
}

/** How many of each filter's games there are, for the chips. */
export function filterCounts(
  games: CollectionGame[],
  ctx: CollectionContext = {},
): Record<CollectionFilter, number> {
  const out = Object.fromEntries(
    COLLECTION_FILTERS.map((f) => [f, 0]),
  ) as Record<CollectionFilter, number>;
  for (const g of games)
    for (const f of COLLECTION_FILTERS)
      if (matchesFilter(g, f, ctx)) out[f] += 1;
  return out;
}

/** Add-ons a game has: its patches and DLC. */
export function addOnCount(g: CollectionGame): number {
  return g.locations.filter(
    (l) => l.pkg?.kind === "patch" || l.pkg?.kind === "dlc",
  ).length;
}

/** A game's full copies beyond the largest of each release: what "Reclaimable" counts (never
 *  an add-on). Copies are one release only when they carry the same content ID, as the
 *  engine decides: a package under the game's ID with another content ID (a DLC read as a
 *  game) is never offered for the Trash (#638). A copy with no content ID (an archive) is
 *  counted with the largest release, as the engine does. */
export function extraCopies(
  g: Pick<CollectionGame, "locations">,
): CollectionLocation[] {
  const sets = new Map<string, CollectionLocation[]>();
  const unknown: CollectionLocation[] = [];
  for (const l of g.locations) {
    if (l.pkg?.kind === "patch" || l.pkg?.kind === "dlc") continue;
    const cid = (l.pkg?.content_id ?? "").trim().toUpperCase();
    if (!cid) unknown.push(l);
    else sets.set(cid, [...(sets.get(cid) ?? []), l]);
  }
  const groups = [...sets.entries()]
    .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
    .map(([, ls]) => ls);
  if (unknown.length) {
    let largest = -1;
    groups.forEach((ls, i) => {
      if (largest < 0 || ls.length > groups[largest].length) largest = i;
    });
    if (largest < 0) groups.push(unknown);
    else groups[largest] = groups[largest].concat(unknown);
  }
  return groups.flatMap((ls) =>
    [...ls].sort((a, b) => b.size_bytes - a.size_bytes).slice(1),
  );
}

/** `1.5 GB`, `0 B`: the same as the engine's exports. */
export function formatCollectionBytes(n: number): string {
  if (!n || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${parseFloat(v.toFixed(2))} ${units[i]}`;
}

/** What a location is, in words, for a row: "Package · Patch 1.09", "Game image (.ffpkg)". */
export function locationKind(type: string): string {
  switch (type) {
    case "pkg":
      return "Package";
    case "folder":
      return "Game folder";
    case "rar":
      return "RAR archive";
    case "7z":
      return "7z archive";
    case "zip":
      return "ZIP archive";
    default:
      return type.startsWith("mount.")
        ? `Game image (.${type.slice(6)})`
        : type;
  }
}
