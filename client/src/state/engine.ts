import { create } from "zustand";
import { isMobile } from "../lib/platform";
import { pageBasePath } from "../lib/basePath";
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

const DEFAULT_ENGINE_PORT = "19113";

/** Trim and drop any trailing slash so callers can `${base}/api/...`. A bare address gets what it
 *  left out: `192.168.1.10` is `http://192.168.1.10:19113`, and `nas.local:8080` is
 *  `http://nas.local:8080`. A full URL says what it means, so `https://ps5.home` keeps its port. */
export function normalizeEngineUrl(raw: string): string {
  const v = raw.trim().replace(/\/+$/, "");
  if (!v) return DEFAULT_ENGINE_URL;
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(v)) return v;
  // No port after the name, whether or not there is a path after it.
  const hostEnd = v.search(/[/?#]/);
  const authority = hostEnd < 0 ? v : v.slice(0, hostEnd);
  // A bare IPv6 address has several colons and no brackets; its last group is not a port.
  const bareV6 = !authority.startsWith("[") && authority.split(":").length > 2;
  const hasPort = !bareV6 && /:\d+$/.test(authority);
  const bracketed = bareV6 ? `[${authority}]` : authority;
  const rest = hostEnd < 0 ? "" : v.slice(hostEnd);
  return `http://${bracketed}${hasPort ? "" : `:${DEFAULT_ENGINE_PORT}`}${rest}`;
}

/** Whether a typed Engine URL can be used: an http(s) address with a host. */
export function engineUrlIsValid(raw: string): boolean {
  try {
    const u = new URL(normalizeEngineUrl(raw));
    return (u.protocol === "http:" || u.protocol === "https:") && !!u.hostname;
  } catch {
    return false;
  }
}

function loadEngineUrl(): string {
  if (typeof window === "undefined") return DEFAULT_ENGINE_URL;
  // In the browser build the UI is served from the engine origin, so use
  // window.location.origin rather than a persisted 127.0.0.1 address.
  if (!isTauriEnv()) return window.location.origin + pageBasePath();
  const v = safeGetItem(KEY_ENGINE_URL);
  return v ? usableEngineUrl(normalizeEngineUrl(v)) : DEFAULT_ENGINE_URL;
}

/** The phone apps have no Engine URL setting: their native commands always reach the engine
 *  inside the app, and Android allows plain http to loopback only. A remote URL saved before the
 *  setting was hidden (or restored from a backup) would point the page's direct fetches at
 *  another machine while the native calls stay here, with no field left to clear it. */
function usableEngineUrl(url: string): string {
  return isTauriEnv() && isMobile() && !isLoopbackUrl(url) ? DEFAULT_ENGINE_URL : url;
}

interface EngineState {
  engineUrl: string;
  setEngineUrl: (url: string) => void;
  /** Where this device's own engine really is, when the shell has said: differs from
   *  `engineUrl` while it runs somewhere else until its next start (the port was taken, or the
   *  setting was changed). */
  runningAt: string | null;
}

export const useEngineStore = create<EngineState>((set) => ({
  engineUrl: loadEngineUrl(),
  runningAt: null,
  setEngineUrl: (url) => {
    const engineUrl = usableEngineUrl(normalizeEngineUrl(url));
    // Clicking into the field and out again is not a change, and must not drop the override below.
    if (engineUrl === useEngineStore.getState().engineUrl) return;
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
  if (isLoopbackUrl(liveEngineUrl) && useEngineStore.getState().runningAt !== liveEngineUrl) {
    useEngineStore.setState({ runningAt: liveEngineUrl });
  }
}

/** The local engine runs somewhere other than the setting says, which a restart may change. */
export function engineRunsElsewhere(s: Pick<EngineState, "engineUrl" | "runningAt">): boolean {
  return !!s.runningAt && isLoopbackUrl(s.engineUrl) && s.runningAt !== s.engineUrl;
}

/** Non-hook accessor for module-scope callers (api/engine.ts, api/ps5.ts).
 *  The live override wins when present so direct fetches follow a
 *  fallback port; otherwise the persisted preference is used. */
export function getEngineUrl(): string {
  const preferred = useEngineStore.getState().engineUrl;
  // The live override is where THIS device's own engine bound. It only applies while the
  // preference is this device's engine: with a self-hosted engine chosen (a NAS, Docker), the
  // app's own engine still starts and reports its port, which must not take the user's
  // choice over.
  return liveEngineUrl && isLoopbackUrl(preferred) ? liveEngineUrl : preferred;
}

/** True for a URL whose host is this device's loopback. */
export function isLoopbackUrl(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  host = host.replace(/^\[|\]$/g, "");
  return host === "localhost" || host === "::1" || /^127\./.test(host);
}

/**
 * True when the engine runs on this device, so a path picked or dropped here
 * is one the engine can open.
 *
 * False in the browser build (the engine is the server the page came from)
 * and when the desktop app points at a remote engine — a homelab Docker
 * container, say. The desktop app used to hand such an engine its own local
 * paths (`C:\Games\x.pkg`), which the engine could never open; every file
 * pick against a remote engine failed. Callers browse the engine's disk
 * instead, the way the web UI does.
 */
export function engineIsOnThisDevice(): boolean {
  return isTauriEnv() && isLoopbackUrl(getEngineUrl());
}
