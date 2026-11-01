import { describe, expect, it, vi } from "vitest";
import { encodeSnapshotReply, KIND_SNAPSHOT, parseSnapshotRequest, scaledSize, takeSnapshot, type SnapshotSource } from "./rpSnapshot";

/** The engine's side of the answer: kind, metadata and image (engine rp_automation.rs). */
function decode(b: Uint8Array): { kind: number; meta: Record<string, unknown>; image: Uint8Array } {
  const n = new DataView(b.buffer, b.byteOffset, b.byteLength).getUint32(1);
  return {
    kind: b[0],
    meta: JSON.parse(new TextDecoder().decode(b.subarray(5, 5 + n))) as Record<string, unknown>,
    image: b.subarray(5 + n),
  };
}

/** A canvas stand-in: toBlob gives "<type> <w>x<h>" so the test sees what was encoded. */
function fakeCanvas(w: number, h: number) {
  const drawn: unknown[][] = [];
  const c = {
    width: w,
    height: h,
    toBlob: vi.fn((cb: (b: Blob | null) => void, type: string) => cb(new Blob([`${type} ${c.width}x${c.height}`]))),
    getContext: vi.fn(() => ({ drawImage: (...a: unknown[]) => drawn.push(a) })),
  };
  return { c: c as unknown as HTMLCanvasElement, raw: c, drawn };
}

function source(canvas: HTMLCanvasElement, seq = 3, at = 1_000) {
  const s = {
    canvas,
    seq,
    frameSeq: () => s.seq,
    frameAt: () => at,
    waitForFrame: vi.fn(async (_seq: number, _ms: number) => false),
  };
  return s satisfies SnapshotSource;
}

describe("parseSnapshotRequest", () => {
  it("takes the engine's request and refuses anything else", () => {
    expect(parseSnapshotRequest({ id: 4, format: "jpeg", max_width: 640 })).toEqual({
      id: 4,
      format: "jpeg",
      maxWidth: 640,
      afterSeq: null,
      waitMs: 1500,
    });
    expect(parseSnapshotRequest({ id: 1 })?.format).toBe("png");
    expect(parseSnapshotRequest({ id: 1, after_seq: 9, wait_ms: 99_999 })).toMatchObject({ afterSeq: 9, waitMs: 1500 });
    for (const bad of [null, 3, "x", {}, { id: -1 }, { id: 1.5 }]) expect(parseSnapshotRequest(bad)).toBeNull();
  });
});

describe("scaledSize", () => {
  it("scales down to max_width keeping the shape, never up", () => {
    expect(scaledSize(1920, 1080, 1280)).toEqual({ w: 1280, h: 720 });
    expect(scaledSize(1920, 1080, null)).toEqual({ w: 1920, h: 1080 });
    expect(scaledSize(1280, 720, 1920)).toEqual({ w: 1280, h: 720 });
    expect(scaledSize(1920, 1080, 16)).toEqual({ w: 16, h: 9 });
  });
});

describe("takeSnapshot", () => {
  it("answers with the picture and its metadata", async () => {
    const { c } = fakeCanvas(1920, 1080);
    const out = decode(await takeSnapshot({ id: 7, format: "png", maxWidth: null, afterSeq: null, waitMs: 0 }, source(c), { now: () => 1_016 }));
    expect(out.kind).toBe(KIND_SNAPSHOT);
    expect(out.meta).toEqual({
      id: 7,
      format: "png",
      width: 1920,
      height: 1080,
      captured_at_ms: 1_016,
      frame_at_ms: 1_000,
      frame_seq: 3,
    });
    expect(new TextDecoder().decode(out.image)).toBe("image/png 1920x1080");
  });

  it("respects max_width by drawing into a smaller canvas, leaving the shown one alone", async () => {
    const shown = fakeCanvas(1920, 1080);
    const small = fakeCanvas(0, 0);
    const out = decode(
      await takeSnapshot({ id: 1, format: "jpeg", maxWidth: 640, afterSeq: null, waitMs: 0 }, source(shown.c), { makeCanvas: () => small.c }),
    );
    expect(out.meta).toMatchObject({ width: 640, height: 360, format: "jpeg" });
    expect(new TextDecoder().decode(out.image)).toBe("image/jpeg 640x360");
    expect(small.drawn).toEqual([[shown.c, 0, 0, 640, 360]]);
    expect(shown.raw.toBlob).not.toHaveBeenCalled();
    expect([shown.raw.width, shown.raw.height]).toEqual([1920, 1080]);
  });

  it("says so when there is no picture yet, or no newer one in time", async () => {
    const { c } = fakeCanvas(1280, 720);
    const none = decode(await takeSnapshot({ id: 2, format: "png", maxWidth: null, afterSeq: null, waitMs: 0 }, source(c, 0)));
    expect(none.meta).toEqual({ id: 2, error: "no_picture" });
    const src = source(c, 5);
    const stale = decode(await takeSnapshot({ id: 3, format: "png", maxWidth: null, afterSeq: 5, waitMs: 50 }, src));
    expect(stale.meta).toEqual({ id: 3, error: "no_newer_frame" });
    expect(src.waitForFrame).toHaveBeenCalledWith(5, 50);
  });

  it("waits for a newer picture when asked, and reports the newer one", async () => {
    const { c } = fakeCanvas(1280, 720);
    const src = source(c, 5);
    src.waitForFrame.mockImplementation(async () => {
      src.seq = 6;
      return true;
    });
    const out = decode(await takeSnapshot({ id: 4, format: "png", maxWidth: null, afterSeq: 5, waitMs: 50 }, src));
    expect(out.meta.frame_seq).toBe(6);
    // Already newer: no wait.
    src.waitForFrame.mockClear();
    await takeSnapshot({ id: 5, format: "png", maxWidth: null, afterSeq: 2, waitMs: 50 }, src);
    expect(src.waitForFrame).not.toHaveBeenCalled();
  });

  it("an encoder that gives nothing is an error, not an empty image", async () => {
    const { c, raw } = fakeCanvas(1280, 720);
    raw.toBlob.mockImplementation((cb: (b: Blob | null) => void) => cb(null));
    const out = decode(await takeSnapshot({ id: 6, format: "png", maxWidth: null, afterSeq: null, waitMs: 0 }, source(c)));
    expect(out.meta).toEqual({ id: 6, error: "encode_failed" });
  });
});

describe("encodeSnapshotReply", () => {
  it("is [9][u32 BE meta length][meta][image]", () => {
    const b = encodeSnapshotReply({ id: 1 }, new Uint8Array([1, 2]));
    expect(Array.from(b.subarray(0, 5))).toEqual([9, 0, 0, 0, 8]);
    expect(new TextDecoder().decode(b.subarray(5, 13))).toBe('{"id":1}');
    expect(Array.from(b.subarray(13))).toEqual([1, 2]);
  });
});
