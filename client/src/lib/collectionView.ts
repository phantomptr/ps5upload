// The Collection screen's filtering, searching and sorting: pure, so it is tested without a
// screen. Filters and sorts are PS Game Library's, plus platform.

import type {
  CollectionGame,
  CollectionLocation,
  GameConsoleState,
} from "../api/collection";

export type CollectionFilter =
  "all" | "duplicates" | "pkg" | "mount" | "folder" | "rar" | "7z" | "zip";

export const COLLECTION_FILTERS: CollectionFilter[] = [
  "all",
  "duplicates",
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
  | "locations-desc";

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
];

/** What a game is to the selected console. */
export type ConsoleFilter = "any" | "missing" | "update" | "dlc";

export interface CollectionQuery {
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
): boolean {
  if (!f || f === "any" || !states) return true;
  const st = states.get(g.game_id);
  if (!st) return false;
  if (f === "missing") return !st.installed;
  if (f === "update") return !!st.installed && !!st.update;
  return st.installed && st.dlc_missing.length > 0;
}

export function consoleCounts(
  games: CollectionGame[],
  states: Map<string, GameConsoleState> | undefined,
): Record<Exclude<ConsoleFilter, "any">, number> {
  const out = { missing: 0, update: 0, dlc: 0 };
  if (!states) return out;
  for (const g of games) {
    if (matchesConsole(g, "missing", states)) out.missing += 1;
    if (matchesConsole(g, "update", states)) out.update += 1;
    if (matchesConsole(g, "dlc", states)) out.dlc += 1;
  }
  return out;
}

function matchesFilter(g: CollectionGame, f: CollectionFilter): boolean {
  if (f === "all") return true;
  if (f === "duplicates") return g.is_duplicate;
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
): number {
  const byTitle = a.title.localeCompare(b.title, undefined, {
    sensitivity: "base",
  });
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
  }
}

export function viewGames(
  games: CollectionGame[],
  q: CollectionQuery,
): CollectionGame[] {
  return games
    .filter(
      (g) =>
        matchesFilter(g, q.filter) &&
        matchesPlatform(g, q.platform) &&
        matchesConsole(g, q.console, q.consoleStates) &&
        matchesSearch(g, q.search),
    )
    .sort((a, b) => compare(a, b, q.sort));
}

/** How many of each filter's games there are, for the chips. */
export function filterCounts(
  games: CollectionGame[],
): Record<CollectionFilter, number> {
  const out = Object.fromEntries(
    COLLECTION_FILTERS.map((f) => [f, 0]),
  ) as Record<CollectionFilter, number>;
  for (const g of games)
    for (const f of COLLECTION_FILTERS) if (matchesFilter(g, f)) out[f] += 1;
  return out;
}

/** Add-ons a game has: its patches and DLC. */
export function addOnCount(g: CollectionGame): number {
  return g.locations.filter(
    (l) => l.pkg?.kind === "patch" || l.pkg?.kind === "dlc",
  ).length;
}

/** A game's full copies beyond its largest: what "Reclaimable" counts (never an add-on). */
export function extraCopies(g: CollectionGame): CollectionLocation[] {
  const copies = g.locations
    .filter((l) => l.pkg?.kind !== "patch" && l.pkg?.kind !== "dlc")
    .sort((a, b) => b.size_bytes - a.size_bytes);
  return copies.slice(1);
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
