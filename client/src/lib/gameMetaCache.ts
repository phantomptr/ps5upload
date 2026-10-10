import type { GameMeta } from "../api/ps5";
import { hostOf } from "./addr";

/**
 * Short-lived memory of game-folder metadata (param.json), keyed by console +
 * path.
 *
 * Every Game files row reads its own param.json when it mounts, and the rows
 * remount every time the screen is shown again, so switching tabs used to
 * re-read up to a hundred files from a console that serves one request at a
 * time. A folder's title and id don't change between two visits a minute
 * apart; a rescan after the TTL still picks up a replaced game.
 */

export const GAME_META_TTL_MS = 2 * 60_000;

interface Entry {
  at: number;
  meta: GameMeta;
}

const done = new Map<string, Entry>();
const pending = new Map<string, Promise<GameMeta>>();

const keyOf = (host: string, path: string) => `${hostOf(host.trim())}|${path}`;

/** A blank answer is how fetchGameMeta reports a failed read; not worth keeping. */
const isBlank = (m: GameMeta) => !m.title && !m.title_id && !m.content_id;

/** The cached metadata, when still fresh. */
export function peekGameMeta(host: string, path: string, now = Date.now()): GameMeta | null {
  const hit = done.get(keyOf(host, path));
  if (!hit) return null;
  if (now - hit.at > GAME_META_TTL_MS) {
    done.delete(keyOf(host, path));
    return null;
  }
  return hit.meta;
}

/** Cached metadata, or one shared read through `load` (concurrent callers for
 *  the same folder wait on the same request). */
export function cachedGameMeta(
  host: string,
  path: string,
  load: () => Promise<GameMeta>,
): Promise<GameMeta> {
  const fresh = peekGameMeta(host, path);
  if (fresh) return Promise.resolve(fresh);
  const key = keyOf(host, path);
  const inFlight = pending.get(key);
  if (inFlight) return inFlight;
  const p = load()
    .then((meta) => {
      if (!isBlank(meta)) done.set(key, { at: Date.now(), meta });
      return meta;
    })
    .finally(() => pending.delete(key));
  pending.set(key, p);
  return p;
}

/** Forget one folder (it was moved, deleted or replaced) or everything. */
export function forgetGameMeta(host?: string, path?: string): void {
  if (host && path) {
    done.delete(keyOf(host, path));
    return;
  }
  done.clear();
}
