// The guest's sound frames as the engine really sends them: [u16 BE frame index][one Opus
// packet], with a lost frame's recovered copy (FEC) arriving after the frames that follow it.
// No real audio plays here: decoders, contexts, elements and MediaSource are fakes (vitest runs
// in node).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FrameOrder, MseSound, OPUS_MP4, SOUND_SLACK, SoundClock, TRIM_OVER, WatchPlayer, opusFrame } from "./watchPlayer";

/** A real 14-byte audio header: stereo, 16 bit, 48 kHz, 480 samples (10 ms) a frame. */
const HEADER = (() => {
  const h = new Uint8Array(14);
  h[0] = 2;
  h[1] = 16;
  new DataView(h.buffer).setUint32(2, 48000);
  new DataView(h.buffer).setUint32(6, 480);
  return h;
})();

/** An Opus packet: TOC 0xfc (CELT FB 20 ms, one frame) then `v`. */
const packet = (v: number) => new Uint8Array([0xfc, v, v]);
/** Frame `idx` as the engine sends it (kind 4 and its payload). */
const msg4 = (idx: number, v: number) => new Uint8Array([4, idx >> 8, idx & 0xff, ...packet(v)]).buffer;
const msg3 = () => new Uint8Array([3, ...HEADER]).buffer;

describe("opusFrame", () => {
  it("splits the frame index from the Opus packet", () => {
    expect(opusFrame(new Uint8Array([0x12, 0x34, 0xfc, 9]))).toEqual({ index: 0x1234, packet: new Uint8Array([0xfc, 9]) });
    expect(opusFrame(new Uint8Array([0, 1]))).toBeNull();
  });
});

describe("FrameOrder", () => {
  it("takes each frame once, in order, across the 16-bit wrap", () => {
    const o = new FrameOrder();
    expect([0xfffe, 0xffff, 0xffff, 0, 0xfffe, 2, 1, 3].map((i) => o.take(i))).toEqual([
      true,
      true,
      false, // a repeat
      true, // wrapped
      false, // from before the wrap
      true, // 1 lost...
      false, // ...its FEC copy came late: left out
      true,
    ]);
    o.reset();
    expect(o.take(0xfffe)).toBe(true);
  });
});

describe("SoundClock", () => {
  it("plays pieces back to back a cushion ahead", () => {
    const c = new SoundClock(0.05);
    expect(c.at(1, 0.01)).toBeCloseTo(1.05);
    expect(c.at(1.001, 0.01)).toBeCloseTo(1.06);
    expect(c.at(1.002, 0.01)).toBeCloseTo(1.07);
    expect(c.underruns).toBe(0);
  });

  it("grows the cushion each time it runs dry, and shrinks it after 10 s of calm", () => {
    const c = new SoundClock(0.05);
    c.at(1, 0.01);
    expect(c.at(2, 0.01)).toBeCloseTo(2.07); // dry: 20 ms more
    expect(c.underruns).toBe(1);
    expect(c.at(3, 0.01)).toBeCloseTo(3.09);
    expect(c.cushion).toBeCloseTo(0.09);
    // Calm: pieces keep coming in time for 10 s.
    let t = 3;
    while (t < 13.5) {
      t += 0.01;
      c.at(t, 0.01);
    }
    expect(c.cushion).toBeCloseTo(0.08);
    expect(c.underruns).toBe(2);
  });

  it("drops a burst's excess instead of starting it over what is already scheduled", () => {
    const c = new SoundClock(0.05);
    const starts: number[] = [];
    // 40 frames (400 ms) arrive at once after a stall.
    for (let i = 0; i < 40; i++) {
      const at = c.at(5, 0.01);
      if (at !== null) starts.push(at);
    }
    // Never two pieces at the same time: each starts where the one before ends.
    starts.forEach((s, i) => expect(s).toBeCloseTo(5.05 + i * 0.01));
    expect(starts[starts.length - 1]).toBeLessThanOrEqual(5 + 0.05 + SOUND_SLACK + 1e-9);
    expect(c.dropped).toBe(40 - starts.length);
    expect(c.dropped).toBeGreaterThan(0);
  });
});

describe("SoundClock after a burst", () => {
  it("brings a lead the burst left back down to the cushion (the sound would stay behind the picture)", () => {
    const c = new SoundClock(0.05);
    // The first burst (measured live: 16 frames at once when a watcher's sound started).
    for (let i = 0; i < 16; i++) c.at(1, 0.01);
    let lead = 0;
    let t = 1;
    // Then steady 10 ms frames for 8 s.
    while (t < 9) {
      t += 0.01;
      const at = c.at(t, 0.01);
      if (at !== null) lead = at - t;
    }
    expect(lead).toBeLessThanOrEqual(0.05 + TRIM_OVER + 0.01 + 1e-9);
    expect(c.underruns).toBe(0);
  });

  it("leaves a lead within the cushion alone", () => {
    const c = new SoundClock(0.05);
    let kept = 0;
    for (let i = 1; i <= 400; i++) if (c.at(1 + i * 0.01, 0.01) !== null) kept++;
    expect(c.dropped).toBe(0);
    expect(kept).toBe(400);
  });
});

describe("the sound's MSE type", () => {
  it("asks for Opus in MP4 as Safari spells it first, then as 11.0.2 did", () => {
    const asked: string[] = [];
    class FakeMs extends EventTarget {
      static isTypeSupported = (t: string) => (asked.push(t), t === 'audio/mp4; codecs="opus"');
      readyState = "closed";
      endOfStream() {}
    }
    vi.stubGlobal("document", { createElement: () => fakeEl() });
    vi.stubGlobal("window", { MediaSource: FakeMs });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:fake");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    const fail = vi.fn();
    const s = new MseSound("player", fail);
    s.setHeader(HEADER);
    s.start();
    expect(asked).toEqual([OPUS_MP4, 'audio/mp4; codecs="opus"']);
    expect(OPUS_MP4).toBe('audio/mp4; codecs="Opus"');
    expect(fail).not.toHaveBeenCalled();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });
});

describe("MseSound with the engine's frames", () => {
  let appended: Uint8Array[];
  beforeEach(() => {
    appended = [];
    vi.stubGlobal("document", { createElement: () => fakeEl() });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:fake");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    class FakeMs extends EventTarget {
      static isTypeSupported = (t: string) => t === OPUS_MP4;
      readyState = "open";
      addSourceBuffer() {
        const sb = Object.assign(new EventTarget(), {
          mode: "",
          updating: false,
          buffered: { length: 0 },
          appendBuffer: (b: Uint8Array) => {
            appended.push(b);
            queueMicrotask(() => sb.dispatchEvent(new Event("updateend")));
          },
        });
        return sb;
      }
      endOfStream() {}
    }
    vi.stubGlobal("window", { MediaSource: FakeMs });
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("appends the Opus packet without the index, each frame once and in order", async () => {
    const s = new MseSound("player", vi.fn());
    s.setHeader(HEADER);
    s.start();
    const ms = (s.el as unknown as { src: string; _ms?: EventTarget }).src;
    expect(ms).toBe("blob:fake");
    // The MediaSource opens.
    const made = (URL.createObjectURL as unknown as { mock: { calls: unknown[][] } }).mock.calls[0][0] as EventTarget;
    made.dispatchEvent(new Event("sourceopen"));
    for (const [i, v] of [
      [7, 1],
      [8, 2],
      [8, 2], // repeated
      [10, 4],
      [9, 3], // late (FEC)
      [11, 5],
    ])
      s.frame(new Uint8Array(msg4(i, v)).subarray(1));
    for (let k = 0; k < 10; k++) await Promise.resolve();
    // The init segment, then frames 7, 8, 10, 11, each ending with its packet (no index).
    expect(appended.length).toBe(5);
    const tails = appended.slice(1).map((seg) => [...seg.subarray(seg.length - 3)]);
    expect(tails).toEqual([[...packet(1)], [...packet(2)], [...packet(4)], [...packet(5)]]);
  });
});

function fakeEl() {
  return {
    className: "",
    dataset: {} as Record<string, string>,
    src: "",
    paused: true,
    currentTime: 0,
    playbackRate: 1,
    disableRemotePlayback: false,
    play: () => Promise.resolve(),
    removeAttribute(n: string) {
      if (n === "src") this.src = "";
    },
    getContext: () => null,
  };
}

describe("WatchPlayer (WebCodecs) with the engine's frames", () => {
  let decoded: Uint8Array[];
  let starts: number[];
  let now: number;
  beforeEach(() => {
    decoded = [];
    starts = [];
    now = 10;
    vi.stubGlobal("document", { createElement: () => fakeEl() });
    vi.stubGlobal("window", { addEventListener() {}, removeEventListener() {} });
    vi.stubGlobal(
      "EncodedAudioChunk",
      class {
        constructor(init: { timestamp: number; data: Uint8Array }) {
          Object.assign(this, init);
        }
      },
    );
    class FakeDecoder {
      static isConfigSupported = async (c: { codec: string }) => ({ supported: c.codec === "opus" });
      state = "unconfigured";
      constructor(private init: AudioDecoderInit) {}
      configure() {
        this.state = "configured";
      }
      decode(c: { timestamp: number; data: Uint8Array }) {
        decoded.push(c.data.slice());
        this.init.output({
          numberOfFrames: 480,
          numberOfChannels: 2,
          sampleRate: 48000,
          timestamp: c.timestamp,
          copyTo: (d: Float32Array) => d.fill(0),
          close() {},
        } as unknown as AudioData);
      }
      close() {
        this.state = "closed";
      }
    }
    vi.stubGlobal("AudioDecoder", FakeDecoder);
    class FakeCtx {
      state = "running";
      destination = {};
      onstatechange: (() => void) | null = null;
      get currentTime() {
        return now;
      }
      resume() {
        return Promise.resolve();
      }
      close() {
        return Promise.resolve();
      }
      createBuffer(_ch: number, n: number, rate: number) {
        return { duration: n / rate, copyToChannel() {} };
      }
      createBufferSource() {
        return { buffer: null, connect() {}, start: (t: number) => starts.push(t) };
      }
    }
    vi.stubGlobal("AudioContext", FakeCtx);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  async function ready(): Promise<WatchPlayer> {
    const p = new WatchPlayer(() => {}, "player");
    p.startSound();
    p.onMessage(msg3());
    for (let k = 0; k < 5; k++) await Promise.resolve();
    return p;
  }

  it("decodes the Opus packet without the index, each frame once (a late FEC copy is not played twice)", async () => {
    const p = await ready();
    for (const [i, v] of [
      [1, 1],
      [2, 2],
      [2, 2], // repeated
      [4, 4],
      [3, 3], // late, put back in place
      [5, 5],
    ])
      p.onMessage(msg4(i, v));
    expect(decoded.map((d) => [...d])).toEqual([1, 2, 3, 4, 5].map((v) => [...packet(v)]));
    expect(p.soundStats.decoded).toBe(5);
  });

  it("never schedules two pieces over each other, even for a burst", async () => {
    const p = await ready();
    for (let i = 0; i < 60; i++) p.onMessage(msg4(i, 1));
    const sorted = [...starts].sort((a, b) => a - b);
    for (let i = 1; i < sorted.length; i++) expect(sorted[i] - sorted[i - 1]).toBeGreaterThanOrEqual(0.01 - 1e-9);
    expect(p.soundStats.dropped).toBe(60 - starts.length);
  });

  it("does nothing before the tap", async () => {
    const p = new WatchPlayer(() => {}, "player");
    p.onMessage(msg3());
    for (let k = 0; k < 5; k++) await Promise.resolve();
    p.onMessage(msg4(1, 1));
    expect(decoded).toEqual([]);
  });
});
