// The Collection screen's state, kept outside the screen: leaving and coming back keeps the
// search, filters, sort, view, and a scan that is running.

import { create } from "zustand";

import {
  collection,
  type GameConsoleState,
  type CollectionGame,
  type CollectionLibrary,
  type CollectionScanStatus,
  type CollectionSettings,
} from "../api/collection";
import { mgmtAddr } from "../lib/addr";
import type {
  ConsoleFilter,
  CollectionFilter,
  CollectionPlatform,
  CollectionSort,
} from "../lib/collectionView";

export type CollectionView = "grid" | "table";

/** One console's state for every game, as last read. */
export interface ConsoleStates {
  states: Record<string, GameConsoleState>;
  loading: boolean;
  error: string | null;
}

interface CollectionState {
  library: CollectionLibrary | null;
  settings: CollectionSettings | null;
  scan: CollectionScanStatus | null;
  loading: boolean;
  error: string | null;
  /** The game whose details are open. */
  openGameId: string | null;
  search: string;
  filter: CollectionFilter;
  platform: CollectionPlatform;
  sort: CollectionSort;
  view: CollectionView;
  /** Per console (bare host). */
  consoleByHost: Record<string, ConsoleStates>;
  consoleFilter: ConsoleFilter;
  set: (p: Partial<CollectionState>) => void;
}

export const useCollectionStore = create<CollectionState>((set) => ({
  library: null,
  settings: null,
  scan: null,
  loading: false,
  error: null,
  openGameId: null,
  search: "",
  filter: "all",
  platform: "all",
  sort: "title-asc",
  view: "grid",
  consoleByHost: {},
  consoleFilter: "any",
  set: (p) => set(p),
}));

const store = () => useCollectionStore.getState();

let polling: ReturnType<typeof setTimeout> | null = null;

/** Follows a running scan, and reloads the library when it ends. */
function pollScan(): void {
  if (polling) return;
  const tick = async () => {
    polling = null;
    try {
      const st = await collection.scanStatus();
      store().set({ scan: st });
      if (st.running) {
        polling = setTimeout(() => void tick(), 1000);
        return;
      }
      await reloadLibrary();
    } catch (e) {
      store().set({ error: e instanceof Error ? e.message : String(e) });
    }
  };
  polling = setTimeout(() => void tick(), 600);
}

async function reloadLibrary(): Promise<void> {
  const library = await collection.library();
  store().set({ library });
}

/** Loads settings, the index and any running scan. */
export async function loadCollection(): Promise<void> {
  store().set({ loading: true, error: null });
  try {
    const [settings, library, scan] = await Promise.all([
      collection.settings(),
      collection.library(),
      collection.scanStatus(),
    ]);
    store().set({ settings, library, scan, loading: false });
    if (scan.running) pollScan();
  } catch (e) {
    store().set({
      loading: false,
      error: e instanceof Error ? e.message : String(e),
    });
  }
}

export async function startCollectionScan(deep = false): Promise<void> {
  store().set({ error: null });
  try {
    const st = await collection.scan(deep);
    store().set({ scan: st });
    pollScan();
  } catch (e) {
    store().set({ error: e instanceof Error ? e.message : String(e) });
  }
}

export async function cancelCollectionScan(): Promise<void> {
  try {
    store().set({ scan: await collection.cancelScan() });
  } catch {
    /* the poll reports the outcome */
  }
}

async function saveSettings(next: CollectionSettings): Promise<boolean> {
  store().set({ error: null });
  try {
    store().set({ settings: await collection.saveSettings(next) });
    return true;
  } catch (e) {
    store().set({ error: e instanceof Error ? e.message : String(e) });
    return false;
  }
}

/** Adds a folder and scans it. */
export async function addCollectionRoot(path: string): Promise<void> {
  const s = store().settings;
  if (!s || s.roots.includes(path)) return;
  if (await saveSettings({ ...s, roots: [...s.roots, path] })) {
    await startCollectionScan(false);
  }
}

export async function removeCollectionRoot(path: string): Promise<void> {
  const s = store().settings;
  if (!s) return;
  if (await saveSettings({ ...s, roots: s.roots.filter((r) => r !== path) })) {
    await startCollectionScan(false);
  }
}

/** Where the engine has no trash: let Move to Trash delete for good. */
export async function setCollectionPermanentDelete(on: boolean): Promise<void> {
  const s = store().settings;
  if (s) await saveSettings({ ...s, allow_permanent_delete: on });
}

/** "Remove new sidecars after each scan". */
export async function setCollectionSweep(on: boolean): Promise<void> {
  const s = store().settings;
  if (s) await saveSettings({ ...s, sweep_sidecars: on });
}

export async function setCollectionRefresh(secs: number | null): Promise<void> {
  const s = store().settings;
  if (s) await saveSettings({ ...s, refresh_secs: secs });
}

/** Imports PS Game Library's index, then rescans to reconcile it with the disk. */
export async function importPsGameLibrary(): Promise<void> {
  store().set({ error: null, loading: true });
  try {
    await collection.importPsGameLibrary();
    const [settings, library] = await Promise.all([
      collection.settings(),
      collection.library(),
    ]);
    store().set({ settings, library, loading: false });
    if (settings.roots.length > 0) await startCollectionScan(false);
  } catch (e) {
    store().set({
      loading: false,
      error: e instanceof Error ? e.message : String(e),
    });
  }
}

export function collectionGames(
  lib: CollectionLibrary | null,
): CollectionGame[] {
  return lib ? Object.values(lib.games) : [];
}

/** Reads what `host` has for every game in the collection. */
export async function loadConsoleStates(host: string): Promise<void> {
  if (!host) return;
  const put = (c: Partial<ConsoleStates>) => {
    const prev = store().consoleByHost[host] ?? {
      states: {},
      loading: false,
      error: null,
    };
    store().set({
      consoleByHost: { ...store().consoleByHost, [host]: { ...prev, ...c } },
    });
  };
  put({ loading: true, error: null });
  try {
    const r = await collection.consoleState(mgmtAddr(host));
    put({
      loading: false,
      states: Object.fromEntries(r.games.map((g) => [g.game_id, g])),
    });
  } catch (e) {
    put({ loading: false, error: e instanceof Error ? e.message : String(e) });
  }
}
