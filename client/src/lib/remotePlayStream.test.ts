import { afterEach, describe, expect, it, vi } from "vitest";
import {
  avcCodecString,
  canDecodeAvc,
  isNoDecoderError,
  canvasClass,
  DEFAULT_KEYMAP,
  isKeyframe,
  keysFor,
  mergePads,
  padFromKeys,
  PadButton,
  IDLE_PAD,
  rebind,
  nextRtt,
  isPasscode,
} from "./remotePlayStream";

describe("rebind", () => {
  it("moves an action to a new key and frees the old one", () => {
    const next = rebind(DEFAULT_KEYMAP, { button: PadButton.CROSS }, "KeyX");
    expect(keysFor(next, { button: PadButton.CROSS })).toEqual(["KeyX"]);
    expect(next.Enter).toBeUndefined();
    expect(padFromKeys(["KeyX"], next).buttons).toBe(PadButton.CROSS);
  });
  it("takes the key off whatever it did before", () => {
    const next = rebind(DEFAULT_KEYMAP, { button: PadButton.CROSS }, "KeyQ");
    expect(keysFor(next, { button: PadButton.SQUARE })).toEqual([]);
  });
});

describe("padFromKeys", () => {
  it("maps buttons, sticks and triggers", () => {
    const p = padFromKeys(["Enter", "ArrowUp", "KeyD", "Digit3"]);
    expect(p.buttons).toBe(PadButton.CROSS | PadButton.UP);
    expect(p.lx).toBe(32767);
    expect(p.r2).toBe(255);
  });
  it("opposite keys cancel", () => {
    expect(padFromKeys(["KeyA", "KeyD"]).lx).toBe(0);
  });
  it("ignores unmapped keys", () => {
    expect(padFromKeys(["KeyP"])).toEqual(IDLE_PAD);
  });
});

describe("mergePads", () => {
  it("ORs buttons and clamps sticks", () => {
    const m = mergePads({ ...IDLE_PAD, buttons: 1, lx: 30000 }, { ...IDLE_PAD, buttons: 2, lx: 30000 });
    expect(m.buttons).toBe(3);
    expect(m.lx).toBe(32767);
  });
});

describe("isKeyframe", () => {
  it("knows an H.264 IDR from a P slice", () => {
    expect(isKeyframe(new Uint8Array([0, 0, 0, 1, 0x65, 0x88]), false)).toBe(true);
    expect(isKeyframe(new Uint8Array([0, 0, 0, 1, 0x41, 0x9a]), false)).toBe(false);
  });
  it("knows an HEVC IDR from a trailing picture", () => {
    expect(isKeyframe(new Uint8Array([0, 0, 0, 1, 19 << 1, 1]), true)).toBe(true);
    expect(isKeyframe(new Uint8Array([0, 0, 0, 1, 1 << 1, 1]), true)).toBe(false);
  });
});

describe("avcCodecString", () => {
  it("reads profile, constraints and level from the SPS", () => {
    expect(avcCodecString(new Uint8Array([0, 0, 0, 1, 0x67, 0x4d, 0x40, 0x32, 0x99]))).toBe("avc1.4d4032");
  });
});

describe("canvasClass", () => {
  it("leaves the picture's size to the browser and maps each fit to object-fit", () => {
    for (const fit of ["fit", "fill", "stretch"] as const) {
      const c = canvasClass(fit).split(" ");
      for (const k of ["rp-picture", "pointer-events-none"]) expect(c).toContain(k);
    }
    expect(canvasClass("fit")).toContain("object-contain");
    expect(canvasClass("fill")).toContain("object-cover");
    expect(canvasClass("stretch")).toContain("object-fill");
    // A stored value from elsewhere falls back to the whole picture.
    expect(canvasClass("zoom" as never)).toContain("object-contain");
  });
});

describe("canDecodeAvc", () => {
  afterEach(() => vi.unstubAllGlobals());
  const decoder = (check: (codec: string) => boolean | Error) =>
    vi.stubGlobal("VideoDecoder", {
      isConfigSupported: async ({ codec }: { codec: string }) => {
        const r = check(codec);
        if (r instanceof Error) throw r;
        return { supported: r };
      },
    });

  it("is false when every H.264 form is refused (Linux without GStreamer's decoder, #548)", async () => {
    decoder(() => false);
    expect(await canDecodeAvc(1920, 1080)).toBe(false);
  });
  it("is true when any form decodes", async () => {
    decoder((c) => c === "avc1.42e01f");
    expect(await canDecodeAvc(1920, 1080)).toBe(true);
  });
  it("does not treat a check that throws as a no", async () => {
    decoder(() => new TypeError("unsupported config dictionary"));
    expect(await canDecodeAvc(1920, 1080)).toBe(true);
  });
  it("is false with no WebCodecs at all", async () => {
    vi.stubGlobal("VideoDecoder", undefined);
    expect(await canDecodeAvc(1920, 1080)).toBe(false);
  });
});

describe("isNoDecoderError", () => {
  it("knows WebKitGTK's missing-decoder error", () => {
    const e = new DOMException("No decoder found for codec avc1.4d4032", "NotSupportedError");
    expect(isNoDecoderError(e)).toBe(true);
    expect(isNoDecoderError("NotSupportedError: No decoder found for codec avc1.4d4032")).toBe(true);
  });
  it("leaves a broken picture to the restart path", () => {
    expect(isNoDecoderError(new DOMException("Decoding error.", "EncodingError"))).toBe(false);
  });
});

describe("nextRtt", () => {
  it("takes the first round trip as it is and smooths the ones after", () => {
    expect(nextRtt(null, 100, 112)).toBe(12);
    expect(nextRtt(12, 200, 224)).toBe(15);
  });

  it("ignores a pong that is not one of this page's timestamps", () => {
    expect(nextRtt(8, "x", 100)).toBe(8);
    expect(nextRtt(null, null, 100)).toBeNull();
    expect(nextRtt(null, Number.NaN, 100)).toBeNull();
  });

  it("never goes below zero", () => {
    expect(nextRtt(null, 150, 100)).toBe(0);
  });
});

describe("isPasscode", () => {
  it("is four digits, nothing else", () => {
    expect(isPasscode("0042")).toBe(true);
    expect(isPasscode("123")).toBe(false);
    expect(isPasscode("12345")).toBe(false);
    expect(isPasscode("12a4")).toBe(false);
    expect(isPasscode(" 1234")).toBe(false);
  });
});
