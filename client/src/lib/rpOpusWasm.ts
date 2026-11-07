// The sound's last decoder (#720): libopus compiled to WebAssembly (the npm package opus-decoder,
// MIT), for a device whose WebCodecs has no Opus AudioDecoder and whose media player (MSE) takes
// no Opus in MP4 either (Safari 16 on macOS 13). It sits behind OpusPipe's AudioCodecs, so the
// pipe's frame order, FEC copies and concealment work as they do with WebCodecs.
//
// Loaded only when it is needed (a dynamic import): the package carries its WebAssembly inline
// (compressed in a string), so it works the same from the app, the browser build and the engine's
// single-file /join and /pad pages, with no file of its own to fetch.

import type { AudioCodecs, AudioDecoderLike } from "./rpAudioPipe";

type OpusModule = typeof import("opus-decoder");
type OpusDecoderInstance = InstanceType<OpusModule["OpusDecoder"]>;

/** The sample rates libopus decodes at. */
const OPUS_RATES = [8000, 12000, 16000, 24000, 48000] as const;
type OpusRate = (typeof OPUS_RATES)[number];

/** Packets kept while the module loads (~0.5 s of 10 ms frames); older ones go first. */
export const WASM_PENDING_MAX = 50;

let loading: Promise<OpusModule> | null = null;

/** The decoder's module, loaded once. A failed load is tried again next time. */
export function loadOpusWasm(): Promise<OpusModule> {
  loading ??= import("opus-decoder").catch((e: unknown) => {
    loading = null;
    throw e;
  });
  return loading;
}

/** Whether this page can run WebAssembly at all. A page whose policy forbids compiling it fails
 *  later, at the load, and the pipe gives up as it does for any decoder that keeps failing. */
export function wasmAvailable(): boolean {
  return typeof WebAssembly === "object" && typeof WebAssembly.instantiate === "function";
}

/** What OpusPipe hands its decoder: the frame's time and its packet. */
interface Chunk {
  timestamp: number;
  data: Uint8Array;
}

/** Decoded sound in the shape OpusPipe reads from WebCodecs' AudioData. */
function audioData(planes: Float32Array[], n: number, rate: number, timestamp: number): AudioData {
  return {
    numberOfFrames: n,
    numberOfChannels: planes.length,
    sampleRate: rate,
    timestamp,
    format: "f32-planar",
    duration: Math.round((n / rate) * 1e6),
    copyTo(dest: Float32Array, opts: { planeIndex: number }) {
      dest.set(planes[opts.planeIndex].subarray(0, n));
    },
    close() {},
  } as unknown as AudioData;
}

/** An AudioDecoder-like decoder over opus-decoder. configure() starts loading the module; packets
 *  that come before it is ready wait (up to WASM_PENDING_MAX). Output is given as each packet is
 *  decoded; a load or decode that throws is an error, said after the call that caused it. */
export class WasmOpusDecoder implements AudioDecoderLike {
  state: "unconfigured" | "configured" | "closed" = "unconfigured";
  private dec: OpusDecoderInstance | null = null;
  private pending: Chunk[] = [];

  constructor(
    private readonly init: AudioDecoderInit,
    private readonly load: () => Promise<OpusModule> = loadOpusWasm,
  ) {}

  configure(c: AudioDecoderConfig): void {
    if (this.state === "closed") throw new Error("the decoder is closed");
    if (!wasmAvailable()) throw new Error("no WebAssembly here");
    const rate = c.sampleRate;
    if (!OPUS_RATES.includes(rate as OpusRate)) throw new Error(`Opus does not decode at ${rate} Hz`);
    const channels = c.numberOfChannels;
    if (channels < 1 || channels > 2) throw new Error(`${channels} channels: only mono and stereo are decoded`);
    this.state = "configured";
    void this.load()
      .then(async (m) => {
        if (this.state === "closed") return;
        const d = new m.OpusDecoder({ channels, sampleRate: rate as OpusRate });
        await d.ready;
        if (this.state !== "configured") {
          d.free();
          return;
        }
        this.dec = d;
        const waiting = this.pending;
        this.pending = [];
        for (const p of waiting) if (this.state === "configured") this.run(p);
      })
      .catch((e: unknown) => this.fail(e));
  }

  decode(chunk: EncodedAudioChunk): void {
    if (this.state !== "configured") return;
    const c = chunk as unknown as Chunk;
    if (this.dec) return this.run(c);
    this.pending.push({ timestamp: c.timestamp, data: c.data.slice() });
    if (this.pending.length > WASM_PENDING_MAX) this.pending.shift();
  }

  private run(c: Chunk): void {
    const d = this.dec;
    if (!d) return;
    let out: ReturnType<OpusDecoderInstance["decodeFrame"]>;
    try {
      out = d.decodeFrame(c.data);
    } catch (e) {
      return this.fail(e);
    }
    // A packet libopus refused gives nothing: the pipe covers the gap as it does a lost one.
    if (!out.samplesDecoded || !out.channelData.length) return;
    this.init.output(audioData(out.channelData, out.samplesDecoded, out.sampleRate, c.timestamp));
  }

  private fail(e: unknown): void {
    if (this.state === "closed") return;
    this.close();
    // After the call that failed, as WebCodecs says it: the pipe may make a new decoder.
    const err = e instanceof Error ? e : new Error(String(e));
    queueMicrotask(() => this.init.error(err as DOMException));
  }

  close(): void {
    this.state = "closed";
    this.pending = [];
    const d = this.dec;
    this.dec = null;
    try {
      d?.free();
    } catch {
      // Already freed.
    }
  }
}

/** OpusPipe's codecs over the WebAssembly decoder. */
export const wasmOpusCodecs: AudioCodecs = {
  makeDecoder: (init) => new WasmOpusDecoder(init),
  makeChunk: (init) =>
    ({
      type: init.type,
      timestamp: init.timestamp,
      data: init.data instanceof Uint8Array ? init.data : new Uint8Array(init.data as ArrayBuffer),
    }) as unknown as EncodedAudioChunk,
};
