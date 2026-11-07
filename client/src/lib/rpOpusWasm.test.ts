// The WebAssembly Opus decoder (#720) on real Opus: testdata/opusSine.ts, a 1 kHz stereo sine in
// 10 ms CELT frames as the PS5 sends them. Nothing plays: the samples are only counted and measured.
import { describe, expect, it, vi } from "vitest";

import { OpusPipe } from "./rpAudioPipe";
import { WASM_PENDING_MAX, WasmOpusDecoder, loadOpusWasm, wasmOpusCodecs } from "./rpOpusWasm";
import { OPUS_SINE_PACKETS } from "./testdata/opusSine";

const packets = () => OPUS_SINE_PACKETS;

/** The engine's audio header: stereo, 16 bit, 48 kHz, 480 samples a frame. */
const HEADER = (() => {
  const h = new Uint8Array(14);
  h[0] = 2;
  h[1] = 16;
  new DataView(h.buffer).setUint32(2, 48000);
  new DataView(h.buffer).setUint32(6, 480);
  return h;
})();

/** Frame `idx` as the engine sends it: [u16 BE index][packet]. */
const frame = (idx: number, p: Uint8Array) => {
  const out = new Uint8Array(p.length + 2);
  out[0] = idx >> 8;
  out[1] = idx & 0xff;
  out.set(p, 2);
  return out;
};

/** Rising zero crossings: a 1 kHz tone has one a millisecond. */
function rising(s: Float32Array): number {
  let n = 0;
  for (let i = 1; i < s.length; i++) if (s[i - 1] < 0 && s[i] >= 0) n++;
  return n;
}

const rms = (s: Float32Array) => Math.sqrt(s.reduce((a, v) => a + v * v, 0) / s.length);

const settle = async () => {
  for (let k = 0; k < 20; k++) await new Promise((r) => setTimeout(r, 0));
};

describe("the WebAssembly Opus decoder", () => {
  it("is in this build and decodes the fixture's packets in Node", async () => {
    const { OpusDecoder } = await loadOpusWasm();
    const d = new OpusDecoder({ channels: 2, sampleRate: 48000 });
    await d.ready;
    const ps = packets();
    expect(ps.length).toBe(21);
    expect(ps[0][0]).toBe(0xf4); // CELT FB 10 ms, stereo: the PS5's own
    const all = ps.map((p) => d.decodeFrame(p));
    d.free();
    for (const o of all) {
      expect(o.errors).toEqual([]);
      expect(o.samplesDecoded).toBe(480);
      expect(o.channelData.length).toBe(2);
    }
    // Past the encoder's start-up: a 1 kHz tone, loud, in both channels.
    const tail = new Float32Array(480 * 10);
    all.slice(8, 18).forEach((o, i) => tail.set(o.channelData[0], i * 480));
    expect(rising(tail)).toBeGreaterThanOrEqual(98);
    expect(rising(tail)).toBeLessThanOrEqual(102);
    expect(rms(tail)).toBeGreaterThan(0.05);
    expect(rms(all[12].channelData[1])).toBeGreaterThan(0.05);
  });

  it("drives OpusPipe unchanged: in order, a late copy put back, a lost frame covered", async () => {
    const out: Array<{ n: number; ch: number; scale: number }> = [];
    const pipe = new OpusPipe((planes, scale) => out.push({ n: planes[0].length, ch: planes.length, scale }), wasmOpusCodecs);
    pipe.header(HEADER);
    const ps = packets();
    // Frames before the module is ready wait for it.
    pipe.packet(frame(100, ps[0]));
    pipe.packet(frame(101, ps[1]));
    await settle();
    expect(out.length).toBe(2);
    // 103 before 102 (the engine's FEC copy comes late): put back in order.
    pipe.packet(frame(103, ps[3]));
    pipe.packet(frame(102, ps[2]));
    expect(out.length).toBe(4);
    // 105 never comes: once AUDIO_HOLD more are in, it is covered by 104, quieter.
    pipe.packet(frame(104, ps[4]));
    for (let i = 106; i <= 109; i++) pipe.packet(frame(i, ps[i - 100]));
    expect(pipe.concealed).toBe(1);
    expect(pipe.decoded).toBe(9);
    expect(out.every((o) => o.n === 480 && o.ch === 2)).toBe(true);
    expect(out.filter((o) => o.scale < 1).length).toBe(1);
    pipe.reset();
  });

  it("keeps at most WASM_PENDING_MAX packets while it loads, the newest", async () => {
    let release: (m: Awaited<ReturnType<typeof loadOpusWasm>>) => void = () => {};
    const gate = new Promise<Awaited<ReturnType<typeof loadOpusWasm>>>((r) => (release = r));
    const stamps: number[] = [];
    const d = new WasmOpusDecoder({ output: (a) => stamps.push(a.timestamp), error: () => {} }, () => gate);
    d.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 2 });
    const p = packets()[5];
    for (let i = 0; i < WASM_PENDING_MAX + 10; i++) d.decode(wasmOpusCodecs.makeChunk({ type: "key", timestamp: i, data: p }));
    release(await loadOpusWasm());
    await settle();
    expect(stamps.length).toBe(WASM_PENDING_MAX);
    expect(stamps[0]).toBe(10);
    d.close();
  });

  it("refuses what libopus cannot decode at configure, as WebCodecs would", () => {
    const d = new WasmOpusDecoder({ output: () => {}, error: () => {} });
    expect(() => d.configure({ codec: "opus", sampleRate: 44100, numberOfChannels: 2 })).toThrow();
    expect(() => d.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 6 })).toThrow();
  });

  it("a module that never loads ends with the pipe giving up, said once", async () => {
    const unavailable = vi.fn();
    const failing = {
      makeDecoder: (init: AudioDecoderInit) => new WasmOpusDecoder(init, () => Promise.reject(new Error("CSP"))),
      makeChunk: wasmOpusCodecs.makeChunk,
    };
    const pipe = new OpusPipe(() => {}, failing, unavailable);
    pipe.header(HEADER);
    await settle();
    expect(unavailable).toHaveBeenCalledTimes(1);
  });

  it("no WebAssembly at all: the pipe gives up at once", () => {
    vi.stubGlobal("WebAssembly", undefined);
    const unavailable = vi.fn();
    const pipe = new OpusPipe(() => {}, wasmOpusCodecs, unavailable);
    pipe.header(HEADER);
    expect(unavailable).toHaveBeenCalledTimes(1);
    vi.unstubAllGlobals();
  });
});
