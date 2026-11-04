import { describe, expect, it, vi } from "vitest";

import * as storage from "../lib/safeStorage";
import { DEFAULT_RP_PREFS, LOWEST_AUDIO_BUFFER_MS, audioBufferFor, loadRpPrefs } from "./remotePlay";

describe("audioBufferFor", () => {
  it("is the chosen sound buffer, or the smallest one in low-latency mode", () => {
    expect(audioBufferFor({ lowLatency: false, audioBufferMs: 120 })).toBe(120);
    expect(audioBufferFor({ lowLatency: true, audioBufferMs: 120 })).toBe(LOWEST_AUDIO_BUFFER_MS);
    expect(audioBufferFor({ lowLatency: true, audioBufferMs: 40 })).toBe(40);
  });

  it("starts with low latency off and the 720p offer on", () => {
    expect(DEFAULT_RP_PREFS.lowLatency).toBe(false);
    expect(DEFAULT_RP_PREFS.autoQuality).toBe(true);
  });
});

describe("the picture on this device", () => {
  it("is on unless the setting says otherwise", () => {
    expect(DEFAULT_RP_PREFS.pictureHere).toBe(true);
  });
});

describe("loadRpPrefs", () => {
  const stored = (v: unknown) => vi.spyOn(storage, "safeGetItem").mockReturnValue(v === null ? null : JSON.stringify(v));

  it("loads settings saved before the button map and HDR, keeping what was set", () => {
    // A 10.0.3 save: no buttonMap, picture with the Upscale switch.
    stored({ res: "1280x720", codec: "h264", picture: { sharpen: 30, upscale: true }, rumble: false });
    const p = loadRpPrefs();
    expect(p.res).toBe("1280x720");
    expect(p.codec).toBe("h264");
    expect(p.rumble).toBe(false);
    expect(p.buttonMap).toEqual({});
    expect(p.picture).toMatchObject({ enhance: "fsr", sharpen: 30 });
    vi.restoreAllMocks();
  });

  it("falls back from values this version has no use for", () => {
    stored({ res: "4k", fps: 144, fit: "zoom", touchSize: "xl", decoding: "gpu" });
    const p = loadRpPrefs();
    expect(p).toMatchObject({
      res: DEFAULT_RP_PREFS.res,
      fps: DEFAULT_RP_PREFS.fps,
      fit: DEFAULT_RP_PREFS.fit,
      touchSize: DEFAULT_RP_PREFS.touchSize,
      decoding: DEFAULT_RP_PREFS.decoding,
    });
    vi.restoreAllMocks();
  });

  it("keeps a button map and HDR, and drops what it does not know", () => {
    stored({ codec: "hdr", buttonMap: { cross: "circle", nope: "cross" } });
    expect(loadRpPrefs()).toMatchObject({ codec: "hdr", buttonMap: { cross: "circle" } });
    stored({ codec: "av1" });
    expect(loadRpPrefs().codec).toBe("auto");
    stored(null);
    expect(loadRpPrefs()).toEqual(DEFAULT_RP_PREFS);
    vi.restoreAllMocks();
  });
});
