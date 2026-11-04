import { describe, expect, it } from "vitest";
import { avcCodecFromHeader, isHevcHeader, orderAudio, parseReplay } from "./rpReplay";

/** A snapshot in the engine's wire form (rp_replay.rs `encode`). */
function snapshot(recs: Array<{ kind: number; t: number; data: number[]; key?: boolean }>): ArrayBuffer {
  const out: number[] = [..."PSRP1"].map((c) => c.charCodeAt(0));
  for (const r of recs) {
    out.push(r.kind);
    const hi = Math.floor(r.t / 2 ** 32);
    for (const n of [hi, r.t >>> 0]) out.push((n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255);
    const l = r.data.length;
    out.push((l >>> 24) & 255, (l >>> 16) & 255, (l >>> 8) & 255, l & 255);
    if (r.kind === 1) out.push(r.key ? 1 : 0);
    out.push(...r.data);
  }
  return new Uint8Array(out).buffer;
}

const SPS = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0xaa];
const VPS = [0, 0, 0, 1, 0x40, 0x01, 0x0c];

describe("instant replay snapshot", () => {
  it("reads the headers and the frames in order, with times past 2^32 µs", () => {
    const r = parseReplay(
      snapshot([
        { kind: 0, t: 0, data: SPS },
        { kind: 3, t: 0, data: [2, 0, 0, 0, 0xbb, 0x80, 0, 0, 1, 0xe0] },
        { kind: 1, t: 5_000_000_000, data: [1, 2, 3], key: true },
        { kind: 4, t: 5_000_000_010, data: [0x12, 0x34, 9] },
      ]),
    );
    expect(r.hevc).toBe(false);
    expect(r.audio).toEqual({ channels: 2, rate: 48000 });
    expect(r.frames.map((f) => [f.kind, f.tUs, f.key])).toEqual([
      [1, 5_000_000_000, true],
      [4, 5_000_000_010, false],
    ]);
    // The packet alone: the decoder must not get the index in front of it.
    expect([...r.frames[1].data]).toEqual([9]);
  });

  it("puts sound back in the console's order, across the index's wrap", () => {
    const f = (t: number, i: number, b: number) => ({ tUs: t, data: new Uint8Array([i >> 8, i & 255, b]) });
    const got = orderAudio([
      f(100, 65534, 1),
      f(200, 0, 3),
      // Recovered late, with a later time.
      f(300, 65535, 2),
      f(400, 1, 4),
      // A copy of one already here.
      f(500, 1, 4),
    ]);
    expect(got.map((g) => g.data[0])).toEqual([1, 2, 3, 4]);
    // Rising times, as a decoder wants.
    expect(got.map((g) => g.tUs)).toEqual([100, 200, 300, 400]);
  });

  it("refuses what is not a snapshot or is cut short", () => {
    expect(() => parseReplay(new TextEncoder().encode("hello").buffer)).toThrow();
    const ok = new Uint8Array(snapshot([{ kind: 0, t: 0, data: SPS }]));
    expect(() => parseReplay(ok.slice(0, ok.length - 2).buffer)).toThrow(/cut short/);
  });

  it("tells H.264 from HEVC and names the H.264 codec from its SPS", () => {
    expect(isHevcHeader(new Uint8Array(VPS))).toBe(true);
    expect(isHevcHeader(new Uint8Array(SPS))).toBe(false);
    expect(avcCodecFromHeader(new Uint8Array(SPS))).toBe("avc1.42001f");
    expect(avcCodecFromHeader(new Uint8Array(VPS))).toBe("");
  });
});
