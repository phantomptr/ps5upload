import { create } from "zustand";
import type { LibraryEntry, Volume } from "../api/ps5";
import { hostOf } from "../lib/addr";

/**
 * Library state lives in a zustand store (not `useState` in the component) so
 * entries survive navigation. When the user clicks Library → Upload → Library
 * again, they see their last-scanned list immediately while a background
 * refresh updates it in place — no "blank then pop" flicker.
 *
 * PER-HOST (multi-console). The library is keyed by console (`byHost`), so each
 * tab keeps its OWN last-known game list, mount map and volumes. Switching
 * consoles shows that console's library instantly and refreshes it in the
 * background — there's no clear-on-switch (the old single-global-slot model
 * blanked on every switch, and worse, left a window where a stale Unmount /
 * Delete could target the wrong PS5 because the rendered path belonged to the
 * previous console). Reads go through `libraryForHost(s, host)`.
 */
export interface LibrarySlot {
  entries: LibraryEntry[] | null;
  /** image_path → mount_point, derived from the probe (authoritative).
   *  Empty until first refresh completes. The probe-derived map can
   *  lag the actual kernel state by a beat right after fs_mount —
   *  that's what `pendingMounts` covers. */
  mountMap: Map<string, string>;
  /** image_path → mount_point for fs_mount calls the client just made,
   *  kept here until the next probe surfaces them in `mountMap`. The row
   *  reads the union so a successful fs_mount immediately flips to MOUNTED. */
  pendingMounts: Map<string, string>;
  /** Writable, non-placeholder volumes the PS5 reports — used by the Move
   *  modal so the user picks a destination from real attached drives. */
  volumes: Volume[];
  /** When the data was last refreshed (unix ms). null = never loaded. */
  lastRefreshedAt: number | null;
  /** True while a refresh is in flight. Used for the header spinner without
   *  wiping the existing entries. */
  loading: boolean;
  error: string | null;
}

/** Stable idle slot returned by `libraryForHost` for a console with no data
 *  yet. MUST be a singleton (not a fresh object/Map per call) so Zustand
 *  selectors comparing by `Object.is` stay stable. Its Maps are never mutated —
 *  every store action builds new Maps. */
export const IDLE_LIBRARY: LibrarySlot = {
  entries: null,
  mountMap: new Map(),
  pendingMounts: new Map(),
  volumes: [],
  lastRefreshedAt: null,
  loading: false,
  error: null,
};

interface LibraryStore {
  /** Per-console library state, keyed by bare host (port-stripped). */
  byHost: Record<string, LibrarySlot>;
  /** Commit a fresh scan for one console. Prunes pending mounts the probe has
   *  now confirmed, and — crucially — keeps the previous entries if the new
   *  scan came back empty but we DID have entries (the "library goes blank
   *  after mounting an image" fix). Single set() callback so the read + write
   *  are atomic against a concurrent invalidate. */
  setData: (
    host: string,
    entries: LibraryEntry[],
    mountMap: Map<string, string>,
    volumes: Volume[],
  ) => void;
  setLoading: (host: string, loading: boolean) => void;
  setError: (host: string, error: string | null) => void;
  /** Optimistic mount add — flips the row to MOUNTED before the rescan. */
  addMount: (host: string, imagePath: string, mountPoint: string) => void;
  /** Optimistic mount remove — flips the row back before the rescan. */
  removeMount: (host: string, imagePath: string) => void;
}

const keyOf = (host: string | null | undefined): string =>
  host?.trim() ? hostOf(host) : "";

/** Read one console's library slot from a store snapshot. */
export function libraryForHost(
  s: { byHost: Record<string, LibrarySlot> },
  host: string | null | undefined,
): LibrarySlot {
  return s.byHost[keyOf(host)] ?? IDLE_LIBRARY;
}

/** Effective mount lookup — probe-derived `mountMap` is authoritative, but a
 *  not-yet-confirmed `pendingMounts` entry covers the post-fs_mount window. The
 *  Library row reads through this so a freshly-mounted image flips to MOUNTED
 *  in the same render the user clicks. Operates on a single console's slot. */
export function effectiveMount(
  s: Pick<LibrarySlot, "mountMap" | "pendingMounts">,
  imagePath: string,
): string | null {
  return s.mountMap.get(imagePath) ?? s.pendingMounts.get(imagePath) ?? null;
}

/** Reverse lookup: given a path inside one of the active mounts, return the
 *  backing image_path. Longest mount-point match wins so nested mounts
 *  attribute correctly. Operates on a single console's slot. */
export function findOwningImage(
  s: Pick<LibrarySlot, "mountMap" | "pendingMounts">,
  candidatePath: string,
): string | null {
  let bestImage: string | null = null;
  let bestMountLen = -1;
  const consider = (image: string, mount: string) => {
    const root = mount.endsWith("/") ? mount : mount + "/";
    if (candidatePath === mount || candidatePath.startsWith(root)) {
      if (mount.length > bestMountLen) {
        bestImage = image;
        bestMountLen = mount.length;
      }
    }
  };
  for (const [image, mount] of s.mountMap) consider(image, mount);
  for (const [image, mount] of s.pendingMounts) consider(image, mount);
  return bestImage;
}

export const useLibraryStore = create<LibraryStore>((set) => {
  const patch = (host: string, partial: Partial<LibrarySlot>) =>
    set((s) => {
      const key = keyOf(host);
      const cur = s.byHost[key] ?? IDLE_LIBRARY;
      return { byHost: { ...s.byHost, [key]: { ...cur, ...partial } } };
    });
  return {
    byHost: {},
    setData: (host, entries, mountMap, volumes) =>
      set((s) => {
        const key = keyOf(host);
        const cur = s.byHost[key] ?? IDLE_LIBRARY;
        // Prune pending entries the probe has now confirmed (or disagrees
        // with — the probe reads getmntinfo directly and wins). Anything left
        // is a mount the probe hasn't picked up yet; keep it so the row
        // doesn't flip back to "Mount".
        const prunedPending = new Map<string, string>();
        for (const [imagePath, mountPoint] of cur.pendingMounts) {
          if (!mountMap.has(imagePath)) {
            prunedPending.set(imagePath, mountPoint);
          }
        }
        // Don't blank the list on an empty rescan if we had entries — keep the
        // old ones, refresh only mountMap + volumes, and let the next scan
        // produce a real count. (The "library goes blank after mounting"
        // symptom.)
        const hadEntries = (cur.entries ?? []).length > 0;
        const nextEntries =
          entries.length === 0 && hadEntries ? cur.entries : entries;
        return {
          byHost: {
            ...s.byHost,
            [key]: {
              ...cur,
              entries: nextEntries,
              mountMap,
              pendingMounts: prunedPending,
              volumes,
              lastRefreshedAt: Date.now(),
              error: null,
            },
          },
        };
      }),
    setLoading: (host, loading) => patch(host, { loading }),
    setError: (host, error) => patch(host, { error }),
    addMount: (host, imagePath, mountPoint) =>
      set((s) => {
        const key = keyOf(host);
        const cur = s.byHost[key] ?? IDLE_LIBRARY;
        const next = new Map(cur.pendingMounts);
        next.set(imagePath, mountPoint);
        return {
          byHost: { ...s.byHost, [key]: { ...cur, pendingMounts: next } },
        };
      }),
    removeMount: (host, imagePath) =>
      set((s) => {
        const key = keyOf(host);
        const cur = s.byHost[key] ?? IDLE_LIBRARY;
        const inProbe = cur.mountMap.has(imagePath);
        const inPending = cur.pendingMounts.has(imagePath);
        if (!inProbe && !inPending) return {};
        const nextProbe = inProbe ? new Map(cur.mountMap) : cur.mountMap;
        if (inProbe) (nextProbe as Map<string, string>).delete(imagePath);
        const nextPending = inPending
          ? new Map(cur.pendingMounts)
          : cur.pendingMounts;
        if (inPending) (nextPending as Map<string, string>).delete(imagePath);
        return {
          byHost: {
            ...s.byHost,
            [key]: { ...cur, mountMap: nextProbe, pendingMounts: nextPending },
          },
        };
      }),
  };
});
