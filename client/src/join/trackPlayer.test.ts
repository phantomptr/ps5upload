import { describe, expect, it, vi } from "vitest";

import { readCarries, readJoinState } from "../screens/Play/playTogetherState";
import type { MsePlayer, WatchPlayer } from "../pad/watchPlayer";
import { TrackPlayer, TrackStats } from "./trackPlayer";

// vitest runs in node: the elements are fakes.
class FakeEl {
  style: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  children: FakeEl[] = [];
  srcObject: unknown = null;
  muted = false;
  autoplay = false;
  playsInline = false;
  paused = true;
  plays = 0;
  append(...c: FakeEl[]) {
    this.children.push(...c);
  }
  contains(c: FakeEl) {
    return this.children.includes(c);
  }
  play() {
    this.plays++;
    this.paused = false;
    return Promise.resolve();
  }
}
vi.stubGlobal("document", { createElement: () => new FakeEl() });

/** The channel's player, as far as the track player uses it. */
function inner() {
  const el = new FakeEl();
  const got: number[] = [];
  const p = {
    element: el,
    stats: { frames: 7, snapshot: () => ({ frames: 7, decoder: "webcodecs" }) },
    onSoundFail: null as (() => void) | null,
    onMessage: (b: ArrayBuffer) => got.push(new Uint8Array(b)[0]),
    setHevc: vi.fn(),
    startSound: vi.fn(),
    setMuted: vi.fn(),
    resumeSound: vi.fn(),
    stop: vi.fn(),
  };
  return { p, got, el };
}

const msg = (kind: number) => Uint8Array.from([kind, 1, 2]).buffer;

describe("the track's numbers (the page's report to the host)", () => {
  it("maps inbound-rtp onto the report, with decode time and jitter buffer delay since the last", () => {
    const first = {
      framesDecoded: 600,
      framesDropped: 3,
      freezeCount: 2,
      totalFreezesDuration: 0.4,
      totalDecodeTime: 2.4,
      jitterBufferDelay: 18,
      jitterBufferEmittedCount: 600,
      nackCount: 11,
      pliCount: 1,
    };
    expect(TrackStats.report(first, {})).toEqual({
      frames: 600,
      dropped: 3,
      stalls: 2,
      stall_ms: 400,
      decode_ms: 4,
      jitter_ms: 30,
      nacks: 11,
      plis: 1,
      decoder: "track",
    });
    // The next 120 frames took 3 ms each to decode and waited 50 ms in the jitter buffer.
    const next = { ...first, framesDecoded: 720, totalDecodeTime: 2.76, jitterBufferDelay: 24, jitterBufferEmittedCount: 720 };
    const r = TrackStats.report(next, first);
    expect([r.frames, r.decode_ms, r.jitter_ms]).toEqual([720, 3, 50]);
    // Nothing new: no average rather than a division by zero.
    expect(TrackStats.report(first, first).decode_ms).toBe(0);
  });

  it("reads the browser's stats on demand, the last read answering", async () => {
    const receiver = {
      getStats: async () => new Map([["v", { type: "inbound-rtp", kind: "video", framesDecoded: 60, freezeCount: 0 }]]),
    } as unknown as RTCRtpReceiver;
    const s = new TrackStats(receiver);
    expect(s.snapshot()).toBeNull();
    await Promise.resolve();
    await Promise.resolve();
    expect(s.frames).toBe(60);
    expect(s.snapshot()).toMatchObject({ frames: 60, decoder: "track" });
  });
});

describe("the track player", () => {
  const media = () => ({
    stream: {} as MediaStream,
    receiver: { getStats: async () => new Map() } as unknown as RTCRtpReceiver,
  });

  it("shows the channel's picture until a track comes, then the track's", () => {
    const { p, got, el } = inner();
    const t = new TrackPlayer(p as unknown as WatchPlayer | MsePlayer);
    expect((t.element as unknown as FakeEl).contains(el)).toBe(true);
    expect(t.video.style.display).toBe("none");
    // No track (an older engine): every kind goes to the channel's player.
    for (const k of [0, 1, 3, 4]) t.onMessage(msg(k));
    expect(got).toEqual([0, 1, 3, 4]);
    expect(t.stats.frames).toBe(7);

    t.setTrack(media());
    expect(t.video.style.display).toBe("");
    expect((t.video as unknown as FakeEl).plays).toBe(1);
    expect(el.style.display).toBe("none");
    // On the track the pictures never come on the channel; the sound does.
    got.length = 0;
    for (const k of [0, 1, 3, 4]) t.onMessage(msg(k));
    expect(got).toEqual([3, 4]);
    expect(t.stats.frames).toBe(0);

    // A resume without a track (the host turned it off): the channel's picture again.
    t.setTrack(null);
    expect(t.video.style.display).toBe("none");
    expect(el.style.display).toBe("");
    expect(t.video.srcObject).toBeNull();
  });

  it("keeps the sound on the channel's player", () => {
    const { p } = inner();
    const t = new TrackPlayer(p as unknown as WatchPlayer | MsePlayer);
    const fail = () => {};
    t.onSoundFail = fail;
    expect(p.onSoundFail).toBe(fail);
    t.setHevc(true);
    t.startSound();
    t.setMuted(true);
    t.resumeSound();
    t.stop();
    expect([p.setHevc, p.startSound, p.setMuted, p.resumeSound, p.stop].map((f) => f.mock.calls.length)).toEqual([1, 1, 1, 1, 1]);
    expect(p.setMuted).toHaveBeenCalledWith(true);
  });
});

describe("the sound on the audio track (#720)", () => {
  const media = () => ({
    stream: {} as MediaStream,
    receiver: { getStats: async () => new Map() } as unknown as RTCRtpReceiver,
    audio: {} as RTCRtpReceiver,
  });

  it("plays from the <video>: nothing of it reaches the channel's player, after the tap, mutable", () => {
    const { p, got } = inner();
    const t = new TrackPlayer(p as unknown as WatchPlayer | MsePlayer);
    t.setTrack(media(), true);
    for (const k of [0, 1, 3, 4]) t.onMessage(msg(k));
    expect(got).toEqual([]);
    // Silent until the guest's tap (a browser plays sound only after one).
    expect(t.video.muted).toBe(true);
    t.startSound();
    expect(t.video.muted).toBe(false);
    t.setMuted(true);
    expect(t.video.muted).toBe(true);
    t.setMuted(false);
    expect(t.video.muted).toBe(false);
    // A resume whose answer had no audio track: the channel's sound again, the <video> silent.
    t.setTrack(media(), false);
    expect(t.video.muted).toBe(true);
    t.onMessage(msg(4));
    expect(got).toEqual([4]);
  });
});

describe("the host's panel: a track that can't carry the stream", () => {
  it("says what it carries only when the engine says it is too slow", () => {
    const track = { bwe_kbps: 9300, stream_kbps: 15000, slow: true };
    expect(readCarries({ video: "track", track })).toEqual({ kbps: 9300, streamKbps: 15000 });
    expect(readCarries({ video: "track", track: { ...track, slow: false } })).toBeNull();
    expect(readCarries({ video: "channel", track: null })).toBeNull();
    expect(readCarries(undefined)).toBeNull();
    const s = readJoinState({ guests: [{ id: 1, player: 2, via: "internet", link: { track } }], settings: {} });
    expect(s.guests[0].carries).toEqual({ kbps: 9300, streamKbps: 15000 });
    // The setting: on unless the engine says off (an older engine says nothing).
    expect(s.settings.videoTrack).toBe(true);
    expect(readJoinState({ settings: { video_track: false } }).settings.videoTrack).toBe(false);
  });
});
