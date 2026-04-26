// Thin HTTP wrappers for the ps5upload-engine sidecar at :19113.
//
// Only covers endpoints the engine ACTUALLY implements today (see
// `engine/crates/ps5upload-engine/src/main.rs`). Endpoints that would
// require payload-side changes (installed-apps listing, PS5-side file
// search, hot-plug refresh) are roadmap items #4, #7, #9 and aren't
// represented here — they'll show up when the payload contract lands.
//
// For desktop use, prefer `api/ps5.ts` (Tauri IPC) over this module.
// The HTTP surface is kept as a stable contract for scripts, CI tests,
// and any future non-desktop consumer.

const BASE = "http://127.0.0.1:19113";

export interface Volume {
  path: string;
  mount_from?: string;
  fs_type: string;
  total_bytes: number;
  free_bytes: number;
  writable: boolean;
  is_placeholder?: boolean;
  source_image?: string;
}

/**
 * Per-request timeout in milliseconds. Picked to be larger than the
 * engine's own internal PS5-side RPC timeouts (30s default) so the
 * server gets a chance to time out and surface a structured error
 * before the client gives up. Without this cap, a hung PS5 + engine
 * mid-call could leave the client polling forever — the status bar
 * dot would stay grey, refresh/poll loops would pile up unfinished
 * fetches, and the user has no signal that something is wrong.
 */
const REQUEST_TIMEOUT_MS = 35_000;

async function json<T>(path: string, init?: RequestInit): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
  try {
    const res = await fetch(`${BASE}${path}`, {
      ...init,
      signal: controller.signal,
    });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    return (await res.json()) as T;
  } finally {
    clearTimeout(timer);
  }
}

export const engineApi = {
  /** Lightweight engine liveness probe. Unlike `/api/ps5/status` (which
   *  round-trips to the PS5 and fails if the console is off), this hits
   *  `/api/jobs` — served from in-memory state, responds immediately,
   *  touches no network. Used by the status bar's engine dot. */
  async ping(): Promise<boolean> {
    try {
      const res = await fetch(`${BASE}/api/jobs`);
      return res.ok;
    } catch {
      return false;
    }
  },

  /** PS5-side status (requires engine → PS5 reachability). */
  status: () => json<{ ok: boolean; version: string }>("/api/ps5/status"),

  /** PS5 volumes — a fresh `getmntinfo(MNT_WAIT)` on every call, so the
   *  client polls this on a timer to pick up hot-plug events. No
   *  separate "refresh" endpoint needed. */
  volumes: () => json<{ volumes: Volume[] }>("/api/ps5/volumes"),
};
