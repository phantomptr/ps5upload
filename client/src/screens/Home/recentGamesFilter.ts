import type { CollectionGame, GameConsoleState } from "../../api/collection";

/** "all", "missing" (on none of the consoles read so far), or a console's host. */
export type RecentFilter = "all" | "missing" | { host: string };

/** What one console has, by game id (the Collection's per-console read). */
export type StatesByHost = Record<string, Record<string, GameConsoleState> | undefined>;

/**
 * The Home "Recent games" row: the Collection's newest games, optionally
 * narrowed to the ones a console has, or the ones none of the read consoles
 * has. Only consoles that were actually read count; one never read this
 * session can't say a game is missing.
 */
export function recentGames(
  games: readonly CollectionGame[],
  filter: RecentFilter,
  states: StatesByHost,
  limit = 8,
): CollectionGame[] {
  const read = Object.values(states).filter((s): s is Record<string, GameConsoleState> => !!s);
  const keep = (g: CollectionGame) => {
    if (filter === "all") return true;
    if (filter === "missing") return read.length > 0 && read.every((s) => !s[g.game_id]?.installed);
    return !!states[filter.host]?.[g.game_id]?.installed;
  };
  return [...games]
    .filter(keep)
    .sort((a, b) => b.added_ts - a.added_ts || a.title.localeCompare(b.title))
    .slice(0, limit);
}
