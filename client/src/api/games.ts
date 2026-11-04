// One game across every saved console and every copy on this computer's drives
// (engine/crates/ps5upload-engine/src/games_api.rs): what the game page shows.

import { getEngineUrl } from "../state/engine";
import type { CollectionLocation, CollectionOffer } from "./collection";
import { engineFetch } from "../lib/engineFetch";

/** One console's side of the game, as last read. */
export interface ConsoleEntry {
  /** The console's IP, no port. */
  host: string;
  /** Unix seconds of the read. */
  read_at: number;
  installed: boolean;
  version?: string | null;
  registered_from?: string | null;
  /** The game's package, when the console does not have it. */
  base?: CollectionOffer | null;
  /** A newer update than the console has. */
  update?: CollectionOffer | null;
  dlc_missing: CollectionOffer[];
}

export interface GameView {
  title_id: string;
  title: string;
  platform: string;
  /** An engine path (`/api/collection/…`) or a full URL. */
  cover: string | null;
  copies: CollectionLocation[];
  consoles: ConsoleEntry[];
}

async function call<T>(path: string, init?: RequestInit): Promise<{ status: number; body: T | null }> {
  const res = await engineFetch(`${getEngineUrl()}/api${path}`, {
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
  if (!res.ok && res.status !== 404) {
    throw new Error((body as { error?: string } | null)?.error ?? `HTTP ${res.status}`);
  }
  return { status: res.status, body: body as T | null };
}

/** A title some saved console was last read to have installed (`GET /api/games/installed`). */
export interface InstalledGame {
  title_id: string;
  /** "" when neither the console nor the Collection named it. */
  title: string;
  /** Bare IPs of the consoles it is installed on. */
  hosts: string[];
}

export const gamesApi = {
  /** Every game the saved consoles were last read to have installed (no console is asked now). */
  installed: async (): Promise<InstalledGame[]> => {
    const r = await call<InstalledGame[]>(`/games/installed`);
    return r.body ?? [];
  },
  /** The game, or null when neither the Collection nor any console read so far knows it. */
  view: async (titleId: string): Promise<GameView | null> => {
    const r = await call<GameView>(`/games/${encodeURIComponent(titleId)}`);
    return r.status === 404 ? null : r.body;
  },
  /** Reads this one title on the console at `addr` (IP:mgmt port) now. */
  refresh: async (titleId: string, addr: string): Promise<ConsoleEntry> => {
    const r = await call<ConsoleEntry>(
      `/games/${encodeURIComponent(titleId)}/refresh?addr=${encodeURIComponent(addr)}`,
      { method: "POST" },
    );
    if (!r.body) throw new Error(`HTTP ${r.status}`);
    return r.body;
  },
  /** Forgets saved state of every console not in `hosts`. */
  /** Forget these consoles (removed here, or at a new IP); other clients' consoles stay. */
  forget: async (hosts: string[]): Promise<void> => {
    await call(`/console-snapshots/forget`, { method: "POST", body: JSON.stringify({ hosts }) });
  },
};
