import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { useConnectionStore } from "./connection";

/**
 * Shared set of currently-running title IDs. Populated by
 * RunningAppsPanel's poll loop; consumed by Library rows to show
 * a "running" badge. Avoids each Library row maintaining its own
 * polling effect (200 rows × 5s polling = bad).
 *
 * In-memory only (no localStorage) — running state is volatile and
 * meaningless across app launches.
 */

interface RunningAppsState {
  /** Title IDs whose app_id is currently in PROC_LIST. Empty when no
   *  RunningAppsPanel has populated it yet (default). */
  titleIds: Set<string>;
  /** Bare host (no port) these title IDs were observed on. When the
   *  user switches to a different PS5 (multi-PS5 roster), Library
   *  rows would otherwise briefly show "running" badges for the
   *  previous console's apps until RunningAppsPanel re-mounts and
   *  rewrites. We now clear on host change. */
  host: string | null;
  /** Last update timestamp (ms). UI uses this to detect stale data
   *  if RunningAppsPanel is unmounted. */
  updatedAtMs: number;
  setRunning: (titleIds: string[], host: string | null) => void;
  /** Drop the set without bumping `updatedAtMs` past the existing
   *  value — used by the connection-store subscriber on profile
   *  switch, before the new PS5's RunningAppsPanel has had a chance
   *  to publish. */
  clearForHostChange: (host: string | null) => void;
}

/** Every writer used to pass its own address shape (bare host from the Games
 *  grid, `host:port` from RunningAppsPanel), so the same console flipped
 *  between two keys, a reader comparing against the bare host saw nothing,
 *  and each flip reallocated the set. One shape for everyone. */
function storeHost(host: string | null): string | null {
  const h = host ? hostOf(host.trim()) : "";
  return h || null;
}

const EMPTY: ReadonlySet<string> = new Set();

export const useRunningAppsStore = create<RunningAppsState>((set, get) => ({
  titleIds: new Set(),
  host: null,
  updatedAtMs: 0,
  setRunning: (titleIds, rawHost) => {
    const host = storeHost(rawHost);
    // Short-circuit when the observed set is identical to the current
    // one. Without this check, every poll tick (every 5 s) allocates a
    // fresh Set and zustand reports state-changed via Object.is —
    // re-rendering every Library row that subscribes to `titleIds`
    // even when nothing is actually running. The Library can have
    // hundreds of rows, so the steady-state CPU cost was visible.
    const prev = get();
    const sameHost = host === prev.host;
    const sameCount = titleIds.length === prev.titleIds.size;
    const sameMembers = sameCount && titleIds.every((id) => prev.titleIds.has(id));
    if (sameHost && sameCount && sameMembers) {
      // Still update timestamp so staleness detection works — but reuse
      // the existing Set reference so subscribers don't re-render.
      set({ updatedAtMs: Date.now() });
      return;
    }
    set({
      titleIds: new Set(titleIds),
      host,
      updatedAtMs: Date.now(),
    });
  },
  clearForHostChange: (rawHost) => {
    const host = storeHost(rawHost);
    // Skip the set() if we're already empty for this host — same
    // anti-churn rationale as setRunning. Multiple connection-store
    // subscribers can fire this in quick succession on host change.
    const prev = get();
    if (prev.host === host && prev.titleIds.size === 0) return;
    set({ titleIds: new Set(), host, updatedAtMs: 0 });
  },
}));

/**
 * Whether any game is running on the active console right now.
 *
 * Drives the "playing" dot on the Games tab. Reads the shared store rather
 * than polling, so it costs nothing wherever it is rendered — the store is
 * kept current by `installRunningWatch` at the shell level, and by the
 * faster screen loops whenever Games or Library is open.
 *
 * Only counts titles observed on the selected console, so it cannot report
 * the previous PS5's games while the new one has not answered yet.
 */
export function useAnyGameRunning(): boolean {
  const host = useConnectionStore((s) => s.host);
  return useRunningAppsStore((s) => runningOn(s, host).size > 0);
}

/** The running set when it was observed on `host` (any address shape), else
 *  empty — so a reader can never show one console's games under another. */
export function runningOn(
  s: Pick<RunningAppsState, "titleIds" | "host">,
  host: string | null | undefined,
): ReadonlySet<string> {
  const h = storeHost(host ?? null);
  return h && s.host === h ? s.titleIds : EMPTY;
}

/** Title ids running on `host`. Stable reference while the set is unchanged. */
export function useRunningTitleIds(host: string | null | undefined): ReadonlySet<string> {
  return useRunningAppsStore((s) => runningOn(s, host));
}
