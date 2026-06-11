import { create } from "zustand";
import { hostOf } from "../lib/addr";
import { useRunningAppsStore } from "./runningApps";

/**
 * Per-title play-time accumulator.
 *
 * Driven by the existing `useRunningAppsStore` updates — every time
 * the running set changes, we compute the elapsed wall-clock since
 * the previous update and credit each currently-running title with
 * that delta.
 *
 * Why this approach rather than a self-owned poll loop:
 *   - Avoids duplicate network calls. RunningAppsPanel already polls
 *     PROC_LIST + app.db every few seconds.
 *   - Naturally pauses when the window is hidden (Phase 65 paused
 *     the panel's polling).
 *
 * Deltas larger than 5 minutes are capped — a sleeping laptop, USB
 * reconnect, or other gap shouldn't credit 8 hours of playtime.
 *
 * Storage: localStorage. Survives app restart but not OS reinstall.
 *
 * PER-HOST (multi-console). Counts are partitioned by console (`byHost`,
 * keyed by bare host) so the SAME game played on two PS5s tracks its time
 * separately — playing a title 2h on the Pro and 1h on the Phat shows 2h
 * under the Pro tab and 1h under the Phat tab, not a merged 3h. The
 * accumulator already knew which console a sample came from (it skipped
 * crediting across host changes); this just gives each console its own
 * bucket instead of summing into one global map. Reads go through
 * `playSecondsFor(s, host, titleId)`.
 */

// v2: per-host partitioning. v1's flat `seconds` map had no host info to
// migrate by, so old counts are dropped on upgrade — playtime is a soft
// stat the accumulator rebuilds within seconds of reconnecting.
const STORAGE_KEY = "ps5upload.play_time.v2";
const MAX_DELTA_MS = 5 * 60 * 1000;

const keyOf = (host: string | null | undefined): string =>
  host?.trim() ? hostOf(host) : "";

interface PlayTimeState {
  /** Accumulated seconds keyed by bare host, then by title_id (CUSAxxxxx). */
  byHost: Record<string, Record<string, number>>;
  /** Last sample timestamp (ms). */
  lastSampleMs: number;
  /** Add a delta to the named titles on one console. */
  credit: (host: string, titleIds: string[], deltaSec: number) => void;
  /** Reset a single title's count on one console (manual override). */
  reset: (host: string, titleId: string) => void;
  /** Wipe all counts across every console. */
  resetAll: () => void;
}

/** Read one console's accumulated seconds for a title from a snapshot. */
export function playSecondsFor(
  s: { byHost: Record<string, Record<string, number>> },
  host: string | null | undefined,
  titleId: string | null | undefined,
): number | undefined {
  if (!titleId) return undefined;
  return s.byHost[keyOf(host)]?.[titleId];
}

function loadStored(): {
  byHost: Record<string, Record<string, number>>;
  lastSampleMs: number;
} {
  if (typeof window === "undefined") return { byHost: {}, lastSampleMs: 0 };
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { byHost: {}, lastSampleMs: 0 };
    const parsed = JSON.parse(raw) as {
      byHost?: Record<string, Record<string, number>>;
      lastSampleMs?: number;
    };
    return {
      byHost:
        parsed.byHost && typeof parsed.byHost === "object" ? parsed.byHost : {},
      lastSampleMs:
        typeof parsed.lastSampleMs === "number" ? parsed.lastSampleMs : 0,
    };
  } catch {
    return { byHost: {}, lastSampleMs: 0 };
  }
}

function persist(state: {
  byHost: Record<string, Record<string, number>>;
  lastSampleMs: number;
}) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // best-effort
  }
}

const initial = loadStored();

export const usePlayTimeStore = create<PlayTimeState>((set, get) => ({
  byHost: initial.byHost,
  lastSampleMs: initial.lastSampleMs,
  credit: (host, titleIds, deltaSec) => {
    if (titleIds.length === 0 || deltaSec <= 0) return;
    const key = keyOf(host);
    const byHost = get().byHost;
    const next = { ...(byHost[key] ?? {}) };
    for (const t of titleIds) {
      next[t] = (next[t] ?? 0) + deltaSec;
    }
    const newState = {
      byHost: { ...byHost, [key]: next },
      lastSampleMs: Date.now(),
    };
    set(newState);
    persist(newState);
  },
  reset: (host, titleId) => {
    const key = keyOf(host);
    const byHost = get().byHost;
    if (!byHost[key]?.[titleId]) return;
    const next = { ...byHost[key] };
    delete next[titleId];
    const newState = {
      byHost: { ...byHost, [key]: next },
      lastSampleMs: get().lastSampleMs,
    };
    set(newState);
    persist(newState);
  },
  resetAll: () => {
    const newState = { byHost: {}, lastSampleMs: 0 };
    set(newState);
    persist(newState);
  },
}));

/** Install the cross-store subscription. Call once at app boot
 *  (idempotent). The runner subscribes for the lifetime of the app. */
let installed = false;
export function installPlayTimeAccumulator() {
  if (installed) return;
  installed = true;
  let lastUpdateMs = Date.now();
  let lastTitles: string[] = [];
  let lastHost: string | null = null;

  // Seed from current state
  const initialRunning = useRunningAppsStore.getState();
  if (initialRunning.updatedAtMs > 0) {
    lastUpdateMs = initialRunning.updatedAtMs;
    lastTitles = Array.from(initialRunning.titleIds);
    lastHost = initialRunning.host;
  }

  useRunningAppsStore.subscribe((state) => {
    const now = state.updatedAtMs;
    // Skip credit when the host changed since the last sample —
    // the previous PS5's running titles aren't ours to credit any
    // more, and the new PS5 hasn't yet reported. clearForHostChange
    // sets updatedAtMs back to 0 so this branch catches it.
    if (now <= 0 || state.host !== lastHost) {
      lastUpdateMs = now > 0 ? now : Date.now();
      lastTitles = Array.from(state.titleIds);
      lastHost = state.host;
      return;
    }
    const deltaMs = Math.min(now - lastUpdateMs, MAX_DELTA_MS);
    if (deltaMs > 0 && lastTitles.length > 0 && lastHost) {
      // Credit titles that were running BEFORE this update — they
      // accrued time over the just-elapsed interval, on the console
      // they were running on (lastHost === state.host in this branch).
      usePlayTimeStore
        .getState()
        .credit(lastHost, lastTitles, Math.round(deltaMs / 1000));
    }
    lastUpdateMs = now;
    lastTitles = Array.from(state.titleIds);
    lastHost = state.host;
  });
}

/** Format seconds as e.g. "12h 34m" or "5m 12s" or "—" for zero. */
export function formatPlayTime(sec: number | undefined): string {
  if (!sec || sec <= 0) return "—";
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}
