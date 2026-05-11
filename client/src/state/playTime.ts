import { create } from "zustand";
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
 * Per-PS5 partitioning intentionally NOT done — counts are global
 * across all PS5s in the roster (most users only have one PS5).
 */

const STORAGE_KEY = "ps5upload.play_time.v1";
const MAX_DELTA_MS = 5 * 60 * 1000;

interface PlayTimeState {
  /** Accumulated seconds keyed by title_id (CUSAxxxxx etc). */
  seconds: Record<string, number>;
  /** Last sample timestamp (ms). */
  lastSampleMs: number;
  /** Add a delta to the named titles. */
  credit: (titleIds: string[], deltaSec: number) => void;
  /** Reset a single title's count (manual override; UI affordance). */
  reset: (titleId: string) => void;
  /** Wipe all counts. */
  resetAll: () => void;
}

function loadStored(): { seconds: Record<string, number>; lastSampleMs: number } {
  if (typeof window === "undefined") return { seconds: {}, lastSampleMs: 0 };
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { seconds: {}, lastSampleMs: 0 };
    const parsed = JSON.parse(raw) as {
      seconds?: Record<string, number>;
      lastSampleMs?: number;
    };
    return {
      seconds:
        parsed.seconds && typeof parsed.seconds === "object"
          ? parsed.seconds
          : {},
      lastSampleMs:
        typeof parsed.lastSampleMs === "number" ? parsed.lastSampleMs : 0,
    };
  } catch {
    return { seconds: {}, lastSampleMs: 0 };
  }
}

function persist(state: { seconds: Record<string, number>; lastSampleMs: number }) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // best-effort
  }
}

const initial = loadStored();

export const usePlayTimeStore = create<PlayTimeState>((set, get) => ({
  seconds: initial.seconds,
  lastSampleMs: initial.lastSampleMs,
  credit: (titleIds, deltaSec) => {
    if (titleIds.length === 0 || deltaSec <= 0) return;
    const next = { ...get().seconds };
    for (const t of titleIds) {
      next[t] = (next[t] ?? 0) + deltaSec;
    }
    const newState = { seconds: next, lastSampleMs: Date.now() };
    set(newState);
    persist(newState);
  },
  reset: (titleId) => {
    const next = { ...get().seconds };
    delete next[titleId];
    const newState = { seconds: next, lastSampleMs: get().lastSampleMs };
    set(newState);
    persist(newState);
  },
  resetAll: () => {
    const newState = { seconds: {}, lastSampleMs: 0 };
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
    if (deltaMs > 0 && lastTitles.length > 0) {
      // Credit titles that were running BEFORE this update — they
      // accrued time over the just-elapsed interval.
      usePlayTimeStore.getState().credit(lastTitles, Math.round(deltaMs / 1000));
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
