// #720 (macOS 13, WebKit 18615): WebCodecs says yes and decodes nothing, no AudioDecoder, and MSE
// takes H.264 but no Opus. The media player's attach, its bound on failed opens, and the sound
// through the WebAssembly decoder. Nothing plays: elements, MediaSource and AudioContext are fakes
// (vitest runs in node); the Opus decoder is the real one, its output only counted.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { OPUS_SINE_PACKETS } from "../lib/testdata/opusSine";
import { FailWindow, MSE_MAX_FAILS, MsePlayer, WasmSound, WatchPlayer, attachSource } from "./watchPlayer";

const hex = (s: string) => Uint8Array.from(s.match(/../g)!.map((b) => parseInt(b, 16)));
/** A real H.264 codec header (Annex B SPS + PPS), as kind 0 carries it. */
const AVC = new Uint8Array([
  ...[0, 0, 0, 1, ...hex("6764002aacb200f0044fcb8088000003000800000303c078c19240")],
  ...[0, 0, 0, 1, ...hex("68ebc3cb22c0")],
]);
const msg = (kind: number, payload: Uint8Array) => new Uint8Array([kind, ...payload]).buffer;

/** The engine's audio header: stereo, 16 bit, 48 kHz, 480 samples a frame. */
const AUDIO = (() => {
  const h = new Uint8Array(14);
  h[0] = 2;
  h[1] = 16;
  new DataView(h.buffer).setUint32(2, 48000);
  new DataView(h.buffer).setUint32(6, 480);
  return h;
})();

/** Real Opus packets (testdata/opusSine.ts says how they were made). */
const opusPackets = () => OPUS_SINE_PACKETS;
const frame4 = (idx: number, p: Uint8Array) => msg(4, new Uint8Array([idx >> 8, idx & 0xff, ...p]));

/** An element that keeps its listeners, so a test can fire "error" at it. */
class FakeEl extends EventTarget {
  className = "";
  dataset: Record<string, string> = {};
  src = "";
  muted = false;
  autoplay = false;
  playsInline = false;
  hidden = false;
  disableRemotePlayback = false;
  paused = true;
  currentTime = 0;
  playbackRate = 1;
  kids: FakeEl[] = [];
  constructor(readonly tag: string) {
    super();
  }
  play() {
    return Promise.resolve();
  }
  removeAttribute(n: string) {
    if (n === "src") this.src = "";
  }
  append(...k: FakeEl[]) {
    this.kids.push(...k);
  }
  querySelector(t: string) {
    return this.kids.find((k) => k.tag === t) ?? null;
  }
  getContext() {
    return null;
  }
}

/** An element whose srcObject takes only a MediaStream, as WebKit before 17's does. */
class OldWebKitEl extends FakeEl {
  get srcObject(): unknown {
    return null;
  }
  set srcObject(v: unknown) {
    if (v !== null) throw new TypeError("The provided value is not of type 'MediaStream'");
  }
}

/** A MediaSource with H.264 and no Opus (Safari 16), counting how many were made. */
function fakeMediaSource(types: (t: string) => boolean) {
  const made: unknown[] = [];
  class FakeMs extends EventTarget {
    static isTypeSupported = types;
    readyState = "closed";
    constructor() {
      super();
      made.push(this);
    }
    endOfStream() {}
  }
  return { FakeMs, made };
}
const noOpus = (t: string) => t.startsWith("video/");

/** An AudioContext that schedules nothing audible: it counts what it was given. */
function fakeAudioContext() {
  const started: number[] = [];
  class FakeCtx {
    static made = 0;
    state = "running";
    destination = {};
    currentTime = 1;
    onstatechange: (() => void) | null = null;
    constructor() {
      FakeCtx.made++;
    }
    resume() {
      return Promise.resolve();
    }
    close() {
      return Promise.resolve();
    }
    createBuffer(ch: number, n: number, rate: number) {
      return { numberOfChannels: ch, length: n, duration: n / rate, copyToChannel() {} };
    }
    createBufferSource() {
      return { buffer: null, connect() {}, start: (t: number) => started.push(t) };
    }
  }
  return { FakeCtx, started };
}

const settle = async () => {
  for (let k = 0; k < 20; k++) await new Promise((r) => setTimeout(r, 0));
};

beforeEach(() => {
  vi.stubGlobal("document", { createElement: (t: string) => new FakeEl(t) });
  vi.stubGlobal("window", { addEventListener() {}, removeEventListener() {} });
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:fake");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("attachSource", () => {
  it("puts the MediaSource on as srcObject where the element takes it", () => {
    const el = Object.assign(new FakeEl("video"), { srcObject: null as unknown });
    const ms = {} as MediaSource;
    expect(attachSource(el as unknown as HTMLMediaElement, ms)).toBe("object");
    expect(el.srcObject).toBe(ms);
    expect(URL.createObjectURL).not.toHaveBeenCalled();
    expect(el.src).toBe("");
  });

  it("falls back to a blob: URL where srcObject throws (WebKit before 17)", () => {
    const el = new OldWebKitEl("video");
    expect(attachSource(el as unknown as HTMLMediaElement, {} as MediaSource)).toBe("url");
    expect(el.src).toBe("blob:fake");
  });

  it("and where the element has no srcObject at all", () => {
    const el = new FakeEl("video");
    expect(attachSource(el as unknown as HTMLMediaElement, {} as MediaSource)).toBe("url");
    expect(el.src).toBe("blob:fake");
  });
});

describe("FailWindow", () => {
  it("is over after max failures within the window, and forgets old ones", () => {
    let t = 0;
    const w = new FailWindow(3, 10_000, () => t);
    expect(w.fail()).toBe(false);
    t = 1000;
    expect(w.fail()).toBe(false);
    t = 10_500; // the first is out of the window
    expect(w.fail()).toBe(false);
    t = 10_800;
    expect(w.fail()).toBe(true);
    w.clear();
    expect(w.fail()).toBe(false);
  });
});

describe("the media player on an element that fails every open (#720)", () => {
  function player() {
    const { FakeMs, made } = fakeMediaSource(noOpus);
    vi.stubGlobal("window", { MediaSource: FakeMs, addEventListener() {}, removeEventListener() {} });
    const problems: Array<string | null> = [];
    const asks = vi.fn();
    const p = new MsePlayer((x) => problems.push(x), "player", asks, () => 0);
    const giveUp = vi.fn();
    p.onGiveUp = giveUp;
    const video = (p.element as unknown as FakeEl).querySelector("video")!;
    return { p, made, problems, giveUp, video };
  }

  it("opens it again a few times, then gives up and says so once", () => {
    const { p, made, problems, giveUp, video } = player();
    p.onMessage(msg(0, AVC));
    expect(made.length).toBe(1);
    // Every open fails at once: what used to loop for ever.
    for (let i = 0; i < 1000; i++) video.dispatchEvent(new Event("error"));
    expect(made.length).toBe(MSE_MAX_FAILS);
    expect(giveUp).toHaveBeenCalledTimes(1);
    expect(p.failed).toBe(true);
    // The problem was said once, not once an error.
    expect(problems).toEqual(["decode"]);
    // The same header again does not start it over.
    p.onMessage(msg(0, AVC));
    expect(made.length).toBe(MSE_MAX_FAILS);
  });

  it("an error raised from inside an open is taken up after it, never by an open inside it", () => {
    const { p, made, giveUp, video } = player();
    p.onMessage(msg(0, AVC));
    // The element errors the moment a source is put on it (synchronously).
    let depth = 0;
    let deepest = 0;
    Object.defineProperty(video, "src", {
      get: () => "",
      set: (v: string) => {
        if (!v) return;
        depth++;
        deepest = Math.max(deepest, depth);
        video.dispatchEvent(new Event("error"));
        depth--;
      },
    });
    video.dispatchEvent(new Event("error"));
    expect(deepest).toBe(1);
    expect(made.length).toBeLessThanOrEqual(MSE_MAX_FAILS);
    expect(giveUp).toHaveBeenCalledTimes(1);
  });

  it("a picture that plays starts the count over", () => {
    const { p, made, giveUp, video } = player();
    p.onMessage(msg(0, AVC));
    for (let i = 0; i < 20; i++) {
      video.dispatchEvent(new Event("error"));
      video.dispatchEvent(new Event("playing"));
    }
    expect(giveUp).not.toHaveBeenCalled();
    expect(made.length).toBe(21);
  });
});

describe("sound with no Opus in WebCodecs or MSE (#720)", () => {
  it("the media player's page: the WebAssembly decoder plays it, and no 'no sound' is said", async () => {
    const { FakeMs } = fakeMediaSource(noOpus);
    vi.stubGlobal("window", { MediaSource: FakeMs, addEventListener() {}, removeEventListener() {} });
    const { FakeCtx, started } = fakeAudioContext();
    vi.stubGlobal("AudioContext", FakeCtx);
    const p = new MsePlayer(() => {}, "player");
    const fail = vi.fn();
    p.onSoundFail = fail;
    p.onMessage(msg(3, AUDIO));
    p.startSound();
    expect(FakeCtx.made).toBe(1); // made in the tap
    const ps = opusPackets();
    ps.slice(0, 4).forEach((pk, i) => p.onMessage(frame4(i, pk)));
    await settle();
    ps.slice(4, 8).forEach((pk, i) => p.onMessage(frame4(i + 4, pk)));
    expect(started.length).toBeGreaterThanOrEqual(6);
    expect(p.soundPlaying).toBe(true);
    expect(fail).not.toHaveBeenCalled();
    p.stop();
  });

  it("WebCodecs' page without AudioDecoder: the same, through its own Web Audio", async () => {
    vi.stubGlobal("AudioDecoder", undefined);
    const { FakeMs } = fakeMediaSource(noOpus);
    vi.stubGlobal("window", { MediaSource: FakeMs, addEventListener() {}, removeEventListener() {} });
    const { FakeCtx, started } = fakeAudioContext();
    vi.stubGlobal("AudioContext", FakeCtx);
    const p = new WatchPlayer(() => {}, "player");
    const fail = vi.fn();
    p.onSoundFail = fail;
    p.startSound();
    p.onMessage(msg(3, AUDIO));
    await settle();
    const ps = opusPackets();
    ps.slice(0, 8).forEach((pk, i) => p.onMessage(frame4(i, pk)));
    await settle();
    ps.slice(8, 12).forEach((pk, i) => p.onMessage(frame4(i + 8, pk)));
    expect(p.soundStats.decoded).toBeGreaterThanOrEqual(10);
    expect(started.length).toBeGreaterThan(0);
    expect(fail).not.toHaveBeenCalled();
    p.stop();
  });

  it("no Web Audio either: the page is told there is no sound, once", () => {
    const { FakeMs } = fakeMediaSource(noOpus);
    vi.stubGlobal("window", { MediaSource: FakeMs, addEventListener() {}, removeEventListener() {} });
    const p = new MsePlayer(() => {}, "player");
    const fail = vi.fn();
    p.onSoundFail = fail;
    p.onMessage(msg(3, AUDIO));
    p.startSound();
    p.startSound();
    expect(fail).toHaveBeenCalledTimes(1);
  });

  it("WasmSound decodes nothing before the tap", async () => {
    const { FakeCtx, started } = fakeAudioContext();
    vi.stubGlobal("AudioContext", FakeCtx);
    const s = new WasmSound("player", vi.fn());
    s.setHeader(AUDIO);
    opusPackets().slice(0, 4).forEach((pk, i) => s.frame(new Uint8Array(frame4(i, pk)).subarray(1)));
    await settle();
    expect(s.stats.decoded).toBe(0);
    expect(started).toEqual([]);
  });
});
