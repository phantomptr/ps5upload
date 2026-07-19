import { create } from "zustand";
import { isTauriEnv } from "../lib/tauriEnv";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Base URL of the ps5upload-engine the UI talks to. Default is the
 * bundled local sidecar; a user can point it at a remote/self-hosted
 * engine (e.g. the Docker image) from Settings. Persisted to
 * localStorage and mirrored to ~/.ps5upload/settings.json.
 *
 * In a browser (non-Tauri) environment the UI is served *by* the engine
 * itself, so the base URL is simply the page origin — this makes every
 * /api/... request same-origin and avoids all CORS concerns.
 */

export const DEFAULT_ENGINE_URL = "http://127.0.0.1:19113";
const KEY_ENGINE_URL = "ps5upload.engine_url";

/** Trim and drop any trailing slash so callers can `${base}/api/...`. */
export function normalizeEngineUrl(raw: string): string {
  const v = raw.trim().replace(/\/+$/, "");
  return v || DEFAULT_ENGINE_URL;
}

function loadEngineUrl(): string {
  if (typeof window === "undefined") return DEFAULT_ENGINE_URL;
  // In the browser build the UI is served from the engine origin, so use
  // window.location.origin rather than a persisted 127.0.0.1 address.
  if (!isTauriEnv()) return window.location.origin;
  const v = safeGetItem(KEY_ENGINE_URL);
  return v ? normalizeEngineUrl(v) : DEFAULT_ENGINE_URL;
}

interface EngineState {
  engineUrl: string;
  setEngineUrl: (url: string) => void;
}

export const useEngineStore = create<EngineState>((set) => ({
  engineUrl: loadEngineUrl(),
  setEngineUrl: (url) => {
    const engineUrl = normalizeEngineUrl(url);
    safeSetItem(KEY_ENGINE_URL, engineUrl);
    set({ engineUrl });
    // A deliberate user/settings change of the engine URL supersedes any
    // transient sidecar-fallback override (see setLiveEngineUrl).
    liveEngineUrl = null;
  },
}));

/**
 * Transient, NON-persisted override for the live engine location.
 *
 * The store's `engineUrl` is the user's *preference* (default 19113 or a
 * remote engine), persisted to localStorage and mirrored to settings.json.
 * But on desktop the sidecar can land on a *different* loopback port when
 * 19113 is occupied — the Rust shell reports where it actually bound via
 * the `ps5upload-engine-ready` event / `engine_url_get`. We stash that
 * here so the renderer's DIRECT fetches (job polling, cover-art img-src,
 * streaming) hit the real port, WITHOUT touching the persisted preference
 * (so the next launch still re-tries 19113 first) and WITHOUT the
 * settings-mirror clobbering it. Cleared when the user changes the URL.
 */
let liveEngineUrl: string | null = null;

/** Follow the sidecar to wherever it actually bound (fallback port). */
export function setLiveEngineUrl(url: string): void {
  liveEngineUrl = normalizeEngineUrl(url);
}

/** Non-hook accessor for module-scope callers (api/engine.ts, api/ps5.ts).
 *  The live override wins when present so direct fetches follow a
 *  fallback port; otherwise the persisted preference is used. */
export function getEngineUrl(): string {
  return liveEngineUrl ?? useEngineStore.getState().engineUrl;
}
