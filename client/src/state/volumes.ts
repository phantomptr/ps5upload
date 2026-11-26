import { useEffect } from "react";
import { create } from "zustand";
import { fetchVolumes, type Volume } from "../api/ps5";
import { consoleAddr, hostOf } from "../lib/addr";
import { isTerminal, useTaskStore, type TaskKind } from "./tasks";

/**
 * One console's drives, as last read. Shared (#944): Files kept its own copy, read once per
 * connection, so its drive chips went on showing the free space of an hour ago while Volumes
 * showed the live figure; Files' own Refresh listed the folder again and left the drives alone.
 * Every screen that shows a drive's free space now reads this one slot, so two screens cannot
 * disagree, and the slot is read again:
 *
 *   - when a screen showing it opens or comes back (a hidden screen's effects are torn down);
 *   - on that screen's Refresh;
 *   - when a job on the console ends (an upload, a delete, a move, an extract, an install):
 *     anything that can change free space registers a task, on whichever screen it started.
 */
export interface VolumesSlot {
  /** Every volume the console reports, unfiltered, as last read. null = never read. */
  volumes: Volume[] | null;
  /** When `volumes` was read (unix ms). null = never. */
  loadedAtMs: number | null;
  /** A read is in flight. The last list stays on screen meanwhile. */
  loading: boolean;
  /** Why the last read failed (raw text, for the screen to word); `volumes` is then the
   *  reading before it. null = the last read worked. */
  error: string | null;
}

/** A stable slot for a console with nothing read yet (selectors compare by identity). */
export const IDLE_VOLUMES: VolumesSlot = { volumes: null, loadedAtMs: null, loading: false, error: null };

interface VolumesState {
  byHost: Record<string, VolumesSlot>;
  patch: (key: string, patch: Partial<VolumesSlot>) => void;
}

export const useVolumesStore = create<VolumesState>((set) => ({
  byHost: {},
  patch: (key, patch) =>
    set((s) => ({ byHost: { ...s.byHost, [key]: { ...(s.byHost[key] ?? IDLE_VOLUMES), ...patch } } })),
}));

const keyOf = (host: string | null | undefined): string => (host?.trim() ? hostOf(host.trim()) : "");

/** Read one console's slot from a store snapshot. */
export function volumesForHost(
  s: { byHost: Record<string, VolumesSlot> },
  host: string | null | undefined,
): VolumesSlot {
  return s.byHost[keyOf(host)] ?? IDLE_VOLUMES;
}

const inflight = new Map<string, Promise<Volume[] | null>>();

/**
 * Read the console's drives into its slot. One read per console at a time: a second caller
 * gets the read in flight. With `maxAgeMs`, a list read that recently is kept (for a refresh
 * that is only a courtesy, as on moving between folders).
 *
 * Resolves with the list, or null when the read failed (the slot then says why).
 */
export function refreshVolumes(
  host: string | null | undefined,
  opts?: { maxAgeMs?: number },
): Promise<Volume[] | null> {
  const key = keyOf(host);
  if (!key) return Promise.resolve(null);
  const running = inflight.get(key);
  if (running) return running;
  const slot = volumesForHost(useVolumesStore.getState(), key);
  if (
    opts?.maxAgeMs !== undefined &&
    slot.volumes !== null &&
    slot.loadedAtMs !== null &&
    Date.now() - slot.loadedAtMs < opts.maxAgeMs
  ) {
    return Promise.resolve(slot.volumes);
  }
  const { patch } = useVolumesStore.getState();
  patch(key, { loading: true });
  const run = fetchVolumes(consoleAddr(key))
    .then(
      (read) => {
        // A reading that says what the last one said keeps the last one's array: screens
        // whose effects follow the list (a destination picker choosing a default) are not
        // run again by a refresh that changed nothing.
        const before = volumesForHost(useVolumesStore.getState(), key).volumes;
        const volumes = before && JSON.stringify(before) === JSON.stringify(read) ? before : read;
        patch(key, { volumes, loadedAtMs: Date.now(), loading: false, error: null });
        return volumes as Volume[] | null;
      },
      (e: unknown) => {
        // The last list stays: one read lost to a busy console must not empty a destination
        // picker mid-queue. `error` says it is no longer a fresh reading; the Volumes screen
        // shows that instead of the list.
        patch(key, { loading: false, error: e instanceof Error ? e.message : String(e) });
        return null;
      },
    )
    .finally(() => {
      if (inflight.get(key) === run) inflight.delete(key);
    });
  inflight.set(key, run);
  return run;
}

/** A read after something changed on the console. One in flight may have read the drives
 *  before the change, so one more runs after it. */
export function refreshVolumesAfterChange(host: string | null | undefined): Promise<Volume[] | null> {
  const key = keyOf(host);
  if (!key) return Promise.resolve(null);
  const running = inflight.get(key);
  return running ? running.then(() => refreshVolumes(key)) : refreshVolumes(key);
}

// ── Read again when a job on the console ends ───────────────────────────────────────────

/** How many screens on show are reading each console's drives. A console nobody is looking at
 *  is not read: the next screen to show it reads it then. */
const watchers = new Map<string, number>();

/** Jobs that cannot change a drive's free space. Every other kind is taken to. */
const NO_SPACE_CHANGE: ReadonlySet<TaskKind> = new Set<TaskKind>([
  "icon-fetch",
  "library-launch",
  "fakelib-scan",
  "helper-send",
  "payload-playlist",
  "app-update",
  "bug-report",
]);

/** A queue of small jobs ends many times in a moment: one read covers them. */
export const VOLUMES_SETTLE_MS = 400;
const settling = new Map<string, ReturnType<typeof setTimeout>>();

function refreshSoon(key: string): void {
  const pending = settling.get(key);
  if (pending) clearTimeout(pending);
  settling.set(
    key,
    setTimeout(() => {
      settling.delete(key);
      if ((watchers.get(key) ?? 0) > 0) void refreshVolumesAfterChange(key);
    }, VOLUMES_SETTLE_MS),
  );
}

let wired = false;
function wireTasks(): void {
  if (wired) return;
  wired = true;
  useTaskStore.subscribe((state, prev) => {
    if (state.tasks === prev.tasks || watchers.size === 0) return;
    // The few jobs that were running: which of them has just ended?
    for (const before of prev.tasks) {
      if (isTerminal(before.status) || NO_SPACE_CHANGE.has(before.kind)) continue;
      const key = keyOf(before.consoleId);
      if (!key || (watchers.get(key) ?? 0) === 0) continue;
      const now = state.tasks.find((t) => t.id === before.id);
      if (now && isTerminal(now.status)) refreshSoon(key);
    }
  });
}

/**
 * A screen on show starts reading a console's drives: read them now, and again whenever a job
 * on that console ends, until the returned function is called (the screen left, or was hidden).
 */
export function watchConsoleVolumes(host: string | null | undefined): () => void {
  const key = keyOf(host);
  if (!key) return () => {};
  wireTasks();
  watchers.set(key, (watchers.get(key) ?? 0) + 1);
  void refreshVolumes(key);
  let watching = true;
  return () => {
    if (!watching) return;
    watching = false;
    const left = (watchers.get(key) ?? 1) - 1;
    if (left > 0) watchers.set(key, left);
    else watchers.delete(key);
  };
}

/**
 * The console's drives for a screen that shows them: the shared slot, read when the screen
 * opens or comes back into view, and again when a job on the console ends while it is on show.
 *
 * `enabled` false reads nothing (no console, or the helper is not answering).
 */
export function useConsoleVolumes(host: string | null | undefined, enabled = true): VolumesSlot {
  const key = keyOf(host);
  const slot = useVolumesStore((s) => s.byHost[key] ?? IDLE_VOLUMES);
  useEffect(() => {
    if (!key || !enabled) return;
    return watchConsoleVolumes(key);
  }, [key, enabled]);
  return slot;
}

/** Tests only: forget every console's drives and pending reads. */
export function resetVolumesForTests(): void {
  for (const t of settling.values()) clearTimeout(t);
  settling.clear();
  inflight.clear();
  watchers.clear();
  useVolumesStore.setState({ byHost: {} });
}
