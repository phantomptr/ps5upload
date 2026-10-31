// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { spsVisibleRect, visibleFix } from "./rpSps";

const fixture = (name: string) => new Uint8Array(readFileSync(`e2e/fixtures/remote-play/${name}`));

describe("spsVisibleRect", () => {
  it("reads a 1080p H.264 stream coded 1920x1088 as its 1920x1080 picture", () => {
    expect(spsVisibleRect(fixture("h264_1920x1080.h264"), false)).toEqual({ x: 0, y: 0, w: 1920, h: 1080 });
  });
  it("reads a 720p H.264 stream", () => {
    expect(spsVisibleRect(fixture("h264_1280x720.h264"), false)).toEqual({ x: 0, y: 0, w: 1280, h: 720 });
  });
  it("reads a 1080p HEVC stream", () => {
    expect(spsVisibleRect(fixture("hevc_1920x1080.hevc"), true)).toEqual({ x: 0, y: 0, w: 1920, h: 1080 });
  });
  it("is null without an SPS or for the other codec's", () => {
    expect(spsVisibleRect(new Uint8Array([0, 0, 1, 0x65, 1, 2]), false)).toBeNull();
    expect(spsVisibleRect(fixture("hevc_1920x1080.hevc"), false)).toBeNull();
  });
});

describe("visibleFix", () => {
  const sps = { x: 0, y: 0, w: 1920, h: 1080 };
  const frame = (w: number, h: number) => ({ codedWidth: 1920, codedHeight: 1088, visibleRect: { width: w, height: h } });
  it("rebuilds Edge's too-small frame (#538: 1920x1080 reported as 1280x720)", () => {
    expect(visibleFix(frame(1280, 720), sps)).toEqual(sps);
    expect(visibleFix(frame(960, 540), sps)).toEqual(sps);
  });
  it("leaves a correctly reported frame alone", () => {
    expect(visibleFix(frame(1920, 1080), sps)).toBeNull();
  });
  it("never reaches past the coded size or acts without an SPS", () => {
    expect(visibleFix({ codedWidth: 1280, codedHeight: 720, visibleRect: { width: 640, height: 360 } }, sps)).toBeNull();
    expect(visibleFix(frame(1280, 720), null)).toBeNull();
  });
});
