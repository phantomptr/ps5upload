// The guest's sound (#657): no real audio plays here; the page, its media elements and
// MediaSource are fakes (vitest runs in node).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { MSE_EDGE, MsePlayer, MseSound, OPUS_MP4, SOUND_EDGE, WatchPlayer, edgeAction } from "./watchPlayer";

/** A MediaSource that takes only `types`, and records what it was asked. */
function fakeMediaSource(types: string[]) {
  const asked: string[] = [];
  class FakeMs extends EventTarget {
    static isTypeSupported(t: string) {
      asked.push(t);
      return types.includes(t);
    }
    readyState = "closed";
    addSourceBuffer() {
      throw new Error("not in this test");
    }
    endOfStream() {}
  }
  return { FakeMs, asked };
}

/** The sound header: channels, a pad byte, then the rate (big endian). */
const HEADER = new Uint8Array([2, 0, 0, 0, 0xbb, 0x80]);

let play: ReturnType<typeof vi.fn<(...a: unknown[]) => Promise<void>>>;

/** Just enough of an element for the players. */
function fakeElement(tag: string) {
  const kids: Array<{ tag: string }> = [];
  return {
    tag,
    className: "",
    dataset: {} as Record<string, string>,
    addEventListener() {},
    src: "",
    muted: false,
    autoplay: false,
    playsInline: false,
    disableRemotePlayback: false,
    srcObject: null as unknown,
    paused: true,
    currentTime: 0,
    playbackRate: 1,
    play: (...a: unknown[]) => play(...a),
    removeAttribute(name: string) {
      if (name === "src") this.src = "";
    },
    append(...els: Array<{ tag: string }>) {
      kids.push(...els);
    },
    querySelector(t: string) {
      return kids.find((k) => k.tag === t) ?? null;
    },
    getContext: () => null,
    remove() {},
  };
}

beforeEach(() => {
  play = vi.fn(() => Promise.resolve());
  vi.stubGlobal("document", { createElement: (t: string) => fakeElement(t) });
  vi.stubGlobal("window", {});
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:fake");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("the sound's live edge", () => {
  it("is looser than the player's picture, so the sound is not seeked on every frame", () => {
    expect(SOUND_EDGE.seek).toBeGreaterThan(MSE_EDGE.player.seek * 3);
    // 0.2 s behind: the picture seeks, the sound only plays a little faster.
    expect(edgeAction(0.2, 10, "player").seekTo).not.toBeNull();
    expect(edgeAction(0.2, 10, SOUND_EDGE)).toEqual({ seekTo: null, rate: 1 });
    expect(edgeAction(0.4, 10, SOUND_EDGE)).toEqual({ seekTo: null, rate: 1.1 });
    expect(edgeAction(1, 10, SOUND_EDGE)).toEqual({ seekTo: 9.88, rate: 1 });
  });
});

describe("MseSound", () => {
  it("opens nothing before the tap, then opens and plays inside it", () => {
    const { FakeMs, asked } = fakeMediaSource([OPUS_MP4]);
    vi.stubGlobal("window", { MediaSource: FakeMs });
    const fail = vi.fn();
    const s = new MseSound("player", fail);
    s.setHeader(HEADER);
    expect(asked).toEqual([]);
    expect(play).not.toHaveBeenCalled();
    s.start();
    expect(asked).toEqual([OPUS_MP4]);
    expect(play).toHaveBeenCalledTimes(1);
    expect(fail).not.toHaveBeenCalled();
    expect(s.playing).toBe(true);
  });

  it("plays in the tap even before the header, and opens when it comes", () => {
    const { FakeMs, asked } = fakeMediaSource([OPUS_MP4]);
    vi.stubGlobal("window", { MediaSource: FakeMs });
    const s = new MseSound("player", vi.fn());
    s.start();
    expect(play).toHaveBeenCalledTimes(1);
    s.setHeader(HEADER);
    expect(asked).toEqual([OPUS_MP4]);
  });

  it("says so when the browser has no Opus in MP4", () => {
    const { FakeMs } = fakeMediaSource([]);
    vi.stubGlobal("window", { MediaSource: FakeMs });
    const fail = vi.fn();
    const s = new MseSound("player", fail);
    s.setHeader(HEADER);
    s.start();
    expect(fail).toHaveBeenCalledTimes(1);
    expect(s.playing).toBe(false);
  });

  it("uses an iPhone's ManagedMediaSource", () => {
    const { FakeMs, asked } = fakeMediaSource([OPUS_MP4]);
    vi.stubGlobal("window", { ManagedMediaSource: FakeMs });
    const s = new MseSound("player", vi.fn());
    s.setHeader(HEADER);
    s.start();
    expect(asked).toEqual([OPUS_MP4]);
    expect(s.el.disableRemotePlayback).toBe(true);
    expect((s.el as unknown as { srcObject: unknown }).srcObject).toBeInstanceOf(FakeMs);
  });
});

describe("players without WebCodecs sound", () => {
  it("MSE page: the tap plays the sound element", () => {
    const { FakeMs } = fakeMediaSource([OPUS_MP4]);
    vi.stubGlobal("window", { MediaSource: FakeMs });
    const p = new MsePlayer(() => {}, "player");
    const msg = new Uint8Array(1 + HEADER.length);
    msg[0] = 3;
    msg.set(HEADER, 1);
    p.onMessage(msg.buffer);
    p.startSound();
    expect(p.sound).toBe(true);
    expect(play).toHaveBeenCalled();
    expect(p.element.querySelector("audio")).not.toBeNull();
    // The picture stays muted for autoplay; the sound is its own element.
    expect((p.element.querySelector("video") as HTMLVideoElement).muted).toBe(true);
  });

  it("MSE page with no Opus: tells the page", () => {
    const { FakeMs } = fakeMediaSource([]);
    vi.stubGlobal("window", { MediaSource: FakeMs });
    const p = new MsePlayer(() => {}, "player");
    const fail = vi.fn();
    p.onSoundFail = fail;
    const msg = new Uint8Array(1 + HEADER.length);
    msg[0] = 3;
    msg.set(HEADER, 1);
    p.onMessage(msg.buffer);
    p.startSound();
    expect(fail).toHaveBeenCalledTimes(1);
  });

  it("VideoDecoder but no AudioDecoder (Safari before 26): the sound header no longer throws, MSE plays it", () => {
    vi.stubGlobal("AudioDecoder", undefined);
    const { FakeMs, asked } = fakeMediaSource([OPUS_MP4]);
    vi.stubGlobal("window", { MediaSource: FakeMs });
    const p = new WatchPlayer(() => {}, "player");
    const msg = new Uint8Array(1 + HEADER.length);
    msg[0] = 3;
    msg.set(HEADER, 1);
    expect(() => p.onMessage(msg.buffer)).not.toThrow();
    p.startSound();
    expect(play).toHaveBeenCalledTimes(1);
    // Asked once to choose MSE over the WebAssembly decoder (#720), then by the track it opens.
    expect(new Set(asked)).toEqual(new Set([OPUS_MP4]));
    // An Opus frame goes to MSE, not a decoder that is not there.
    expect(() => p.onMessage(new Uint8Array([4, 0xfc, 1, 2]).buffer)).not.toThrow();
  });
});
