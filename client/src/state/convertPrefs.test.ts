import { beforeEach, describe, expect, it, vi } from "vitest";

describe("convertPrefs", () => {
  beforeEach(() => {
    localStorage.clear();
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
    localStorage.setItem("ps5upload.convert.compression", "turbo");
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().compression).toBe("balanced");
  });
});
