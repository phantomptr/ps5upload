import { describe, expect, it } from "vitest";
import { annexBNals, opusInit, opusSamples, opusSegment, videoInit, videoSegment } from "./fmp4";

const hex = (s: string) => Uint8Array.from(s.match(/../g)!.map((b) => parseInt(b, 16)));
const annexB = (...nals: string[]) => {
  const parts = nals.map((n) => [0, 0, 0, 1, ...hex(n)]);
  return Uint8Array.from(parts.flat());
};

// Parameter sets from 1920×1080 streams made with ffmpeg (x264 High; x265 Main and Main 10).
const AVC = annexB("6764002aacb200f0044fcb8088000003000800000303c078c19240", "68ebc3cb22c0");
const HEVC8 = annexB(
  "40010c01ffff01600000030090000003000003007b959809",
  "42010101600000030090000003000003007ba003c08010e596566924caf0168080000003008000001e04",
  "4401c172b46240",
);
const HEVC10 = annexB(
  "40010c01ffff02200000030090000003000003007b928090",
  "42010102200000030090000003000003007ba003c08010e4d964a924caf0168080000003008000001e04",
  "4401c172b46240",
);

/** The top-level boxes: [type, size]. */
function boxes(b: Uint8Array): [string, number][] {
  const out: [string, number][] = [];
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  for (let o = 0; o < b.length; ) {
    const size = dv.getUint32(o);
    out.push([String.fromCharCode(...b.subarray(o + 4, o + 8)), size]);
    if (size < 8) break;
    o += size;
  }
  return out;
}

const has = (b: Uint8Array, type: string) => new TextDecoder("latin1").decode(b).includes(type);

describe("fmp4", () => {
  it("splits Annex-B into NAL units", () => {
    expect(annexBNals(AVC).map((n) => n[0] & 0x1f)).toEqual([7, 8]);
  });

  it("writes an H.264 init segment with the SPS's codec string", () => {
    const v = videoInit(AVC, false, { w: 1920, h: 1080 })!;
    expect(v.mime).toBe('video/mp4; codecs="avc1.64002a"');
    expect(boxes(v.init).map(([t]) => t)).toEqual(["ftyp", "moov"]);
    expect(boxes(v.init).reduce((a, [, n]) => a + n, 0)).toBe(v.init.length);
    expect(has(v.init, "avcC")).toBe(true);
  });

  it("writes HEVC codec strings for Main and Main 10", () => {
    expect(videoInit(HEVC8, true, { w: 1920, h: 1080 })!.mime).toBe('video/mp4; codecs="hvc1.1.6.L123.90"');
    const ten = videoInit(HEVC10, true, { w: 1920, h: 1080 })!;
    expect(ten.mime).toBe('video/mp4; codecs="hvc1.2.4.L123.90"');
    expect(has(ten.init, "hvcC")).toBe(true);
  });

  it("gives up without parameter sets", () => {
    expect(videoInit(annexB("68ebc3cb22c0"), false, { w: 1, h: 1 })).toBeNull();
    expect(videoInit(annexB("4401c172b46240"), true, { w: 1, h: 1 })).toBeNull();
  });

  it("writes one sample per segment, length-prefixed, without the delimiter", () => {
    const au = annexB("09f0", "65888400ff");
    const seg = videoSegment(7, 90000, 1500, au, false, true);
    expect(boxes(seg).map(([t]) => t)).toEqual(["moof", "mdat"]);
    const mdat = seg.subarray(boxes(seg)[0][1] + 8);
    expect(Array.from(mdat)).toEqual([0, 0, 0, 5, 0x65, 0x88, 0x84, 0x00, 0xff]);
    // trun's data offset points at the sample.
    const i = new TextDecoder("latin1").decode(seg).indexOf("trun");
    expect(new DataView(seg.buffer).getUint32(i + 12)).toBe(boxes(seg)[0][1] + 8);
  });

  it("times Opus packets from their TOC byte", () => {
    expect(opusSamples(Uint8Array.of(0xfc))).toBe(960); // CELT 20 ms, one frame
    expect(opusSamples(Uint8Array.of(0x78))).toBe(960); // hybrid 20 ms
    expect(opusSamples(Uint8Array.of(0xfd))).toBe(1920); // two frames
    expect(opusSamples(Uint8Array.of(0xfb, 0x03))).toBe(2880); // three 20 ms SILK frames (code 3)
    expect(has(opusInit(2, 48000), "dOps")).toBe(true);
    expect(boxes(opusSegment(1, 0, Uint8Array.of(0xfc, 1, 2))).map(([t]) => t)).toEqual(["moof", "mdat"]);
  });
});
