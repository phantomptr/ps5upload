import { describe, expect, it } from "vitest";
import { installPolyfills, timeoutSignal, uuidV4 } from "./polyfills";

describe("polyfills for an older WebKit (#765)", () => {
  it("AbortSignal.timeout aborts after the time, as a TimeoutError", async () => {
    const s = timeoutSignal(10);
    expect(s.aborted).toBe(false);
    await new Promise((r) => setTimeout(r, 30));
    expect(s.aborted).toBe(true);
    expect((s.reason as { name?: string }).name).toBe("TimeoutError");
  });

  it("is installed where missing and left alone where present", () => {
    const fakeSignal = function () {} as unknown as typeof AbortSignal;
    const fakeCrypto = { getRandomValues: (b: Uint8Array) => b.fill(7) } as unknown as Crypto;
    const g = { AbortSignal: fakeSignal, crypto: fakeCrypto } as unknown as typeof globalThis;
    installPolyfills(g);
    expect(typeof (g.AbortSignal as unknown as { timeout: unknown }).timeout).toBe("function");
    expect(g.crypto.randomUUID()).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    const mine = () => "kept";
    const g2 = { AbortSignal: Object.assign(function () {}, { timeout: mine }), crypto: undefined } as unknown as typeof globalThis;
    installPolyfills(g2);
    expect((g2.AbortSignal as unknown as { timeout: unknown }).timeout).toBe(mine);
  });

  it("makes version 4 ids", () => {
    const id = uuidV4((b) => void b.fill(255));
    expect(id).toBe("ffffffff-ffff-4fff-bfff-ffffffffffff");
  });
});

describe("Play on an older WebKit (#766)", () => {
  it("can stream with the system media player when there is no WebCodecs", async () => {
    const { vi } = await import("vitest");
    const { canStream, streamRefusal } = await import("./remotePlayStream");
    vi.stubGlobal("VideoDecoder", undefined);
    vi.stubGlobal("WebSocket", function () {});
    vi.stubGlobal("MediaSource", undefined);
    expect(canStream()).toBe(false);
    vi.stubGlobal("MediaSource", function () {});
    expect(canStream()).toBe(true);
    expect(streamRefusal()).toBeNull();
    vi.unstubAllGlobals();
  });
});

describe("H.264 check without WebCodecs (#769)", () => {
  it("asks the system media player, and says no only when it plays no H.264", async () => {
    const { vi } = await import("vitest");
    const { canDecodeAvc } = await import("./remotePlayStream");
    vi.stubGlobal("VideoDecoder", undefined);
    vi.stubGlobal("ManagedMediaSource", undefined);
    vi.stubGlobal("MediaSource", { isTypeSupported: (t: string) => t.includes("avc1.640032") });
    expect(await canDecodeAvc(1280, 720)).toBe(true);
    vi.stubGlobal("MediaSource", { isTypeSupported: () => false });
    expect(await canDecodeAvc(1280, 720)).toBe(false);
    vi.stubGlobal("MediaSource", undefined);
    expect(await canDecodeAvc(1280, 720)).toBe(false);
    vi.unstubAllGlobals();
  });
});
