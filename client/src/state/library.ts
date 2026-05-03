import { create } from "zustand";
import type { LibraryEntry, Volume } from "../api/ps5";

/**
 * Library state lives in a zustand store (not `useState` in the
 * component) so entries survive navigation. When the user clicks
 * Library → Upload → Library again, they see their last-scanned list
 * immediately while a background refresh (fired by the effect in
 * LibraryScreen) updates it in place — no "blank then pop" flicker.
 *
 * Also holds the image-path → mount-point map used to show the
 * MOUNTED badge on archive rows, same caching rationale: switching
 * tabs shouldn't lose the information you already had.
 */
export interface LibraryState {
  entries: LibraryEntry[] | null;
  /** image_path → mount_point, empty until first refresh completes. */
  mountMap: Map<string, string>;
  /** Writable, non-placeholder volumes the PS5 reports — used by the
   *  Move modal so the user picks a destination from real attached
   *  drives. Same probe that feeds `mountMap`, kept here so move's
   *  destination dropdown doesn't have to re-fetch on every modal open. */
  volumes: Volume[];
  /** When the data was last refreshed (unix ms). null = never loaded. */
  lastRefreshedAt: number | null;
  /** True while a refresh is in flight. Used for the header spinner
   *  without wiping the existing entries. */
  loading: boolean;
  error: string | null;

  setData: (
    entries: LibraryEntry[],
    mountMap: Map<string, string>,
    volumes: Volume[],
  ) => void;
  setLoading: (loading: boolean) => void;
  setError: (error: string | null) => void;
  /** Optimistic mount add. Lets the UI flip the row to MOUNTED +
   *  Unmount-button immediately on a successful fs_mount, before the
   *  background rescan refreshes volumes. The next setData() will
   *  overwrite this with the authoritative state from getmntinfo. */
  addMount: (imagePath: string, mountPoint: string) => void;
  /** Optimistic mount remove. Counterpart to addMount; flips the row
   *  back to NOT-mounted immediately on a successful fs_unmount. */
  removeMount: (imagePath: string) => void;
  clear: () => void;
}

export const useLibraryStore = create<LibraryState>((set) => ({
  entries: null,
  mountMap: new Map(),
  volumes: [],
  lastRefreshedAt: null,
  loading: false,
  error: null,
  setData: (entries, mountMap, volumes) =>
    set({
      entries,
      mountMap,
      volumes,
      lastRefreshedAt: Date.now(),
      error: null,
    }),
  setLoading: (loading) => set({ loading }),
  setError: (error) => set({ error }),
  addMount: (imagePath, mountPoint) =>
    set((s) => {
      const next = new Map(s.mountMap);
      next.set(imagePath, mountPoint);
      return { mountMap: next };
    }),
  removeMount: (imagePath) =>
    set((s) => {
      if (!s.mountMap.has(imagePath)) return {};
      const next = new Map(s.mountMap);
      next.delete(imagePath);
      return { mountMap: next };
    }),
  clear: () =>
    set({
      entries: null,
      mountMap: new Map(),
      volumes: [],
      lastRefreshedAt: null,
      loading: false,
      error: null,
    }),
}));
