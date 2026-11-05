// A guest's picture after its decoder failed or was taken back: it used to stay frozen until they
// left and rejoined. No real video here: the decoder is a fake (vitest runs in node).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { WatchPlayer } from "./watchPlayer";

const made: FakeDecoder[] = [];

class FakeDecoder {
  state = "unconfigured";
  decodeQueueSize = 0;
  decoded = 0;
  constructor(readonly init: { output: (f: unknown) => void; error: (e: unknown) => void }) {
    made.push(this);
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

const SPS = [0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0xaa];
const PPS = [0, 0, 0, 1, 0x68, 0xce];
const HEADER = new Uint8Array([0, ...SPS, ...PPS]).buffer;
const KEY = new Uint8Array([1, 0, 0, 0, 1, 0x65, 1, 2]).buffer;
const DELTA = new Uint8Array([1, 0, 0, 0, 1, 0x41, 1, 2]).buffer;
const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  made.length = 0;
  vi.stubGlobal("VideoDecoder", FakeDecoder);
  vi.stubGlobal("EncodedVideoChunk", class {});
  vi.stubGlobal("document", {
    createElement: () => ({ getContext: () => null, dataset: {}, className: "", width: 0, height: 0 }),
  });
  vi.stubGlobal("AudioDecoder", class {});
});
afterEach(() => vi.unstubAllGlobals());

describe("a guest's decoder", () => {
  const setUp = async () => {
    const problems: Array<string | null> = [];
    const asks = { n: 0 };
    const p = new WatchPlayer((x) => problems.push(x), "player", () => asks.n++);
    p.onMessage(HEADER);
    await flush();
    p.onMessage(KEY);
    expect(made).toHaveLength(1);
    expect(made[0].decoded).toBe(1);
    return { p, problems, asks };
  };

  it("comes back after an error: a new one, a keyframe asked for, the problem cleared", async () => {
    const { p, problems, asks } = await setUp();
    made[0].init.error(new Error("bad"));
    expect(problems).toEqual(["decode"]);
    await flush();
    expect(made).toHaveLength(2);
    expect(asks.n).toBeGreaterThan(0);
    // Nothing until the keyframe, then pictures again.
    p.onMessage(DELTA);
    expect(made[1].decoded).toBe(0);
    p.onMessage(KEY);
    expect(made[1].decoded).toBe(1);
    made[1].init.output({ displayWidth: 1, displayHeight: 1, close() {} });
    expect(problems).toEqual(["decode", null]);
  });

  it("comes back when the browser closed it without a word (a hidden page)", async () => {
    const { p } = await setUp();
    made[0].state = "closed";
    p.onMessage(DELTA);
    await flush();
    expect(made).toHaveLength(2);
    expect(made[1].state).toBe("configured");
  });

  it("ignores an error from one it already replaced", async () => {
    const { problems } = await setUp();
    made[0].init.error(new Error("bad"));
    await flush();
    made[0].init.error(new Error("late"));
    expect(problems).toEqual(["decode"]);
  });
});
