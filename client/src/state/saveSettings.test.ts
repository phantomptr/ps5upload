import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { useSaveSettingsStore, normalizeSavePath, DEFAULT_SAVE_PATH } from "./saveSettings";

// vitest's default node env has no `window`; the store reads
// `window.localStorage`. Install a tiny in-memory stub (same pattern as
// installSettings.test.ts) so the real persist branches run.
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

const KEY = "ps5upload.save_path";

describe("normalizeSavePath", () => {
  it("trims whitespace", () => {
    expect(normalizeSavePath("  /mnt/usb0/savedata  ")).toBe(
      "/mnt/usb0/savedata",
    );
  });

  it("strips trailing slashes", () => {
    expect(normalizeSavePath("/mnt/usb0/savedata/")).toBe(
      "/mnt/usb0/savedata",
    );
    expect(normalizeSavePath("/mnt/usb0/savedata///")).toBe(
      "/mnt/usb0/savedata",
    );
  });

  it("falls back to the default when empty or whitespace-only", () => {
    expect(normalizeSavePath("")).toBe(DEFAULT_SAVE_PATH);
    expect(normalizeSavePath("   ")).toBe(DEFAULT_SAVE_PATH);
  });

  it("leaves an already-normalized custom path untouched", () => {
    expect(normalizeSavePath("/mnt/ext0/backups")).toBe("/mnt/ext0/backups");
  });
});

describe("saveSettings — savePath", () => {
  beforeEach(() => {
    installWindowStub();
  });

  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
    vi.resetModules();
  });

  it("defaults to DEFAULT_SAVE_PATH when the key is absent", async () => {
    installWindowStub();
    vi.resetModules();
    const mod = await import("./saveSettings");
    expect(mod.useSaveSettingsStore.getState().savePath).toBe(
      DEFAULT_SAVE_PATH,
    );
  });

  it("loads and normalizes a previously stored value", async () => {
    installWindowStub({ [KEY]: "/mnt/usb1/savedata/" });
    vi.resetModules();
    const mod = await import("./saveSettings");
    expect(mod.useSaveSettingsStore.getState().savePath).toBe(
      "/mnt/usb1/savedata",
    );
  });

  it("setter persists the normalized value and mirrors state", () => {
    const s = useSaveSettingsStore.getState();
    s.setSavePath("/mnt/ext0/backups/");
    expect(window.localStorage.getItem(KEY)).toBe("/mnt/ext0/backups");
    expect(useSaveSettingsStore.getState().savePath).toBe(
      "/mnt/ext0/backups",
    );
  });

  it("setter falls back to the default for an empty value", () => {
    const s = useSaveSettingsStore.getState();
    s.setSavePath("   ");
    expect(window.localStorage.getItem(KEY)).toBe(DEFAULT_SAVE_PATH);
    expect(useSaveSettingsStore.getState().savePath).toBe(DEFAULT_SAVE_PATH);
  });
});
