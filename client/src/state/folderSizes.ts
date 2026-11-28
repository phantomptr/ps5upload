// Known folder sizes for the Files listing (#974), and the engine's queue of folders to measure.
//
// A folder's sizes are fetched once each time its listing loads, never on a timer. Only while
// something is queued or being measured on a console is that console's queue polled, and the
// folders on screen are fetched again when a size changed (the queue's `rev`). Nothing polls once
// the queue is empty or nobody shows a folder of that console.

import { useEffect } from "react";
import { create } from "zustand";

import {
  EMPTY_QUEUE,
  folderSizesApi,
  queueActive,
  type FolderSizes,
  type MeasureQueue,
} from "../api/folderSizes";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { registerSettingReload } from "../lib/liveSettings";

/** Between two looks at a busy queue. */
export const POLL_MS = 1500;

const key = (addr: string, dir: string) => `${addr}\u0000${dir}`;

interface FolderSizesState {
  /** Per console and folder (`addr\0dir`). */
  dirs: Record<string, FolderSizes>;
  /** Per console. */
  queues: Record<string, MeasureQueue>;
  load: (addr: string, dir: string) => Promise<void>;
  measure: (addr: string, paths: string[]) => Promise<void>;
  measureAllIn: (addr: string, dir: string) => Promise<void>;
  cancel: (addr: string, paths?: string[]) => Promise<void>;
}

/** Folders on screen, per console, with how many views show each. */
const watched = new Map<string, Map<string, number>>();
const timers = new Map<string, ReturnType<typeof setTimeout>>();

function watch(addr: string, dir: string): () => void {
  const m = watched.get(addr) ?? new Map<string, number>();
  watched.set(addr, m);
  m.set(dir, (m.get(dir) ?? 0) + 1);
  return () => {
    const n = (m.get(dir) ?? 1) - 1;
    if (n > 0) m.set(dir, n);
    else m.delete(dir);
    if (m.size === 0) {
      watched.delete(addr);
      stopPolling(addr);
    }
  };
}

function stopPolling(addr: string) {
  const t = timers.get(addr);
  if (t !== undefined) clearTimeout(t);
  timers.delete(addr);
}

export const useFolderSizesStore = create<FolderSizesState>((set, get) => {
  /** A new queue for `addr`: refetch what is on screen when a size changed, poll while busy. */
  function takeQueue(addr: string, q: MeasureQueue) {
    const before = get().queues[addr];
    set((s) => ({ queues: { ...s.queues, [addr]: q } }));
    if (before && before.rev !== q.rev) {
      for (const dir of watched.get(addr)?.keys() ?? []) void refetch(addr, dir);
    }
    if (queueActive(q)) schedule(addr);
    else stopPolling(addr);
  }

  async function refetch(addr: string, dir: string) {
    try {
      const r = await folderSizesApi.get(addr, dir);
      set((s) => ({ dirs: { ...s.dirs, [key(addr, dir)]: r } }));
    } catch {
      // Kept as it was: the next listing asks again.
    }
  }

  function schedule(addr: string) {
    if (timers.has(addr) || !watched.has(addr)) return;
    timers.set(
      addr,
      setTimeout(() => {
        timers.delete(addr);
        if (!watched.has(addr)) return;
        folderSizesApi.queue(addr).then(
          (q) => takeQueue(addr, q),
          // An engine that stopped answering is not asked again until the next listing.
          () => {},
        );
      }, POLL_MS),
    );
  }

  return {
    dirs: {},
    queues: {},
    async load(addr, dir) {
      let r: FolderSizes;
      try {
        r = await folderSizesApi.get(addr, dir);
      } catch {
        // An engine without the route (or not answering): sizes stay unknown.
        return;
      }
      set((s) => ({ dirs: { ...s.dirs, [key(addr, dir)]: r } }));
      takeQueue(addr, r.queue);
    },
    async measure(addr, paths) {
      if (paths.length === 0) return;
      takeQueue(addr, await folderSizesApi.measure(addr, paths));
    },
    async measureAllIn(addr, dir) {
      takeQueue(addr, await folderSizesApi.measureAllIn(addr, dir));
    },
    async cancel(addr, paths) {
      takeQueue(addr, await folderSizesApi.cancel(addr, paths));
    },
  };
});

/**
 * The known sizes of `dir` on the console at `addr`, fetched whenever `reloadKey` changes (pass
 * the listing of `dir` itself: each load is a new array, null while it loads). `addr` null:
 * nothing is asked.
 */
export function useFolderSizes(
  addr: string | null,
  dir: string,
  reloadKey: unknown,
): { sizes: FolderSizes | undefined; queue: MeasureQueue } {
  useEffect(() => {
    if (!addr) return;
    return watch(addr, dir);
  }, [addr, dir]);
  // Keyed on the listing alone: a new folder's old rows (still on screen for a moment after the
  // path changed) must not ask for the new folder twice. `dir` is read when its listing lands.
  useEffect(() => {
    if (!addr || reloadKey === null || reloadKey === undefined) return;
    void useFolderSizesStore.getState().load(addr, dir);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [addr, reloadKey]);
  const sizes = useFolderSizesStore((s) => (addr ? s.dirs[key(addr, dir)] : undefined));
  const queue = useFolderSizesStore((s) => (addr ? s.queues[addr] : undefined)) ?? EMPTY_QUEUE;
  return { sizes, queue };
}

/** Test seam: forget everything and stop every poll. */
export function resetFolderSizes() {
  for (const a of [...timers.keys()]) stopPolling(a);
  watched.clear();
  useFolderSizesStore.setState({ dirs: {}, queues: {} });
}

/** Test seam: show `dir` of `addr` as a screen would. */
export const watchFolderForTest = watch;

// ── "Keep folder sizes fresh" (off by default; the engine reads it from the shared settings) ──

export const KEEP_SIZES_FRESH_KEY = "ps5upload.files_keep_sizes_fresh";

export function readKeepSizesFresh(): boolean {
  return safeGetItem(KEEP_SIZES_FRESH_KEY) === "true";
}

interface KeepFreshState {
  keepFresh: boolean;
  setKeepFresh: (on: boolean) => void;
}

export const useKeepSizesFresh = create<KeepFreshState>((set) => ({
  keepFresh: typeof window === "undefined" ? false : readKeepSizesFresh(),
  setKeepFresh: (on) => {
    safeSetItem(KEEP_SIZES_FRESH_KEY, on ? "true" : "false");
    set({ keepFresh: on });
  },
}));

registerSettingReload([KEEP_SIZES_FRESH_KEY], () =>
  useKeepSizesFresh.setState({ keepFresh: readKeepSizesFresh() }),
);
