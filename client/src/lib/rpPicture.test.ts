import { describe, expect, it } from "vitest";
import {
  DEFAULT_PICTURE,
  cropRect,
  isPlaneFormat,
  needsPass,
  planeLayout,
  rcasStops,
  readPicture,
  sharpens,
  upscaleSize,
  upscales,
  yuvToRgb,
} from "./rpPicture";

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
    // A strength alone does nothing: Sharpening and upscaling is off.
    expect(needsPass({ ...DEFAULT_PICTURE, sharpen: 10 })).toBe(false);
    expect(needsPass({ ...DEFAULT_PICTURE, enhance: "sharpen" })).toBe(true);
    expect(needsPass({ ...DEFAULT_PICTURE, crop219: true })).toBe(true);
  });

  it("reads a stored setting safely", () => {
    expect(readPicture(undefined)).toEqual(DEFAULT_PICTURE);
    expect(readPicture({ enhance: "sharpen", sharpen: 500, brightness: -99, contrast: "x", crop219: 1 })).toEqual({
      ...DEFAULT_PICTURE,
      enhance: "sharpen",
      sharpen: 100,
      brightness: -50,
    });
    expect(readPicture({ enhance: "turbo" }).enhance).toBe("off");
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
    expect(DEFAULT_PICTURE.enhance).toBe("off");
    expect(readPicture({ enhance: "fsr" }).enhance).toBe("fsr");
    expect(upscales({ ...DEFAULT_PICTURE, enhance: "fsr" })).toBe(true);
    expect(upscales({ ...DEFAULT_PICTURE, enhance: "sharpen" })).toBe(false);
    expect(upscales({ ...DEFAULT_PICTURE, enhance: "fsr", pixelated: true })).toBe(false);
    expect(sharpens({ ...DEFAULT_PICTURE, enhance: "fsr", pixelated: true })).toBe(false);
    expect(needsPass({ ...DEFAULT_PICTURE, enhance: "fsr", pixelated: true })).toBe(false);
  });

  it("keeps the look of settings saved before Sharpening and upscaling was one choice", () => {
    // The Upscale switch was FSR (with its sharpening from the Sharpen slider).
    expect(readPicture({ upscale: true, sharpen: 40 })).toMatchObject({ enhance: "fsr", sharpen: 40 });
    // A Sharpen amount alone was sharpening at the picture's size.
    expect(readPicture({ upscale: false, sharpen: 30 })).toMatchObject({ enhance: "sharpen", sharpen: 30 });
    expect(readPicture({ upscale: "yes", sharpen: 0 }).enhance).toBe("off");
    expect("upscale" in readPicture({ upscale: true })).toBe(false);
  });

  it("sets RCAS from mild (0) to strongest (100)", () => {
    expect(rcasStops(0)).toBe(2);
    expect(rcasStops(50)).toBe(1);
    expect(rcasStops(100)).toBe(0);
    expect(rcasStops(400)).toBe(0);
  });
});

describe("YUV planes", () => {
  it("packs NV12 and I420 planes tightly, chroma rounded up for odd sizes", () => {
    expect(planeLayout("NV12", 1920, 1080)).toEqual({
      layout: [
        { offset: 0, stride: 1920 },
        { offset: 1920 * 1080, stride: 1920 },
      ],
      size: 1920 * 1080 * 1.5,
    });
    expect(planeLayout("I420", 1920, 1080)).toEqual({
      layout: [
        { offset: 0, stride: 1920 },
        { offset: 1920 * 1080, stride: 960 },
        { offset: 1920 * 1080 + 960 * 540, stride: 960 },
      ],
      size: 1920 * 1080 * 1.5,
    });
    expect(planeLayout("NV12", 5, 3)).toEqual({
      layout: [
        { offset: 0, stride: 5 },
        { offset: 15, stride: 6 },
      ],
      size: 15 + 6 * 2,
    });
  });

  it("reads only NV12 and I420 as planes", () => {
    expect(isPlaneFormat("NV12")).toBe(true);
    expect(isPlaneFormat("I420")).toBe(true);
    expect(isPlaneFormat("RGBA")).toBe(false);
    expect(isPlaneFormat("I420P10")).toBe(false);
    expect(isPlaneFormat(null)).toBe(false);
  });

  /** rgb (0-255) for yuv (0-255) through the matrix, as the shader does. */
  const rgb = (c: ReturnType<typeof yuvToRgb>, y: number, u: number, v: number) => {
    const d = [y / 255 - c.off[0], u / 255 - c.off[1], v / 255 - c.off[2]];
    return [0, 1, 2].map((r) => Math.round(255 * (c.m[r] * d[0] + c.m[3 + r] * d[1] + c.m[6 + r] * d[2])));
  };

  it("maps limited-range black and white to 0 and 255, any matrix", () => {
    for (const matrix of ["bt709", "smpte170m", "bt2020-ncl", null]) {
      const c = yuvToRgb({ matrix, fullRange: false });
      expect(rgb(c, 16, 128, 128)).toEqual([0, 0, 0]);
      expect(rgb(c, 235, 128, 128)).toEqual([255, 255, 255]);
    }
    const full = yuvToRgb({ matrix: "bt709", fullRange: true });
    expect(rgb(full, 0, 128, 128)).toEqual([0, 0, 0]);
    expect(rgb(full, 255, 128, 128)).toEqual([255, 255, 255]);
  });

  it("uses BT.709 unless the frame says BT.601, and limited range unless it says full", () => {
    // A pixel of the PS5's own stream (BT.709 limited): reference values from the standard.
    expect(rgb(yuvToRgb({ matrix: "bt709", fullRange: false }), 96, 104, 147)).toEqual([127, 88, 42]);
    expect(rgb(yuvToRgb(null), 96, 104, 147)).toEqual([127, 88, 42]);
    expect(rgb(yuvToRgb({ matrix: "smpte170m", fullRange: false }), 96, 104, 147)).toEqual([123, 87, 45]);
    expect(rgb(yuvToRgb({ matrix: "bt470bg" }), 96, 104, 147)).toEqual([123, 87, 45]);
    // Limited-range red (BT.709), to within the 8-bit code values' rounding.
    const red = rgb(yuvToRgb({ matrix: "bt709" }), 63, 102, 240);
    [255, 0, 0].forEach((v, i) => expect(Math.abs(red[i] - v)).toBeLessThanOrEqual(1));
  });
});
