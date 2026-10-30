import { describe, expect, it } from "vitest";
import { DEFAULT_PICTURE, cropRect, needsPass, readPicture } from "./rpPicture";

describe("picture tools", () => {
  it("crops a 16:9 frame to its middle 21:9 band, or leaves it", () => {
    expect(cropRect(1920, 1080, true)).toEqual({ y: 129, h: 822 });
    expect(cropRect(1280, 720, true)).toEqual({ y: 86, h: 548 });
    expect(cropRect(1920, 1080, false)).toEqual({ y: 0, h: 1080 });
    // Already wider than 21:9: nothing to cut.
    expect(cropRect(2560, 800, true)).toEqual({ y: 0, h: 800 });
  });

  it("uses the GPU only for a tool that needs it (sharp pixels is CSS)", () => {
    expect(needsPass(DEFAULT_PICTURE)).toBe(false);
    expect(needsPass({ ...DEFAULT_PICTURE, pixelated: true })).toBe(false);
    expect(needsPass({ ...DEFAULT_PICTURE, sharpen: 10 })).toBe(true);
    expect(needsPass({ ...DEFAULT_PICTURE, crop219: true })).toBe(true);
  });

  it("reads a stored setting safely", () => {
    expect(readPicture(undefined)).toEqual(DEFAULT_PICTURE);
    expect(readPicture({ sharpen: 500, brightness: -99, contrast: "x", crop219: 1 })).toEqual({
      ...DEFAULT_PICTURE,
      sharpen: 100,
      brightness: -50,
    });
  });
});
