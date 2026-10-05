/** The Cheats screen's game list, built from three sources.
 *
 *  A player thinks "my game → its cheats", not "a cheat file → a title id".
 *  So the list is the games on the console, each annotated with what the
 *  cheat collection has for it and what is already on the console — plus the
 *  odd cheat downloaded for a game that is not installed here. */

import type { CheatRepoEntry, CheatTitle, InstalledTitle } from "../api/ps5";
import { isSupportedCheatFormat, isUsableGameTitle } from "./cheatBrowse";

export interface CheatGame {
  titleId: string;
  name: string;
  installed: boolean;
  /** A cheat file for it is already on the console. */
  downloaded: boolean;
  /** Version the downloaded cheat targets, when its filename says. */
  downloadedVersion: string;
  /** Formats of the cheat files on the console (json / shn / mc4). */
  downloadedFormats: string[];
  /** Cheats currently switched on for it. */
  enabledCount: number;
  /** Cheat files in the collection the console can read. */
  available: CheatRepoEntry[];
  running: boolean;
}

export type CheatSectionKey = "playing" | "ready" | "available" | "none";

export interface CheatSection {
  key: CheatSectionKey;
  games: CheatGame[];
}

/** Version strings compared by their numbers, so `01.000.016` equals
 *  `1.0.16` and `01.05` sorts below `01.10`. Non-numeric parts compare as 0. */
export function versionParts(v: string | null | undefined): number[] {
  return (v ?? "")
    .trim()
    .split(".")
    .filter((p) => p.length > 0)
    .map((p) => {
      const n = parseInt(p, 10);
      return Number.isFinite(n) ? n : 0;
    });
}

export function compareVersions(a: string, b: string): number {
  const x = versionParts(a);
  const y = versionParts(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

export function sameVersion(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  if (versionParts(a).length === 0 || versionParts(b).length === 0) return false;
  return compareVersions(a, b) === 0;
}

export function buildCheatGames(input: {
  installed: InstalledTitle[];
  downloaded: CheatTitle[];
  index: CheatRepoEntry[];
  runningTitleId?: string | null;
  repoNames?: Map<string, string>;
}): CheatGame[] {
  const byId = new Map<string, CheatGame>();
  const running = (input.runningTitleId ?? "").toUpperCase();
  const available = new Map<string, CheatRepoEntry[]>();
  for (const e of input.index) {
    const id = (e.title_id ?? "").toUpperCase();
    if (!id || !isSupportedCheatFormat(e.format)) continue;
    const list = available.get(id) ?? [];
    list.push(e);
    available.set(id, list);
  }
  const game = (id: string, name: string, installed: boolean): CheatGame => ({
    titleId: id,
    name,
    installed,
    downloaded: false,
    downloadedVersion: "",
    downloadedFormats: [],
    enabledCount: 0,
    available: available.get(id) ?? [],
    running: id === running,
  });
  for (const t of input.installed) {
    const id = (t.titleId ?? "").toUpperCase();
    if (!id || t.system || byId.has(id)) continue;
    byId.set(id, game(id, t.titleName || id, true));
  }
  for (const d of input.downloaded) {
    const id = (d.title_id ?? "").toUpperCase();
    if (!id) continue;
    let g = byId.get(id);
    if (!g) {
      const name = isUsableGameTitle(d.name)
        ? d.name
        : input.repoNames?.get(id) ?? id;
      g = game(id, name, false);
      byId.set(id, g);
    }
    g.downloaded = true;
    // An installed game whose name the console didn't give us can still be
    // named by the cheat file's own game name.
    if (g.name === id && isUsableGameTitle(d.name)) g.name = d.name;
    g.downloadedVersion = d.version ?? "";
    g.downloadedFormats = (d.formats ?? []).map((f) => f.toLowerCase());
    g.enabledCount = d.enabled ?? 0;
    if (d.running) g.running = true;
  }
  if (running && !byId.has(running)) {
    byId.set(running, game(running, input.repoNames?.get(running) ?? running, true));
  }
  return [...byId.values()];
}

/** Grouped the way a player looks for them: the game on screen right now,
 *  games with cheats ready to switch on, games with cheats to download, and
 *  the rest. Alphabetical inside each; empty sections are left out. */
export function cheatSections(games: CheatGame[]): CheatSection[] {
  const byName = (a: CheatGame, b: CheatGame) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  const pick = (f: (g: CheatGame) => boolean) => games.filter(f).sort(byName);
  const sections: CheatSection[] = [
    { key: "playing", games: pick((g) => g.running) },
    { key: "ready", games: pick((g) => !g.running && g.downloaded) },
    {
      key: "available",
      games: pick((g) => !g.running && !g.downloaded && g.available.length > 0),
    },
    {
      key: "none",
      games: pick((g) => !g.running && !g.downloaded && g.available.length === 0),
    },
  ];
  return sections.filter((s) => s.games.length > 0);
}

/** A game's downloadable cheats, best first: the ones made for the version
 *  installed here, then the rest newest first. Cheats patch memory at fixed
 *  addresses, so a cheat for another version usually does nothing — or
 *  crashes the game — which is why the match leads. */
export function rankCheatFiles(
  entries: CheatRepoEntry[],
  installedVersion: string | null | undefined,
): CheatRepoEntry[] {
  return [...entries].sort((a, b) => {
    const am = sameVersion(a.game_version, installedVersion) ? 0 : 1;
    const bm = sameVersion(b.game_version, installedVersion) ? 0 : 1;
    if (am !== bm) return am - bm;
    const v = compareVersions(b.game_version ?? "", a.game_version ?? "");
    if (v !== 0) return v;
    return a.filename.localeCompare(b.filename);
  });
}

/** Whether the collection has a cheat for exactly this version. */
export function hasVersionMatch(
  entries: CheatRepoEntry[],
  installedVersion: string | null | undefined,
): boolean {
  return entries.some((e) => sameVersion(e.game_version, installedVersion));
}

/** Search the list by name or title id. */
export function filterCheatGames(games: CheatGame[], query: string): CheatGame[] {
  const q = query.trim().toLowerCase();
  if (!q) return games;
  return games.filter(
    (g) => g.name.toLowerCase().includes(q) || g.titleId.toLowerCase().includes(q),
  );
}

export type CheatScopeFilter = "all" | "installed" | "withCheats";
export type CheatStateFilter = "all" | "playing" | "switchedOn" | "ready" | "toDownload";

export interface CheatListFilters {
  query: string;
  /** "" for any, else json | shn | mc4. */
  format: string;
  state: CheatStateFilter;
  scope: CheatScopeFilter;
}

export const EMPTY_CHEAT_FILTERS: CheatListFilters = {
  query: "",
  format: "",
  state: "all",
  scope: "all",
};

export function cheatFiltersActive(f: CheatListFilters): boolean {
  return !!f.query.trim() || !!f.format || f.state !== "all" || f.scope !== "all";
}

/** Formats a game has cheats in, on the console or in the collection. */
export function cheatGameFormats(g: CheatGame): string[] {
  const set = new Set<string>(g.downloadedFormats);
  for (const e of g.available) if (e.format) set.add(e.format.toLowerCase());
  return [...set].sort();
}

/** The search box plus the three dropdown filters, all ANDed. */
export function applyCheatFilters(games: CheatGame[], f: CheatListFilters): CheatGame[] {
  return filterCheatGames(games, f.query).filter((g) => {
    if (f.scope === "installed" && !g.installed) return false;
    if (f.scope === "withCheats" && !g.downloaded && g.available.length === 0) return false;
    if (f.format && !cheatGameFormats(g).includes(f.format.toLowerCase())) return false;
    switch (f.state) {
      case "playing":
        return g.running;
      case "switchedOn":
        return g.enabledCount > 0;
      case "ready":
        return g.downloaded;
      case "toDownload":
        return !g.downloaded && g.available.length > 0;
      default:
        return true;
    }
  });
}
