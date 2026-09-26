import { beforeEach, describe, expect, it, vi } from "vitest";

const FAVORITES_KEY = "ps5upload.desktop-sidebar.favorites.v1";
const HINT_KEY = "ps5upload.desktop-sidebar.favorites-hint.v1";

// These tests run in the repo's plain-node environment (there is no jsdom),
// so `window.localStorage` has to be stood up by hand. A tiny in-memory Map
// is enough: `safeStorage` only ever calls get/set/remove, and using the real
// wrappers keeps the persistence path under test rather than mocked out.
function installStorage(seed: Record<string, string> = {}) {
  const map = new Map(Object.entries(seed));
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
      removeItem: (k: string) => void map.delete(k),
      clear: () => map.clear(),
    },
  };
  return map;
}

/** The store snapshots localStorage at module load, so a test that needs a
 *  specific starting state must seed storage and then re-import the module. */
async function freshStore() {
  vi.resetModules();
  const mod = await import("./navFavorites");
  return mod.useNavFavoritesStore;
}

beforeEach(() => {
  installStorage();
  vi.resetModules();
});

describe("navFavorites persistence", () => {
  it("writes the pinned list to storage so it survives a relaunch", async () => {
    const storage = installStorage();
    const store = await freshStore();

    store.getState().toggle("/games");
    store.getState().toggle("/files");

    expect(JSON.parse(storage.get(FAVORITES_KEY) ?? "null")).toEqual([
      "/games",
      "/files",
    ]);
    // Pinning a first screen answers the hint's question, so it retires
    // itself rather than making the user dismiss it as well.
    expect(storage.get(HINT_KEY)).toBe("1");
  });

  it("un-pins on a second toggle and persists the removal", async () => {
    const storage = installStorage({
      [FAVORITES_KEY]: JSON.stringify(["/games", "/files"]),
    });
    const store = await freshStore();

    store.getState().toggle("/games");

    expect(store.getState().favorites).toEqual(["/files"]);
    expect(JSON.parse(storage.get(FAVORITES_KEY) ?? "null")).toEqual([
      "/files",
    ]);
  });

  it("reads a previously saved list back at startup", async () => {
    // The regression this guards: Favorites was the one preference that was
    // never mirrored to settings.json, so it was the one users lost.
    installStorage({
      [FAVORITES_KEY]: JSON.stringify(["/games", "/files"]),
      [HINT_KEY]: "1",
    });
    const store = await freshStore();

    expect(store.getState().favorites).toEqual(["/games", "/files"]);
    expect(store.getState().hintDismissed).toBe(true);
  });

  it("setFavorites replaces the list and persists it", async () => {
    // This is the path the settings.json mirror hydrates through.
    const storage = installStorage({
      [FAVORITES_KEY]: JSON.stringify(["/old"]),
    });
    const store = await freshStore();

    store.getState().setFavorites(["/games", "/console"], true);

    expect(store.getState().favorites).toEqual(["/games", "/console"]);
    expect(store.getState().hintDismissed).toBe(true);
    expect(JSON.parse(storage.get(FAVORITES_KEY) ?? "null")).toEqual([
      "/games",
      "/console",
    ]);
  });

  it("setFavorites drops non-string entries from a hand-edited file", async () => {
    // settings.json is documented as hand-editable, so its array can hold
    // anything. A number in the list must not become a nav item.
    const store = await freshStore();

    store
      .getState()
      .setFavorites(["/games", 42, null, "/files"] as unknown as string[]);

    expect(store.getState().favorites).toEqual(["/games", "/files"]);
  });

  it("treats a corrupt stored value as no favorites rather than crashing", async () => {
    installStorage({ [FAVORITES_KEY]: "{not json" });
    const store = await freshStore();

    expect(store.getState().favorites).toEqual([]);
  });

  it("ignores a stored value that is valid JSON but not an array", async () => {
    installStorage({ [FAVORITES_KEY]: '"/games"' });
    const store = await freshStore();

    expect(store.getState().favorites).toEqual([]);
  });
});
