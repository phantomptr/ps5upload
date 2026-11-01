import type { CollectionGame, GameConsoleState } from "../../api/collection";

/** How many games the Home row holds at most; the rest are a click away in the Collection. */
export const RECENT_GAMES_LIMIT = 12;

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
  limit = RECENT_GAMES_LIMIT,
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

/**
 * Whether a sideways-scrolling row has more before and after what shows.
 * `scrollLeft` is negative in a right-to-left layout (browsers count from the
 * start edge towards negative), so the distance from the start is its
 * magnitude. A pixel of slack absorbs fractional scroll positions on scaled
 * displays.
 */
export function rowEdges(scrollLeft: number, scrollWidth: number, clientWidth: number) {
  const from = Math.abs(scrollLeft);
  return { prev: from > 1, next: from + clientWidth < scrollWidth - 1 };
}
