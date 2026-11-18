import { describe, expect, it } from "vitest";
import {
  chromaBlank,
  DEFAULT_PICTURE,
  PicturePass,
  cropRect,
  isPlaneFormat,
  needsPass,
  PlaneReader,
  planeLayout,
  rcasStops,
  srcRect,
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

describe("PicturePass.release", () => {
  /** A WebGL 2 context that answers everything and records what was called. */
  function fakeGl() {
    const calls: string[] = [];
    let lost = false;
    const ext = { loseContext: () => ((lost = true), calls.push("loseContext")) };
    const gl = new Proxy({} as Record<string, unknown>, {
      get(_t, k: string) {
        if (k === "isContextLost") return () => lost;
        if (k === "getExtension") return (n: string) => (n === "WEBGL_lose_context" ? ext : null);
        if (/^[A-Z_0-9]+$/.test(k)) return 1;
        return (..._a: unknown[]) => {
          calls.push(k);
          return k.startsWith("get") && k.endsWith("Parameter") ? true : {};
        };
      },
    });
    return { gl, calls };
  }

  it("frees its GPU objects and gives the context back at once", () => {
    const { gl, calls } = fakeGl();
    const canvas = { width: 0, height: 0, getContext: () => gl } as unknown as HTMLCanvasElement;
    const pass = new PicturePass(canvas);
    pass.release();
    expect(calls).toContain("deleteTexture");
    expect(calls).toContain("deleteProgram");
    expect(calls[calls.length - 1]).toBe("loseContext");
    expect(pass.lost).toBe(true);
    // Twice is harmless (a stream that ends after its canvas was replaced).
    pass.release();
    expect(calls.filter((c) => c === "loseContext")).toHaveLength(1);
  });
});

describe("chromaBlank (#656 green picture)", () => {
  const frame = (format: "NV12" | "I420", fill: number) => {
    const { layout, size } = planeLayout(format, 64, 36);
    const data = new Uint8Array(size);
    data.fill(fill, layout[1].offset);
    return { format, w: 64, h: 36, vis: { x: 0, y: 0, w: 64, h: 36 }, complete: true, data, layout, colorSpace: null };
  };
  it("is true when the copy left the chroma at zero", () => {
    expect(chromaBlank(frame("NV12", 0))).toBe(true);
    expect(chromaBlank(frame("I420", 0))).toBe(true);
  });
  it("is false for a real (even black) picture", () => {
    expect(chromaBlank(frame("NV12", 128))).toBe(false);
    expect(chromaBlank(frame("I420", 128))).toBe(false);
  });
});

/** A VideoFrame whose copyTo writes the requested rect's planes, all but the last `short`
 *  rows of each (0: all of them), the way a Linux app's 1080p copy came back. */
function fakeFrame(format: "NV12" | "I420", cw: number, ch: number, vh: number, short: number) {
  const calls: Array<{ rect: { x: number; y: number; width: number; height: number }; layout: Array<{ offset: number; stride: number }> }> = [];
  const frame = {
    format,
    codedWidth: cw,
    codedHeight: ch,
    visibleRect: { x: 0, y: 0, width: cw, height: vh },
    colorSpace: null,
    async copyTo(buf: Uint8Array, o: { rect: { x: number; y: number; width: number; height: number }; layout: Array<{ offset: number; stride: number }> }) {
      calls.push(o);
      const w = o.rect.width;
      const h = o.rect.height;
      const c = Math.ceil(w / 2);
      const planes = format === "NV12" ? [{ rw: w, rh: h }, { rw: c * 2, rh: Math.ceil(h / 2) }] : [{ rw: w, rh: h }, { rw: c, rh: Math.ceil(h / 2) }, { rw: c, rh: Math.ceil(h / 2) }];
      planes.forEach((pl, i) => {
        const rows = short && h > 2 * short ? pl.rh - Math.ceil(short / (i ? 2 : 1)) : pl.rh;
        for (let r = 0; r < rows; r++) buf.fill(100, o.layout[i].offset + r * o.layout[i].stride, o.layout[i].offset + r * o.layout[i].stride + pl.rw);
      });
    },
  };
  return { frame: frame as unknown as VideoFrame, calls };
}

describe("PlaneReader end marks (green band under a 1080p picture)", () => {
  it("a copy that fills every row is complete", async () => {
    for (const format of ["NV12", "I420"] as const) {
      const { frame } = fakeFrame(format, 1920, 1088, 1080, 0);
      const p = await new PlaneReader().read(frame);
      expect(p?.complete).toBe(true);
      expect(p?.h).toBe(1080);
    }
  });
  it("a copy that leaves the last rows empty is not, even with stale rows from the last frame", async () => {
    const r = new PlaneReader();
    const { frame } = fakeFrame("NV12", 1920, 1088, 1080, 0);
    await r.read(frame);
    const short = fakeFrame("NV12", 1920, 1088, 1080, 68);
    expect((await r.read(short.frame))?.complete).toBe(false);
  });
  it("coded mode copies the whole coded frame and keeps the visible part", async () => {
    const r = new PlaneReader();
    r.mode = "coded";
    const { frame, calls } = fakeFrame("NV12", 1920, 1088, 1080, 0);
    const p = await r.read(frame);
    expect(calls[0].rect).toEqual({ x: 0, y: 0, width: 1920, height: 1088 });
    expect(p).toMatchObject({ w: 1920, h: 1088, vis: { x: 0, y: 0, w: 1920, h: 1080 }, complete: true });
    expect(PlaneReader.codedDiffers(frame)).toBe(true);
  });
  // The picture is drawn with one coordinate for luma and chroma, and the chroma planes are read
  // with linear filtering: the last visible luma row (1079) reads chroma a quarter of the way
  // into row 540, which a coded copy holds as padding (whatever the encoder left there; zeros
  // are green). A visible copy ends at row 539, and the texture's edge repeats it.
  it("coded mode repeats the picture's last chroma row and column into the padding next to them", async () => {
    for (const format of ["NV12", "I420"] as const) {
      const r = new PlaneReader();
      r.mode = "coded";
      // 16×16 coded, 12×10 visible at (2, 2): chroma 8×8, the picture's chroma columns 1-6, rows 1-5.
      const frame = {
        format,
        codedWidth: 16,
        codedHeight: 16,
        visibleRect: { x: 2, y: 2, width: 12, height: 10 },
        colorSpace: null,
        async copyTo(buf: Uint8Array, o: { layout: Array<{ offset: number; stride: number }> }) {
          const px = format === "NV12" ? 2 : 1;
          buf.fill(200, 0, 256);
          o.layout.slice(1).forEach((l) => {
            for (let y = 0; y < 8; y++)
              for (let x = 0; x < 8; x++) {
                const inside = x >= 1 && x <= 6 && y >= 1 && y <= 5;
                for (let b = 0; b < px; b++) buf[l.offset + y * l.stride + x * px + b] = inside ? 100 + y * 8 + x + b * 50 : 0;
              }
          });
        },
      } as unknown as VideoFrame;
      const p = (await r.read(frame))!;
      expect(p.complete).toBe(true);
      const px = format === "NV12" ? 2 : 1;
      const at = (l: { offset: number; stride: number }, x: number, y: number, b = 0) => p.data[l.offset + y * l.stride + x * px + b];
      for (const l of p.layout.slice(1)) {
        for (let b = 0; b < px; b++) {
          // Below and above the picture: the row next to it.
          for (let x = 0; x < 8; x++) {
            expect(at(l, x, 6, b)).toBe(at(l, x, 5, b));
            expect(at(l, x, 0, b)).toBe(at(l, x, 1, b));
          }
          // Right and left of it: the column next to it.
          for (let y = 0; y <= 6; y++) {
            expect(at(l, 7, y, b)).toBe(at(l, 6, y, b));
            expect(at(l, 0, y, b)).toBe(at(l, 1, y, b));
          }
          // The picture itself is as copied.
          expect(at(l, 3, 3, b)).toBe(100 + 3 * 8 + 3 + b * 50);
          expect(at(l, 6, 5, b)).toBe(100 + 5 * 8 + 6 + b * 50);
        }
      }
    }
  });
  it("visible mode asks for the visible rect", async () => {
    const { frame, calls } = fakeFrame("I420", 1280, 720, 720, 0);
    await new PlaneReader().read(frame);
    expect(calls[0].rect).toEqual({ x: 0, y: 0, width: 1280, height: 720 });
    expect(PlaneReader.codedDiffers(frame)).toBe(false);
  });
});

describe("srcRect", () => {
  const planes = (w: number, h: number, vh: number) => {
    const { layout, size } = planeLayout("NV12", w, h);
    return { format: "NV12" as const, w, h, vis: { x: 0, y: 0, w, h: vh }, complete: true, data: new Uint8Array(size), layout, colorSpace: null };
  };
  it("reads the whole texture for visible planes", () => {
    expect(srcRect(planes(1920, 1080, 1080), 1080, { y: 0, h: 1080 })).toEqual([0, 0, 1, 1]);
  });
  it("reads only the visible rows of a coded copy", () => {
    const [x, y, w, h] = srcRect(planes(1920, 1088, 1080), 1080, { y: 0, h: 1080 });
    expect([x, y, w]).toEqual([0, 0, 1]);
    expect(h).toBeCloseTo(1080 / 1088);
  });
  it("keeps a 21:9 crop within the visible rows", () => {
    const crop = cropRect(1920, 1080, true);
    const [, y, , h] = srcRect(planes(1920, 1088, 1080), 1080, crop);
    expect(y).toBeCloseTo(crop.y / 1088);
    expect(h).toBeCloseTo(crop.h / 1088);
  });
});
