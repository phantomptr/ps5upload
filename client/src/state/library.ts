import { create } from "zustand";
import {
  appsInstalled,
  fetchVolumes,
  scanLibraryChecked,
  type LibraryEntry,
  type Volume,
} from "../api/ps5";
import { hostOf, transferAddr } from "../lib/addr";
import { imageBasename, smpMountImageBasename } from "../lib/mountPaths";

/** What the console knows about a title we registered from a given source
 *  path. A `.exfat`/`.ffpkg` row has no readable `sce_sys/` of its own (the
 *  metadata lives *inside* the image), so this is the only way to put a real
 *  name and cover on a disk-image row without mounting it first. */
export interface RegisteredTitle {
  titleId: string;
  titleName: string;
}

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
  /** The last scan could not read some folders, so `entries` may be missing games. */
  incomplete: boolean;
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
  /** source_path → the title registered from it, from the console's
   *  /user/appmeta inventory. Keyed by the SOURCE path (not the mount point)
   *  so a disk-image row can look itself up directly. Empty when the probe
   *  failed or the payload is too old to report the registered set — rows
   *  then fall back to the filename, exactly as before. */
  registeredBySource: Map<string, RegisteredTitle>;
  /** Title IDs (upper case) the console has installed from a package: a game folder of one of
   *  them is a second copy of an installed game. */
  pkgInstalled: Set<string>;
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
  incomplete: false,
  mountMap: new Map(),
  pendingMounts: new Map(),
  volumes: [],
  registeredBySource: new Map(),
  pkgInstalled: new Set(),
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
    registeredBySource: Map<string, RegisteredTitle>,
    pkgInstalled?: Set<string>,
    /** Some of the console could not be read: what is missing is unknown, not gone. */
    incomplete?: boolean,
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
    setData: (host, entries, mountMap, volumes, registeredBySource, pkgInstalled, incomplete = false) =>
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
        // An empty scan that could not read everything keeps the old list (the "library goes
        // blank after mounting" symptom); one that read everything means there is nothing, as
        // after deleting the last game or ejecting the only drive with games.
        const hadEntries = (cur.entries ?? []).length > 0;
        // A scan that could not read some folders says nothing about what it did not find, so
        // earlier rows stay until a full scan: titles must not vanish on a helper blip.
        const seen = new Set(entries.map((e) => e.path));
        const nextEntries =
          entries.length === 0 && hadEntries && incomplete
            ? cur.entries
            : incomplete
              ? [...entries, ...(cur.entries ?? []).filter((e) => !seen.has(e.path))]
              : entries;
        return {
          byHost: {
            ...s.byHost,
            [key]: {
              ...cur,
              entries: nextEntries,
              incomplete,
              mountMap,
              pendingMounts: prunedPending,
              volumes,
              registeredBySource,
              pkgInstalled: pkgInstalled ?? cur.pkgInstalled,
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

/** How old a console's scan may be before a screen that shows it scans again. */
export const LIBRARY_STALE_MS = 30_000;

/** One scan per console at a time: a second caller shares the first one's. */
const inflight = new Map<string, Promise<void>>();

/** The console's scan is missing or older than LIBRARY_STALE_MS. */
export function libraryIsStale(host: string, now = Date.now()): boolean {
  const at = libraryForHost(useLibraryStore.getState(), host).lastRefreshedAt;
  return at === null || now - at > LIBRARY_STALE_MS;
}

/**
 * Scan one console's drives for game folders and disk images, and commit the result to its
 * slot (the Games grid, every game page and the not-linked list read it).
 *
 * Alongside the walk it reads the volumes (the Move dialog's destinations and the image →
 * mount map) and the registered titles (what puts a name on an image row, and which folders
 * are second copies of a package install). Both are non-fatal: without them the rows fall
 * back to file names and the Move dialog offers no destinations.
 *
 * The result is written to the slot of the console that was scanned, never "the current one",
 * so a scan that lands after a console switch cannot paint one PS5's files under another's name.
 */
/** A scan to run after a change here (a delete, a move): one in flight may have read the console
 *  before the change and would paint the row back, so one more runs after it. */
export function refreshLibraryAfterChange(host: string): Promise<void> {
  const running = inflight.get(keyOf(host));
  return running ? running.then(() => refreshLibrary(host)) : refreshLibrary(host);
}

export function refreshLibrary(host: string): Promise<void> {
  const key = keyOf(host);
  if (!key) return Promise.resolve();
  const running = inflight.get(key);
  if (running) return running;
  const store = useLibraryStore.getState();
  const run = (async () => {
    store.setLoading(host, true);
    store.setError(host, null);
    try {
      const addr = transferAddr(host);
      const [result, volumes, registered] = await Promise.all([
        scanLibraryChecked(addr),
        fetchVolumes(addr).catch((e) => {
          console.warn(
            "[library] volume probe failed; Move modal will offer no destinations:",
            e,
          );
          return [] as Volume[];
        }),
        appsInstalled(addr).catch((e) => {
          console.warn(
            "[library] installed-apps probe failed; disk-image rows will show filenames only:",
            e,
          );
          return { titles: [], registeredUnavailable: true, incomplete: true };
        }),
      ]);
      // Two ways a registered title points back at a row:
      //   1. its source IS the row's path: folder games, and any image ps5upload mounted itself.
      //   2. its source is a ShadowMount+ mount point, which encodes only the image's basename.
      // Both go into one map; images look themselves up under both keys.
      const registeredBySource = new Map<string, RegisteredTitle>();
      const titles = registered.titles ?? [];
      const pkgInstalled = new Set(
        titles
          .filter((t) => t.origin === "pkg")
          .map((t) => t.titleId.toUpperCase()),
      );
      for (const t of titles) {
        // Only registered titles carry a source path; a .pkg install has none.
        if (!t.source) continue;
        const title = { titleId: t.titleId, titleName: t.titleName };
        registeredBySource.set(t.source, title);
        const smpBasename = smpMountImageBasename(t.source);
        if (smpBasename) registeredBySource.set(smpBasename, title);
      }
      // image_path → mount_point, for any mount point (the payload's tracker still decides
      // what Unmount may touch).
      const mountMap = new Map<string, string>();
      for (const v of volumes) {
        if (v.source_image) mountMap.set(v.source_image, v.path);
      }
      // Move destinations: real attached drives only.
      const writable = volumes.filter((v) => v.writable && !v.is_placeholder);
      useLibraryStore
        .getState()
        .setData(host, result.entries, mountMap, writable, registeredBySource, pkgInstalled, result.incomplete);
    } catch (e) {
      useLibraryStore
        .getState()
        .setError(host, e instanceof Error ? e.message : String(e));
    } finally {
      useLibraryStore.getState().setLoading(host, false);
      inflight.delete(key);
    }
  })();
  inflight.set(key, run);
  return run;
}

/** The title ids a console's drives hold, upper case: game folders by their own id, images by
 *  the title registered from them. */
export function driveTitleIds(slot: LibrarySlot): Set<string> {
  const out = new Set<string>();
  for (const e of slot.entries ?? []) {
    const id = entryTitleId(e, slot);
    if (id) out.add(id.toUpperCase());
  }
  return out;
}

/** The title a drive entry belongs to, or null when nothing links it to one: a folder's own
 *  id; for an image, the title registered from it (by its path, or the basename ShadowMount+
 *  names its mount after), else the game folder inside it while it is mounted. */
export function entryTitleId(
  e: LibraryEntry,
  slot: Pick<LibrarySlot, "registeredBySource" | "entries" | "mountMap" | "pendingMounts">,
): string | null {
  if (e.titleId) return e.titleId;
  if (e.kind !== "image") return null;
  const registered =
    slot.registeredBySource.get(e.path)?.titleId ??
    slot.registeredBySource.get(imageBasename(e.path))?.titleId;
  if (registered) return registered;
  const mount = slot.mountMap.get(e.path) ?? slot.pendingMounts.get(e.path);
  if (!mount) return null;
  const root = mount.endsWith("/") ? mount : mount + "/";
  const inner = (slot.entries ?? []).find(
    (x) => x.kind === "game" && !!x.titleId && (x.path === mount || x.path.startsWith(root)),
  );
  return inner?.titleId ?? null;
}
