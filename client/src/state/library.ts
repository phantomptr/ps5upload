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
