import { describe, expect, it } from "vitest";

import { isHdrFrame, pickCodec } from "./rpHdr";

describe("which video format to ask the console for", () => {
  it("is H.264 when chosen, whatever else works", () => {
    expect(pickCodec("h264", true, { ok: true })).toEqual({ codec: "h264" });
  });

  it("is HEVC where it decodes, else H.264, on Auto", () => {
    expect(pickCodec("auto", true, null)).toEqual({ codec: "h265" });
    expect(pickCodec("auto", false, null)).toEqual({ codec: "h264" });
  });

  it("is HDR only where it can be shown in HDR, else SDR with the reason", () => {
    expect(pickCodec("hdr", true, { ok: true })).toEqual({ codec: "h265_hdr" });
    expect(pickCodec("hdr", true, { ok: false, why: "screen" })).toEqual({ codec: "h265", hdrOff: "screen" });
    expect(pickCodec("hdr", false, { ok: false, why: "decoder" })).toEqual({ codec: "h264", hdrOff: "decoder" });
    expect(pickCodec("hdr", true, null)).toEqual({ codec: "h265", hdrOff: "not checked" });
  });
});

describe("isHdrFrame", () => {
  it("is a PQ or HLG picture", () => {
    expect(isHdrFrame({ transfer: "pq" })).toBe(true);
    expect(isHdrFrame({ transfer: "hlg" })).toBe(true);
    expect(isHdrFrame({ transfer: "bt709" })).toBe(false);
    expect(isHdrFrame(null)).toBe(false);
  });
});
