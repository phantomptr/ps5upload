import { create } from "zustand";

/**
 * Base URL of the ps5upload-engine the UI talks to. Default is the
 * bundled local sidecar; a user can point it at a remote/self-hosted
 * engine (e.g. the Docker image) from Settings. Persisted to
 * localStorage and mirrored to ~/.ps5upload/settings.json.
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
  const v = window.localStorage.getItem(KEY_ENGINE_URL);
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
    window.localStorage.setItem(KEY_ENGINE_URL, engineUrl);
    set({ engineUrl });
  },
}));

/** Non-hook accessor for module-scope callers (api/engine.ts, api/ps5.ts). */
export function getEngineUrl(): string {
  return useEngineStore.getState().engineUrl;
}
