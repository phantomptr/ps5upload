// #720 (macOS 13, 12.3.0): the app's media player jumped to the live edge ~10 times a minute, every
// time from a playhead read as 0.00 s with the last ~10 s buffered, about as often as old video is
// trimmed. Each jump now says how long after the last trim and the last keyframe it came. Fakes
// only: the element and the MediaSource (vitest runs in node).
import { afterEach, describe, expect, it, vi } from "vitest";

import { HOST_MSE_EDGE, MseTrack, mseJumpsText } from "./watchPlayer";

class FakeSb extends EventTarget {
  mode = "";
  updating = false;
  range: [number, number] | null = null;
  removed: Array<[number, number]> = [];
  get buffered() {
    const r = this.range;
    return { length: r ? 1 : 0, start: () => r![0], end: () => r![1] };
  }
  appendBuffer() {}
  remove(a: number, b: number) {
    this.removed.push([a, b]);
  }
}

function setUp() {
  const sb = new FakeSb();
  let open: (() => void) | null = null;
  class FakeMs {
    static isTypeSupported = () => true;
    readyState = "open";
    addEventListener(_: string, f: () => void) {
      open = f;
    }
    addSourceBuffer() {
      return sb;
    }
    endOfStream() {}
  }
  vi.stubGlobal("window", { MediaSource: FakeMs });
  const el = {
    currentTime: 0,
    readyState: 4,
    paused: false,
    seeking: false,
    playbackRate: 1,
    play: () => Promise.resolve(),
    removeAttribute() {},
    srcObject: null as unknown,
  };
  let t = 0;
  const track = new MseTrack(el as unknown as HTMLMediaElement, () => {}, HOST_MSE_EDGE, () => {}, () => t);
  expect(track.open('video/mp4; codecs="avc1.640028"', new Uint8Array(1))).toBe(true);
  open!();
  return { track, sb, el, at: (ms: number) => (t = ms) };
}

afterEach(() => vi.unstubAllGlobals());

describe("the media player's jumps to the live edge", () => {
  it("say how long after the last trim and keyframe they came", () => {
    const { track, sb, el, at } = setUp();
    track.markKey(0);
    at(8000);
    track.markKey(8);
    // At the edge with 12 s buffered: no jump, the old video goes (up to the keyframe at 8 s).
    at(12_000);
    sb.range = [0, 12];
    el.currentTime = 11.9;
    track.push(new Uint8Array(1));
    expect(track.seeks).toBe(0);
    expect(track.trims).toBe(1);
    expect(sb.removed).toHaveLength(1);
    // Half a second later the playhead reads 0 with the buffer well past it: a jump.
    at(12_500);
    sb.range = [8, 12.6];
    el.currentTime = 0;
    track.push(new Uint8Array(1));
    expect(track.seeks).toBe(1);
    expect(track.fromZero).toBe(1);
    expect(track.lastJump).toMatchObject({ at: 0, start: 8, sinceTrimMs: 500, trimFrom: 11.9, sinceKeyMs: 4500 });
    const text = mseJumpsText({ seeks: 1, maxLagMs: 12600, fromZero: 1, trims: 1, lastJump: track.lastJump });
    expect(text).toContain("1 jumps to the live edge (1 from 0.00 s, 1 trims; most behind 12600 ms");
    expect(text).toContain("since the last trim 500 ms (made at 11.90 s), since the last keyframe 4500 ms");
  });

  it("before any trim or keyframe, say so", () => {
    const text = mseJumpsText({
      seeks: 1,
      maxLagMs: 900,
      fromZero: 0,
      trims: 0,
      lastJump: {
        at: 1,
        start: 0,
        end: 1.9,
        ranges: 1,
        readyState: 4,
        paused: false,
        seeking: false,
        sinceTrimMs: -1,
        trimFrom: -1,
        sinceKeyMs: -1,
      },
    });
    expect(text).toContain("since the last trim none yet, since the last keyframe none yet");
    expect(mseJumpsText({ seeks: 0, maxLagMs: 0, fromZero: 0, trims: 0, lastJump: null })).toBe(
      "; media player: 0 jumps to the live edge (0 from 0.00 s, 0 trims; most behind 0 ms)",
    );
  });
});
