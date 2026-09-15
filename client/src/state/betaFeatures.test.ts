import { describe, expect, it, afterEach, vi } from "vitest";

import { useBetaFeaturesStore } from "./betaFeatures";

// vitest's default node env has no `window`; the store reads
// `window.localStorage`. Install a tiny in-memory stub (same pattern as
// installSettings.test.ts) so the real load branch runs.
function installWindowStub(seed?: Record<string, string>) {
  const store = new globalThis.Map<string, string>(
    seed ? Object.entries(seed) : [],
  );
  const localStorage = {
    getItem: (k: string) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k: string, v: string) => void store.set(k, String(v)),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  };
  (globalThis as { window?: unknown }).window = { localStorage };
}

const KEY = "ps5upload.beta_features";

/** The store reads localStorage at module-init, so a stored value has to be
 *  in place before the import — hence the dynamic import per case. */
async function freshStore() {
  vi.resetModules();
  return (await import("./betaFeatures")).useBetaFeaturesStore;
}

describe("betaFeatures — OFF unless switched on", () => {
  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
    vi.resetModules();
  });

  // The default is opt-IN. Beta screens are by definition not reliable, so a
  // user who never touches the setting must never see one. Pin it: flipping
  // `=== "on"` to `!== "off"` in the loader would silently ship a half-built
  // screen to everyone, and nothing else in the app would notice.
  it("defaults OFF when nothing is stored", async () => {
    installWindowStub();
    const store = await freshStore();
    expect(store.getState().enabled).toBe(false);
  });

  it("stays OFF for any stored value that is not exactly 'on'", async () => {
    for (const value of ["off", "false", "true", "1", "0", ""]) {
      installWindowStub({ [KEY]: value });
      const store = await freshStore();
      expect(store.getState().enabled, `stored ${JSON.stringify(value)}`).toBe(
        false,
      );
    }
  });

  it("is OFF when there is no window at all", () => {
    // The static import at the top of this file was evaluated with no
    // `window` — that is the SSR/test path, and it must not throw or opt in.
    expect(useBetaFeaturesStore.getState().enabled).toBe(false);
  });

  it("loads ON only for an explicit 'on'", async () => {
    installWindowStub({ [KEY]: "on" });
    const store = await freshStore();
    expect(store.getState().enabled).toBe(true);
  });
});

describe("betaFeatures — the switch persists", () => {
  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
    vi.resetModules();
  });

  it("writes 'on'/'off' and mirrors the state", () => {
    installWindowStub();
    useBetaFeaturesStore.getState().setEnabled(true);
    expect(window.localStorage.getItem(KEY)).toBe("on");
    expect(useBetaFeaturesStore.getState().enabled).toBe(true);

    useBetaFeaturesStore.getState().setEnabled(false);
    expect(window.localStorage.getItem(KEY)).toBe("off");
    expect(useBetaFeaturesStore.getState().enabled).toBe(false);
  });

  it("notifies subscribers so the nav can re-render", () => {
    installWindowStub();
    const seen: boolean[] = [];
    const stop = useBetaFeaturesStore.subscribe((s) => seen.push(s.enabled));
    useBetaFeaturesStore.getState().setEnabled(true);
    useBetaFeaturesStore.getState().setEnabled(false);
    stop();
    expect(seen).toEqual([true, false]);
  });
});
