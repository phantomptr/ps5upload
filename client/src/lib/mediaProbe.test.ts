import { afterEach, describe, expect, it, vi } from "vitest";

import { AVC_CODECS, HEVC_CODECS, MSE_TYPES, probeMedia, sampleAvcC } from "./mediaProbe";

const g = globalThis as Record<string, unknown>;
const saved: Record<string, unknown> = {};
function stub(name: string, v: unknown) {
  if (!(name in saved)) saved[name] = g[name];
  g[name] = v;
}

afterEach(() => {
  for (const [k, v] of Object.entries(saved)) {
    if (v === undefined) delete g[k];
    else g[k] = v;
    delete saved[k];
  }
  vi.useRealTimers();
});

describe("probeMedia", () => {
  it("asks for every codec in hardware and software, with and without a description", async () => {
    const asked: { codec: string; hardwareAcceleration: string; description?: Uint8Array }[] = [];
    stub("VideoDecoder", {
      isConfigSupported: async (c: { codec: string; hardwareAcceleration: string; description?: Uint8Array }) => {
        asked.push(c);
        // No HEVC in software, no Main10 at all.
        const ok = !(c.codec.startsWith("hvc1.2") || (c.codec.startsWith("h") && c.hardwareAcceleration === "prefer-software"));
        return { supported: ok };
      },
    });
    stub("AudioDecoder", { isConfigSupported: async () => ({ supported: true }) });
    stub("MediaSource", { isTypeSupported: (t: string) => t.includes("avc1") });
    stub("ManagedMediaSource", undefined);
    stub("isSecureContext", true);
    const p = await probeMedia();
    expect(p.video).toHaveLength(AVC_CODECS.length * 2 + HEVC_CODECS.length);
    expect(asked).toHaveLength(p.video.length * 2);
    expect(asked.filter((c) => c.description).length).toBe(AVC_CODECS.length * 2);
    expect(p.video.find((v) => v.codec === "hvc1.1.6.L120.90")).toEqual({ codec: "hvc1.1.6.L120.90", desc: false, hw: true, sw: false });
    expect(p.video.find((v) => v.codec === "hvc1.2.4.L120.90")).toMatchObject({ hw: false, sw: false });
    expect(p.opus).toBe(true);
    expect(Object.keys(p.mse ?? {})).toEqual([...MSE_TYPES]);
    expect(p.mse?.['video/mp4; codecs="avc1.640028"']).toBe(true);
    expect(p.managed_mse).toBeNull();
    expect(p.secure_context).toBe(true);
    expect(p.has).toMatchObject({ VideoDecoder: true, AudioDecoder: true, MediaSource: true, ManagedMediaSource: false });
  });

  it("never throws, and a probe that hangs or fails says null", async () => {
    stub("VideoDecoder", {
      isConfigSupported: (c: { codec: string }) =>
        c.codec.startsWith("avc1") ? new Promise(() => {}) : Promise.reject(new Error("boom")),
    });
    stub("AudioDecoder", { isConfigSupported: () => { throw new Error("sync boom"); } });
    stub("MediaSource", { isTypeSupported: () => { throw new Error("no"); } });
    vi.useFakeTimers();
    const pending = probeMedia();
    await vi.advanceTimersByTimeAsync(2000);
    const p = await pending;
    expect(p.video.every((v) => v.hw === null && v.sw === null)).toBe(true);
    expect(p.opus).toBeNull();
    expect(Object.values(p.mse ?? {}).every((v) => v === false)).toBe(true);
  });

  it("says so when there is no WebCodecs", async () => {
    stub("VideoDecoder", undefined);
    stub("AudioDecoder", undefined);
    const p = await probeMedia();
    expect(p.has.VideoDecoder).toBe(false);
    expect(p.video.every((v) => v.hw === null)).toBe(true);
    expect(p.opus).toBeNull();
  });

  it("builds an avcC with the codec's profile and level", () => {
    const a = sampleAvcC("avc1.4d4028");
    expect([...a.slice(0, 4)]).toEqual([1, 0x4d, 0x40, 0x28]);
    expect(a[5]).toBe(0xe1);
  });
});
