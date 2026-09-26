import { processList } from "../api/ps5";

/** A game/app currently running on the console, distilled from the process
 *  list down to what the Installed Apps screen needs to show "Playing" and to
 *  stop it. */
export interface RunningGame {
  titleId: string;
  /** Sony app id — preferred handle for a clean stop via appKill(). 0 when the
   *  process list didn't carry one (fall back to the pid). */
  appId: number;
  /** A pid belonging to the title — the processKill() fallback when there's no
   *  app id. */
  pid: number;
}

/**
 * Map `title_id` → running game, from the console's process list.
 *
 * A running game spawns many threads/processes that all share one title_id;
 * we collapse them to a single entry per title, preferring a process that
 * carries a real Sony app id (so a later appKill() has a clean handle). Only
 * actual app/game processes count — never the PS5Upload helper itself
 * (`is_self`), the payload, or system processes — so the running indicator and
 * the Stop button can't target our own connection or the OS.
 *
 * Read-only: this never touches a running game; detecting one must not risk
 * disturbing a title that's still coming up.
 */
export async function fetchRunningGames(
  mgmtAddr: string,
): Promise<Map<string, RunningGame>> {
  const res = await processList(mgmtAddr);
  const byTitle = new Map<string, RunningGame>();
  for (const p of res.processes) {
    if (p.kind !== "app" || !p.title_id || p.is_self) continue;
    const existing = byTitle.get(p.title_id);
    if (!existing) {
      byTitle.set(p.title_id, {
        titleId: p.title_id,
        appId: p.app_id || 0,
        pid: p.pid,
      });
    } else if (!existing.appId && p.app_id) {
      // Upgrade to a process that has a real app id for a cleaner kill.
      existing.appId = p.app_id;
      existing.pid = p.pid;
    }
  }
  return byTitle;
}

/**
 * Move running titles to the front, leaving everything else in order.
 *
 * A running title is the one row on the Games grid with a live,
 * time-sensitive action attached (Close game), and hunting for it among two
 * hundred covers is the opposite of what the user needs at that moment.
 *
 * Applied AFTER any other ordering so it wins rather than competes with it,
 * and it relies on `sort` being stable (guaranteed since ES2019) so the
 * relative order of everything else — including the play-time sort — is
 * preserved exactly.
 *
 * Returns the input array untouched when nothing is running, so the common
 * case does not allocate.
 */
export function sortRunningFirst<T extends { titleId: string }>(
  rows: readonly T[],
  running: ReadonlySet<string> | ReadonlyMap<string, unknown>,
): readonly T[] {
  const has = (id: string) =>
    running instanceof Map ? running.has(id) : (running as Set<string>).has(id);
  if (!rows.some((r) => has(r.titleId))) return rows;
  return [...rows].sort(
    (a, b) => Number(has(b.titleId)) - Number(has(a.titleId)),
  );
}
