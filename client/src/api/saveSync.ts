// Save sync: one user's saves kept in step across two consoles (engine save_sync.rs). The engine
// decides what changed by fingerprints kept at the last copy, never by dates, each copy through
// Copy to another console (the target backed up first). A save the user turned on is kept in step
// by the engine itself (save_sync_auto.rs); the rest are copied only when asked.

import { getEngineUrl } from "../state/engine";
import { ENGINE_LONG_TIMEOUT_MS, engineFetch, type EngineFetchInit } from "../lib/engineFetch";

export interface SyncSide {
  host: string;
  user_id: number;
}

export interface SyncPair {
  id: string;
  a: SyncSide;
  b: SyncSide;
  /** Saves left out, by key. */
  excluded: string[];
  /** Saves kept in step automatically, by key. */
  auto?: string[];
  /** A console in rest is woken to take a waiting copy. */
  wake?: boolean;
}

export type AutoState = "in_step" | "waiting" | "conflict" | "cant_sync";

export type AutoReason =
  | "offline"
  | "settling"
  | "running"
  | "queued"
  | "error"
  | "ps4"
  | "no_save"
  | "refused"
  | "failed";

/** Where a save kept in step stands (engine save_sync_auto.rs AutoView). */
export interface AutoView {
  state: AutoState;
  title_id: string;
  kind: string;
  /** The console the state is about: the one waited for, or the one the reason names. */
  side: "a" | "b" | null;
  reason: AutoReason | null;
  detail: string | null;
  direction: SyncDirection | null;
  /** Conflict: never synced before. */
  first: boolean;
  /** Conflict: the backups taken of both sides. */
  kept: string[];
  since_ms: number;
  fails: number;
  woke_ms: number | null;
  copied_ms: number | null;
  copied_direction: SyncDirection | null;
}

/** One save kept in step (GET /api/save-sync/games). */
export interface SyncGame extends AutoView {
  pair: string;
  key: string;
  a: SyncSide;
  b: SyncSide;
  synced_ms: number | null;
}

export type SyncStatus =
  | "in_step"
  | "a_newer"
  | "b_newer"
  | "both_changed"
  | "first_time"
  | "only_on_a"
  | "only_on_b"
  | "unavailable"
  | "excluded";

export type SyncReason = "offline" | "settling" | "running" | "ps4" | "error";

export type SyncDirection = "a_to_b" | "b_to_a";

export interface SyncSideView {
  path: string;
  size: number;
  /** When the engine first saw this side differ (its clock: approximate). */
  changed_ms: number | null;
}

export interface SyncRow {
  key: string;
  title_id: string;
  kind: string;
  status: SyncStatus;
  reason: SyncReason | null;
  reason_side: "a" | "b" | null;
  a: SyncSideView | null;
  b: SyncSideView | null;
  synced_ms: number | null;
  last_error: string | null;
  /** Kept in step automatically: where it stood at this check. */
  auto?: AutoView | null;
}

export interface SyncSuggestion {
  pair: string;
  key: string;
  title_id: string;
  kind: string;
  direction: SyncDirection;
  state: "ready" | "waiting" | "conflict";
  at_ms: number;
}

export interface SyncView {
  pairs: SyncPair[];
  suggestions: SyncSuggestion[];
  /** Console pairs with a copy running now. */
  applying: string[];
}

export interface SyncApplyResult {
  key: string;
  ok: boolean;
  error?: string;
}

/** A view whatever the engine answered: an engine without save sync (older, or one the browser
 *  build talks to) or an odd answer is "nothing set up", never a crash of the screen that asked. */
export function syncView(raw: unknown): SyncView {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const list = <T,>(v: unknown): T[] => (Array.isArray(v) ? (v as T[]) : []);
  return {
    pairs: list<SyncPair>(o.pairs),
    suggestions: list<SyncSuggestion>(o.suggestions),
    applying: list<string>(o.applying),
  };
}

async function call<T>(path: string, init?: EngineFetchInit): Promise<T> {
  const res = await engineFetch(`${getEngineUrl()}/api/save-sync${path}`, {
    ...init,
    headers: init?.body ? { "content-type": "application/json" } : undefined,
  });
  const text = await res.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!res.ok) throw new Error((body as { error?: string } | null)?.error ?? `HTTP ${res.status}`);
  return body as T;
}

export const saveSyncApi = {
  view: async () => syncView(await call<unknown>("")),
  setPair: (a: SyncSide, b: SyncSide, excluded?: string[], wake?: boolean) =>
    call<{ pair: SyncPair }>("/pair", {
      method: "PUT",
      body: JSON.stringify({
        a: { host: a.host, user_id: a.user_id >>> 0 },
        b: { host: b.host, user_id: b.user_id >>> 0 },
        excluded,
        wake,
      }),
    }),
  /** Every save kept in step and where it stands (reads no console). */
  games: async () => {
    const r = await call<{ games?: unknown }>("/games");
    return Array.isArray(r?.games) ? (r.games as SyncGame[]) : [];
  },
  /** Keep one save in step automatically, or stop. Turning it on checks it first (both consoles
   *  read, nothing copied) and is refused with the reason when it can't be done safely. */
  setAuto: (pair: string, key: string, on: boolean) =>
    call<{ ok: boolean; auto: AutoView | null }>("/auto", {
      method: "PUT",
      timeoutMs: ENGINE_LONG_TIMEOUT_MS,
      body: JSON.stringify({ pair, key, on }),
    }),
  /** Does now what the background look would for one save kept in step. */
  syncNow: (pair: string, key: string) =>
    call<{ ok: boolean; auto: AutoView }>("/sync-now", {
      method: "POST",
      timeoutMs: ENGINE_LONG_TIMEOUT_MS,
      body: JSON.stringify({ pair, key }),
    }),
  /** The pick in a conflict: `keep`'s save is copied over the other (backed up first). */
  resolve: (pair: string, key: string, keep: "a" | "b") =>
    call<{ ok: boolean; auto: AutoView | null }>("/resolve", {
      method: "POST",
      timeoutMs: ENGINE_LONG_TIMEOUT_MS,
      body: JSON.stringify({ pair, key, keep }),
    }),
  removePair: (pair: string) =>
    call<{ ok: boolean }>("/pair/remove", { method: "POST", body: JSON.stringify({ pair }) }),
  /** Check now: reads both consoles (wakes neither). */
  status: async (pair: string) => {
    const r = await call<{ pair: SyncPair; rows?: unknown; checked_ms: number }>(
      `/status?pair=${encodeURIComponent(pair)}`,
      { timeoutMs: ENGINE_LONG_TIMEOUT_MS },
    );
    return { ...r, rows: Array.isArray(r?.rows) ? (r.rows as SyncRow[]) : [] };
  },
  /** Copies the picked saves, one at a time. */
  apply: (pair: string, items: { key: string; direction: SyncDirection }[]) =>
    call<{ results: SyncApplyResult[] }>("/apply", {
      method: "POST",
      timeoutMs: ENGINE_LONG_TIMEOUT_MS,
      body: JSON.stringify({ pair, items }),
    }),
  /** Not now: the suggestion goes until the game closes with a change again. */
  dismiss: (pair: string, key: string) =>
    call<{ ok: boolean }>("/dismiss", { method: "POST", body: JSON.stringify({ pair, key }) }),
};
