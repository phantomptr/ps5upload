import { create } from "zustand";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
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
  /** Wall-clock ms of the last time ps5upload OBSERVED each title running,
   *  keyed by bare host then title_id. Distinct from `byHost` (cumulative
   *  seconds) — this is "when did we last see it playing", which drives the
   *  Installed-Apps "not played recently" surface (#116). Only reflects play
   *  ps5upload actually saw; a game played with the app closed isn't counted
   *  (there's no honest console-side last-played the app can read cheaply). */
  lastSeenByHost: Record<string, Record<string, number>>;
  /** Last sample timestamp (ms). */
  lastSampleMs: number;
  /** Add a delta to the named titles on one console (and stamp last-seen). */
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

/** Read one console's last-observed-playing timestamp (ms) for a title.
 *  `undefined` = ps5upload has never seen this title running (a strong
 *  "you haven't played this while using the app" signal for #116). */
export function lastSeenPlayingFor(
  s: { lastSeenByHost: Record<string, Record<string, number>> },
  host: string | null | undefined,
  titleId: string | null | undefined,
): number | undefined {
  if (!titleId) return undefined;
  return s.lastSeenByHost[keyOf(host)]?.[titleId];
}

interface StoredPlayTime {
  byHost: Record<string, Record<string, number>>;
  lastSeenByHost: Record<string, Record<string, number>>;
  lastSampleMs: number;
}

function loadStored(): StoredPlayTime {
  const empty: StoredPlayTime = {
    byHost: {},
    lastSeenByHost: {},
    lastSampleMs: 0,
  };
  if (typeof window === "undefined") return empty;
  try {
    const raw = safeGetItem(STORAGE_KEY);
    if (!raw) return empty;
    const parsed = JSON.parse(raw) as Partial<StoredPlayTime>;
    return {
      byHost:
        parsed.byHost && typeof parsed.byHost === "object" ? parsed.byHost : {},
      // `lastSeenByHost` is additive over v2 (old payloads lack it) — default
      // to empty so a pre-#116 stored blob upgrades cleanly. Never-seen titles
      // simply have no timestamp until observed running once.
      lastSeenByHost:
        parsed.lastSeenByHost && typeof parsed.lastSeenByHost === "object"
          ? parsed.lastSeenByHost
          : {},
      lastSampleMs:
        typeof parsed.lastSampleMs === "number" ? parsed.lastSampleMs : 0,
    };
  } catch {
    return empty;
  }
}

function persist(state: StoredPlayTime) {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // best-effort
  }
}

const initial = loadStored();

export const usePlayTimeStore = create<PlayTimeState>((set, get) => ({
  byHost: initial.byHost,
  lastSeenByHost: initial.lastSeenByHost,
  lastSampleMs: initial.lastSampleMs,
  credit: (host, titleIds, deltaSec) => {
    if (titleIds.length === 0 || deltaSec <= 0) return;
    const key = keyOf(host);
    const now = Date.now();
    const { byHost, lastSeenByHost } = get();
    const nextSeconds = { ...(byHost[key] ?? {}) };
    const nextSeen = { ...(lastSeenByHost[key] ?? {}) };
    for (const t of titleIds) {
      nextSeconds[t] = (nextSeconds[t] ?? 0) + deltaSec;
      // Stamp "last observed running" for each currently-running title.
      nextSeen[t] = now;
    }
    const newState = {
      byHost: { ...byHost, [key]: nextSeconds },
      lastSeenByHost: { ...lastSeenByHost, [key]: nextSeen },
      lastSampleMs: now,
    };
    set(newState);
    persist(newState);
  },
  reset: (host, titleId) => {
    const key = keyOf(host);
    const { byHost, lastSeenByHost, lastSampleMs } = get();
    if (!byHost[key]?.[titleId] && !lastSeenByHost[key]?.[titleId]) return;
    const nextSeconds = { ...(byHost[key] ?? {}) };
    delete nextSeconds[titleId];
    const nextSeen = { ...(lastSeenByHost[key] ?? {}) };
    delete nextSeen[titleId];
    const newState = {
      byHost: { ...byHost, [key]: nextSeconds },
      lastSeenByHost: { ...lastSeenByHost, [key]: nextSeen },
      lastSampleMs,
    };
    set(newState);
    persist(newState);
  },
  resetAll: () => {
    const newState = { byHost: {}, lastSeenByHost: {}, lastSampleMs: 0 };
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

/** Number of whole days between `ms` and now (>= 0). `undefined` in → null
 *  ("never observed"). Pure + `now`-injectable for tests. */
export function daysSinceLastSeen(
  ms: number | undefined,
  now = Date.now(),
): number | null {
  if (!ms || ms <= 0) return null;
  return Math.max(0, Math.floor((now - ms) / (24 * 60 * 60 * 1000)));
}

/** Human "last seen playing" label: "never", "today", "yesterday", or
 *  "N days ago". Drives the Installed-Apps not-played surface (#116). The
 *  honest phrasing is "seen" — it's when ps5upload last observed the title
 *  running, not Sony's console-side last-played (which the app can't read
 *  cheaply). `now` injectable for tests. */
export function formatLastSeen(ms: number | undefined, now = Date.now()): string {
  const d = daysSinceLastSeen(ms, now);
  if (d === null) return "never";
  if (d === 0) return "today";
  if (d === 1) return "yesterday";
  return `${d} days ago`;
}
