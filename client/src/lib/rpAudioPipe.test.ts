import { describe, expect, it } from "vitest";
import { OpusPipe, type AudioCodecs } from "./rpAudioPipe";

/** A decoder that "decodes" each packet to 480 samples of its first byte / 100 at once. */
function fakeCodecs() {
  const made: Array<{ state: string; closed: number }> = [];
  const codecs: AudioCodecs = {
    makeChunk: (init) => init as unknown as EncodedAudioChunk,
    makeDecoder: (init) => {
      const d = {
        state: "unconfigured",
        closed: 0,
        configure() {
          d.state = "configured";
        },
        decode(c: EncodedAudioChunk) {
          const { timestamp, data } = c as unknown as { timestamp: number; data: Uint8Array };
          const v = data[0] / 100;
          init.output({
            numberOfFrames: 480,
            numberOfChannels: 2,
            timestamp,
            copyTo: (dst: Float32Array) => dst.fill(v),
            close() {},
          } as unknown as AudioData);
        },
        close() {
          d.state = "closed";
          d.closed++;
        },
      };
      made.push(d);
      return d;
    },
  };
  return { codecs, made };
}

const header = () => {
  const h = new Uint8Array(10);
  h[0] = 2;
  new DataView(h.buffer).setUint32(2, 48000);
  new DataView(h.buffer).setUint32(6, 480);
  return h;
};
const frame = (idx: number, v: number) => new Uint8Array([idx >> 8, idx & 0xff, v]);

describe("OpusPipe (Remote Play sound, records to samples)", () => {
  it("says once that it can't decode when this device has no decoder", () => {
    let said = 0;
    const codecs: AudioCodecs = {
      makeChunk: (init) => init as unknown as EncodedAudioChunk,
      makeDecoder: () => {
        throw new ReferenceError("AudioDecoder is not defined");
      },
    };
    const pipe = new OpusPipe(() => {}, codecs, () => said++);
    pipe.header(header());
    pipe.header(header());
    pipe.packet(frame(0, 1));
    expect(said).toBe(1);
    expect(pipe.open).toBe(false);
  });

  it("gives up on decoders that keep failing, instead of making new ones for ever", () => {
    let said = 0;
    let made = 0;
    const errors: Array<(e: DOMException) => void> = [];
    const codecs: AudioCodecs = {
      makeChunk: (init) => init as unknown as EncodedAudioChunk,
      makeDecoder: (init) => {
        made++;
        errors.push(init.error);
        return { state: "configured", configure() {}, decode() {}, close() {} };
      },
    };
    const pipe = new OpusPipe(() => {}, codecs, () => said++);
    pipe.header(header());
    for (let i = 0; i < 10 && said === 0; i++) errors[errors.length - 1](new DOMException("no"));
    expect(said).toBe(1);
    expect(made).toBe(3);
  });

  it("decodes frames in order, puts late ones back in place, and covers lost ones", () => {
    const { codecs } = fakeCodecs();
    const got: Array<[number, number]> = [];
    const pipe = new OpusPipe((planes, scale) => got.push([Math.round(planes[0][0] * 100), scale]), codecs);
    pipe.packet(frame(0, 1)); // before the header: nothing to decode with
    pipe.header(header());
    pipe.packet(frame(10, 10));
    pipe.packet(frame(12, 12)); // 11 late
    pipe.packet(frame(11, 11));
    expect(got).toEqual([
      [10, 1],
      [11, 1],
      [12, 1],
    ]);
    // 13 never comes: after AUDIO_HOLD frames ahead it is given up and covered by 12, quieter.
    for (const i of [14, 15, 16, 17]) pipe.packet(frame(i, i));
    expect(got.slice(3)).toEqual([
      [12, 0.5],
      [14, 1],
      [15, 1],
      [16, 1],
      [17, 1],
    ]);
    expect(pipe.concealed).toBe(1);
    // A copy of one already played is ignored.
    pipe.packet(frame(15, 15));
    expect(got.length).toBe(8);
  });

  it("hands the sink arrays of its own (it may transfer them) and keeps a copy for covering", () => {
    const { codecs } = fakeCodecs();
    const given: Float32Array[][] = [];
    const pipe = new OpusPipe((planes) => given.push(planes), codecs);
    pipe.header(header());
    pipe.packet(frame(0, 50));
    given[0][0].fill(0); // the sink did what it liked with them
    for (const i of [2, 3, 4, 5]) pipe.packet(frame(i, i));
    // Frame 1 covered with frame 0 as it was decoded.
    expect(given[1][0][0]).toBeCloseTo(0.5);
    expect(given[1][0]).not.toBe(given[0][0]);
  });

  it("closes its decoder on reset and starts over at the next header", () => {
    const { codecs, made } = fakeCodecs();
    const got: number[] = [];
    const pipe = new OpusPipe((p) => got.push(Math.round(p[0][0] * 100)), codecs);
    pipe.header(header());
    pipe.packet(frame(100, 1));
    pipe.reset();
    expect(made[0].state).toBe("closed");
    expect(pipe.open).toBe(false);
    pipe.packet(frame(101, 2)); // no decoder until the header comes again
    pipe.header(header());
    pipe.packet(frame(500, 3)); // any index starts the new stream
    expect(got).toEqual([1, 3]);
    expect(made.length).toBe(2);
  });

  it("takes whole engine records", () => {
    const { codecs } = fakeCodecs();
    const got: number[] = [];
    const pipe = new OpusPipe((p) => got.push(Math.round(p[0][0] * 100)), codecs);
    const rec = (kind: number, p: Uint8Array) => new Uint8Array([kind, ...p]);
    pipe.record(rec(3, header()));
    pipe.record(rec(4, frame(7, 40)));
    expect(got).toEqual([40]);
    expect(pipe.sampleRate).toBe(48000);
  });
});
