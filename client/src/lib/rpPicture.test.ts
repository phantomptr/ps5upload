import { describe, expect, it } from "vitest";
import { DEFAULT_PICTURE, cropRect, needsPass, readPicture, upscaleSize, upscales } from "./rpPicture";

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

describe("upscaling", () => {
  const hd = { w: 1920, h: 1080 };

  it("upscales to the picture's box on screen when that is bigger", () => {
    expect(upscaleSize(hd, { w: 3840, h: 2160 })).toEqual({ w: 3840, h: 2160 });
    expect(upscaleSize({ w: 1280, h: 720 }, { w: 2560, h: 1440 })).toEqual({ w: 2560, h: 1440 });
    // A stretched box: each side its own.
    expect(upscaleSize({ w: 1280, h: 720 }, { w: 2560, h: 1080 })).toEqual({ w: 2560, h: 1080 });
  });

  it("does nothing when the box is not bigger, or not known", () => {
    expect(upscaleSize(hd, { w: 1920, h: 1080 })).toBeNull();
    expect(upscaleSize(hd, { w: 2000, h: 1100 })).toBeNull();
    expect(upscaleSize(hd, { w: 1280, h: 720 })).toBeNull();
    expect(upscaleSize(hd, null)).toBeNull();
    expect(upscaleSize({ w: 0, h: 0 }, { w: 3840, h: 2160 })).toBeNull();
  });

  it("goes at most 3x per side and 4K in all, never below the picture", () => {
    expect(upscaleSize({ w: 1280, h: 720 }, { w: 7680, h: 4320 })).toEqual({ w: 3840, h: 2160 });
    const fill = upscaleSize(hd, { w: 7680, h: 2160 })!;
    expect(fill.w * fill.h).toBeLessThanOrEqual(3840 * 2160 + 4000);
    expect(fill.w).toBeGreaterThanOrEqual(1920);
    expect(fill.h).toBeGreaterThanOrEqual(1080);
    expect(upscaleSize({ w: 640, h: 360 }, { w: 3840, h: 2160 })).toEqual({ w: 1920, h: 1080 });
  });

  it("is a stored setting, off by default, and gives way to sharp pixels", () => {
    expect(DEFAULT_PICTURE.upscale).toBe(false);
    expect(readPicture({ upscale: true }).upscale).toBe(true);
    expect(readPicture({ upscale: "yes" }).upscale).toBe(false);
    expect(needsPass({ ...DEFAULT_PICTURE, upscale: true })).toBe(true);
    expect(upscales({ ...DEFAULT_PICTURE, upscale: true, pixelated: true })).toBe(false);
    expect(needsPass({ ...DEFAULT_PICTURE, upscale: true, pixelated: true })).toBe(false);
  });
});
