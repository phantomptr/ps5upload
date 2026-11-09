// #720: an iPhone XR (iOS 18) joining Play Together over the LAN had WebCodecs take the stream and
// fail every decode: 0 pictures drawn, for good. A page whose WebCodecs never drew a picture moves
// the picture to the system's media player, as the macOS 13 host does. Fakes only (vitest runs in
// node): the decoder, the elements and the MediaSource.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { WEBCODECS_FAILS_BEFORE_MSE, WatchPlayer } from "./watchPlayer";

const decoders: FakeDecoder[] = [];
class FakeDecoder {
  state = "unconfigured";
  decodeQueueSize = 0;
  decoded = 0;
  constructor(readonly init: { output: (f: unknown) => void; error: (e: unknown) => void }) {
    decoders.push(this);
  }
  configure() {
    this.state = "configured";
  }
  decode() {
    this.decoded++;
  }
  close() {
    this.state = "closed";
  }
}

class FakeEl extends EventTarget {
  className = "";
  dataset: Record<string, string> = {};
  style: Record<string, string> = {};
  src = "";
  muted = false;
  autoplay = false;
  playsInline = false;
  disableRemotePlayback = false;
  paused = true;
  currentTime = 0;
  playbackRate = 1;
  width = 0;
  height = 0;
  kids: FakeEl[] = [];
  constructor(readonly tag: string) {
    super();
  }
  play() {
    return Promise.resolve();
  }
  removeAttribute() {}
  append(...k: FakeEl[]) {
    this.kids.push(...k);
  }
  getContext() {
    return null;
  }
}

const sources: unknown[] = [];
class FakeMs extends EventTarget {
  static isTypeSupported = (t: string) => t.startsWith("video/");
  readyState = "closed";
  constructor() {
    super();
    sources.push(this);
  }
  endOfStream() {}
}

const hex = (s: string) => Uint8Array.from(s.match(/../g)!.map((b) => parseInt(b, 16)));
/** A real H.264 codec header (Annex B SPS + PPS), as kind 0 carries it. */
const AVC = new Uint8Array([
  ...[0, 0, 0, 1, ...hex("6764002aacb200f0044fcb8088000003000800000303c078c19240")],
  ...[0, 0, 0, 1, ...hex("68ebc3cb22c0")],
]);
const msg = (kind: number, payload: Uint8Array) => new Uint8Array([kind, ...payload]).buffer;
const KEY = msg(1, new Uint8Array([0, 0, 0, 1, 0x65, 1, 2]));
const flush = () => new Promise((r) => setTimeout(r, 0));

function stub(withMse: boolean) {
  vi.stubGlobal("VideoDecoder", FakeDecoder);
  vi.stubGlobal("EncodedVideoChunk", class {});
  vi.stubGlobal("AudioDecoder", class {});
  vi.stubGlobal("document", { createElement: (t: string) => new FakeEl(t) });
  vi.stubGlobal(
    "window",
    withMse ? { MediaSource: FakeMs, addEventListener() {}, removeEventListener() {} } : { addEventListener() {}, removeEventListener() {} },
  );
}

beforeEach(() => {
  decoders.length = 0;
  sources.length = 0;
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:fake");
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

/** A guest's player with its header in, and the decoder failing on `fails` keyframes. */
async function failing(fails: number) {
  const problems: Array<string | null> = [];
  const asks = vi.fn();
  const p = new WatchPlayer((x) => problems.push(x), "player", asks);
  const wrap = p.element as unknown as FakeEl;
  p.onMessage(msg(0, AVC));
  await flush();
  for (let i = 0; i < fails; i++) {
    p.onMessage(KEY);
    decoders[decoders.length - 1].init.error(new Error("EncodingError: Decoder failure"));
    await flush();
  }
  return { p, wrap, problems, asks };
}

describe("a page whose WebCodecs never draws a picture (#720)", () => {
  it("plays the picture through the system's media player after a few failed decodes", async () => {
    stub(true);
    const { p, wrap, problems, asks } = await failing(WEBCODECS_FAILS_BEFORE_MSE);
    expect(p.onMse).toBe(true);
    // In the same place as the canvas, which is hidden; one media source on the codec header.
    const [canvas, mse] = wrap.kids;
    expect(canvas.tag).toBe("canvas");
    expect(canvas.style.display).toBe("none");
    expect(mse.tag).toBe("div");
    expect(sources).toHaveLength(1);
    // A keyframe asked for it, and no new decoder made after the move.
    expect(asks).toHaveBeenCalled();
    const made = decoders.length;
    p.onMessage(KEY);
    expect(decoders.length).toBe(made);
    expect(decoders.every((d) => d.decoded <= 1)).toBe(true);
    // The host's log names the media player.
    expect(p.stats.snapshot().decoder).toBe("mse");
    // "The picture could not be played here" goes once the media player plays.
    expect(problems[problems.length - 1]).toBe("decode");
    mse.kids[0].dispatchEvent(new Event("playing"));
    expect(problems[problems.length - 1]).toBeNull();
  });

  it("keeps trying WebCodecs before that", async () => {
    stub(true);
    const { p } = await failing(WEBCODECS_FAILS_BEFORE_MSE - 1);
    expect(p.onMse).toBe(false);
    expect(sources).toHaveLength(0);
    expect(decoders).toHaveLength(WEBCODECS_FAILS_BEFORE_MSE);
  });

  it("never moves a picture WebCodecs has drawn: a later failure rebuilds the decoder", async () => {
    stub(true);
    const p = new WatchPlayer(() => {}, "player", () => {});
    p.onMessage(msg(0, AVC));
    await flush();
    p.onMessage(KEY);
    decoders[0].init.output({ displayWidth: 2, displayHeight: 2, close() {} });
    for (let i = 0; i < WEBCODECS_FAILS_BEFORE_MSE + 2; i++) {
      p.onMessage(KEY);
      decoders[decoders.length - 1].init.error(new Error("bad"));
      await flush();
    }
    expect(p.onMse).toBe(false);
    expect(sources).toHaveLength(0);
  });

  it("stays on WebCodecs where there is no media player", async () => {
    stub(false);
    const { p } = await failing(WEBCODECS_FAILS_BEFORE_MSE + 2);
    expect(p.onMse).toBe(false);
    expect(decoders.length).toBe(WEBCODECS_FAILS_BEFORE_MSE + 3);
  });
});
