import { beforeEach, describe, expect, it, vi } from "vitest";

// vitest's node env has no `window`; the store reads `window.localStorage` through
// safeStorage. One in-memory stub, kept across module reloads, as a page reload keeps storage.
const stored = new globalThis.Map<string, string>();
(globalThis as { window?: unknown }).window = {
  localStorage: {
    getItem: (k: string) => (stored.has(k) ? (stored.get(k) as string) : null),
    setItem: (k: string, v: string) => void stored.set(k, String(v)),
    removeItem: (k: string) => void stored.delete(k),
  },
};

describe("convertPrefs", () => {
  beforeEach(() => {
    stored.clear();
    vi.resetModules();
  });

  it("defaults to no folder and Balanced", async () => {
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().outputDir).toBe("");
    expect(useConvertPrefs.getState().compression).toBe("balanced");
  });

  it("remembers the folder and level across loads", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().setOutputDir("/Volumes/Gone/fpkg");
    first.useConvertPrefs.getState().setCompression("smallest");
    vi.resetModules();
    const second = await import("./convertPrefs");
    // A remembered folder that no longer exists still loads as-is: the engine's check
    // reports it, the screen does not crash.
    expect(second.useConvertPrefs.getState().outputDir).toBe("/Volumes/Gone/fpkg");
    expect(second.useConvertPrefs.getState().compression).toBe("smallest");
  });

  it("ignores a corrupt stored level", async () => {
    stored.set("ps5upload.convert.compression", "turbo");
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().compression).toBe("balanced");
  });
});
