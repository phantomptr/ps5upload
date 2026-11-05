import { describe, expect, it, vi } from "vitest";
import { KEY_ASK_GAP_MS, KeyAsker, MseTrack, trimPoint } from "./watchPlayer";

describe("KeyAsker (#648)", () => {
  it("asks at once, then at most once per gap", () => {
    let t = 0;
    let asks = 0;
    const k = new KeyAsker(() => asks++, () => t);
    k.waiting();
    expect(asks).toBe(1);
    t = 400;
    k.waiting();
    expect(asks).toBe(1);
    t = KEY_ASK_GAP_MS;
    k.waiting();
    expect(asks).toBe(2);
  });

  it("does nothing without an ask (a watcher)", () => {
    expect(() => new KeyAsker(undefined).waiting()).not.toThrow();
  });
});

describe("cutting old video out of an MSE buffer (#649)", () => {
  it("cuts only at a keyframe far enough behind the playhead", () => {
    expect(trimPoint([0, 8, 11], 0, 12)).toBe(8);
    expect(trimPoint([0, 11], 0, 12)).toBe(null);
    // The stream's first keyframe is where the buffer starts: nothing to cut before it.
    expect(trimPoint([0], 0, 30)).toBe(null);
    expect(trimPoint([], 0, 30)).toBe(null);
  });

  function track(buffered: [number, number], currentTime: number, onNeedKey?: () => void) {
    const removed: number[][] = [];
    const sb = {
      updating: false,
      buffered: { length: 1, start: () => buffered[0], end: () => buffered[1] },
      remove: (a: number, b: number) => removed.push([a, b]),
      appendBuffer: vi.fn(),
    };
    const el = { currentTime, playbackRate: 1, paused: false, play: async () => {} };
    const t = new MseTrack(el as unknown as HTMLMediaElement, () => {}, "player", onNeedKey);
    Object.assign(t, { sb, ready: true });
    return { t, removed, sb };
  }

  it("with no keyframe to cut at, keeps the frames and asks for one", () => {
    const ask = vi.fn();
    const { t, removed } = track([0, 12], 12, ask);
    t.markKey(0);
    t.push(new Uint8Array(1));
    expect(removed).toEqual([]);
    expect(ask).toHaveBeenCalled();
  });

  it("doesn't ask while a new keyframe is on its way to being old enough", () => {
    const ask = vi.fn();
    const { t, removed } = track([0, 12], 12, ask);
    t.markKey(0);
    t.markKey(11);
    t.push(new Uint8Array(1));
    expect(removed).toEqual([]);
    expect(ask).not.toHaveBeenCalled();
  });

  it("removes up to just before the keyframe", () => {
    const { t, removed } = track([0, 12], 12, () => {});
    t.markKey(0);
    t.markKey(8);
    t.push(new Uint8Array(1));
    expect(removed).toEqual([[0, 7.999]]);
  });

  it("sound has no keyframes to wait for", () => {
    const { t, removed } = track([0, 12], 12);
    t.push(new Uint8Array(1));
    expect(removed).toEqual([[0, 9]]);
  });
});
