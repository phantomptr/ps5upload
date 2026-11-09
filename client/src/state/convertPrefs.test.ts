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

  it("remembers Stored across loads", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().setCompression("stored");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().compression).toBe("stored");
  });

  it("a preset image choice shows now and is not remembered", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().showMake("image");
    first.useConvertPrefs.getState().showImageChoice("exfat", true);
    expect(first.useConvertPrefs.getState().imageCompress).toBe(true);
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().imageCompress).toBe(false);
    expect(second.useConvertPrefs.getState().make).not.toBe("image");
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

  it("remembers where outputs go: the Output folder by default, or next to each source", async () => {
    const first = await import("./convertPrefs");
    expect(first.useConvertPrefs.getState().outputPlace).toBe("folder");
    first.useConvertPrefs.getState().setOutputPlace("beside");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().outputPlace).toBe("beside");
    stored.set("ps5upload.convert.output_place", "junk");
    vi.resetModules();
    const third = await import("./convertPrefs");
    expect(third.useConvertPrefs.getState().outputPlace).toBe("folder");
  });

  it("#780: picking an Output folder turns off next-to-its-source, so the folder is used", async () => {
    const { useConvertPrefs } = await import("./convertPrefs");
    const { outputFor } = await import("../lib/outputPlace");
    useConvertPrefs.getState().setOutputPlace("beside");
    useConvertPrefs.getState().setOutputDir("/out");
    const s = useConvertPrefs.getState();
    expect(s.outputPlace).toBe("folder");
    expect(stored.get("ps5upload.convert.output_place")).toBe("folder");
    expect(outputFor("/games/Astro Bot", s.outputPlace, s.outputDir)).toEqual({
      outputDir: "/out",
      keepBoth: false,
      beside: false,
    });
    // Clearing the field leaves the choice as it is; next-to-source can be turned on again.
    useConvertPrefs.getState().setOutputPlace("beside");
    useConvertPrefs.getState().setOutputDir("");
    expect(useConvertPrefs.getState().outputPlace).toBe("beside");
  });

  it("remembers the .ffpfsc level, Balanced by default and for a corrupt value", async () => {
    const first = await import("./convertPrefs");
    expect(first.useConvertPrefs.getState().imageLevel).toBe("balanced");
    first.useConvertPrefs.getState().setImageLevel("fast");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().imageLevel).toBe("fast");
    expect(second.parseImageLevel("turbo")).toBe("balanced");
    expect(second.parseImageLevel("smallest")).toBe("smallest");
  });

  it("ignores a corrupt stored level", async () => {
    stored.set("ps5upload.convert.compression", "turbo");
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().compression).toBe("balanced");
  });

  it("defaults to the fast check", async () => {
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().fullVerify).toBe(false);
  });

  it("remembers full verification across loads", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().setFullVerify(true);
    expect(stored.get("ps5upload.convert.full_verify")).toBe("1");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().fullVerify).toBe(true);
    second.useConvertPrefs.getState().setFullVerify(false);
    expect(stored.get("ps5upload.convert.full_verify")).toBe("0");
  });

  it("reads anything but 1 as the fast check", async () => {
    const { parseFullVerify } = await import("./convertPrefs");
    expect(parseFullVerify("1")).toBe(true);
    for (const v of [null, "", "0", "true", "yes"]) expect(parseFullVerify(v)).toBe(false);
  });

  it("remembers a folder as what to make, and reads anything else as a package", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().setMake("folder");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().make).toBe("folder");
    expect(second.parseMake("image")).toBe("image");
    for (const v of [null, "", "dir", "pkg"]) expect(second.parseMake(v)).toBe("pkg");
  });

  it("shows a one-off make without remembering it", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().setMake("image");
    first.useConvertPrefs.getState().showMake("folder");
    expect(first.useConvertPrefs.getState().make).toBe("folder");
    expect(stored.get("ps5upload.convert.make")).toBe("image");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().make).toBe("image");
  });

  it("remembers the zstd pack runtime: the newest release by default and for a corrupt value", async () => {
    const first = await import("./convertPrefs");
    expect(first.useConvertPrefs.getState().zstdRuntime).toBe("latest");
    first.useConvertPrefs.getState().setZstdRuntime("pinned");
    vi.resetModules();
    const second = await import("./convertPrefs");
    expect(second.useConvertPrefs.getState().zstdRuntime).toBe("pinned");
    stored.set("ps5upload.convert.zstd_runtime", "v9");
    vi.resetModules();
    const third = await import("./convertPrefs");
    expect(third.useConvertPrefs.getState().zstdRuntime).toBe("latest");
  });
});
