// Remote Play sound from the engine's records to decoded samples: the audio header (kind 3)
// sets up an Opus decoder, each audio frame (kind 4, [u16 index][Opus packet]) is put back in
// order, decoded, and handed on; a frame that never came is covered by the one before, quieter.
// The same code runs in the decoding worker (rpAudioWorker.ts) or, where that cannot decode, on
// the page (remotePlayStream.ts).

/** Audio frames that may arrive ahead of a missing one before it is given up on. The engine
 *  sends a lost frame's recovered copy (FEC) right after the next ones. */
export const AUDIO_HOLD = 3;

/** Decoders in a row that may fail with no sound out before this device counts as unable. */
const MAX_DECODER_FAILURES = 3;

/** Where decoded sound goes: its channels (arrays the sink may keep or transfer), times `scale`. */
export type AudioSink = (planes: Float32Array[], scale: number) => void;

export interface AudioDecoderLike {
  readonly state: string;
  configure(c: AudioDecoderConfig): void;
  decode(c: EncodedAudioChunk): void;
  close(): void;
}

export interface AudioCodecs {
  makeDecoder(init: AudioDecoderInit): AudioDecoderLike;
  makeChunk(init: EncodedAudioChunkInit): EncodedAudioChunk;
}

const webCodecs: AudioCodecs = {
  makeDecoder: (init) => new AudioDecoder(init),
  makeChunk: (init) => new EncodedAudioChunk(init),
};

export class OpusPipe {
  private audio: AudioDecoderLike | null = null;
  private rate = 48000;
  private frame = 480;
  /** Next frame wanted (unwrapped), frames that came ahead of it, the last one played. */
  private want = -1;
  private ahead = new Map<number, Uint8Array>();
  private lastOut = -1;
  private prev: Float32Array[] | null = null;
  /** Frames covered by the one before (they never came), since the start. */
  concealed = 0;
  /** Frames decoded since the start. */
  decoded = 0;
  private failures = 0;
  private gaveUp = false;

  constructor(
    private readonly sink: AudioSink,
    private readonly codecs: AudioCodecs = webCodecs,
    /** This device can't decode the sound: no AudioDecoder, or no Opus. Said once. */
    private readonly onUnavailable?: () => void,
  ) {}

  /** One engine record: [kind][payload]. */
  record(b: Uint8Array): void {
    if (b[0] === 3) this.header(b.subarray(1));
    else if (b[0] === 4) this.packet(b.subarray(1));
  }

  /** The audio header: [channels][?][u32 rate][u32 samples per frame]. */
  header(header: Uint8Array): void {
    if (this.gaveUp) return;
    const dv = new DataView(header.buffer, header.byteOffset, header.byteLength);
    const channels = header[0];
    this.rate = dv.getUint32(2);
    this.frame = dv.getUint32(6);
    const frameUs = (this.frame / this.rate) * 1e6;
    this.closeDecoder();
    const copy = header.slice();
    let audio: AudioDecoderLike | null = null;
    const init: AudioDecoderInit = {
      output: (data) => {
        this.failures = 0;
        const n = data.numberOfFrames;
        const planes: Float32Array[] = [];
        for (let c = 0; c < data.numberOfChannels; c++) {
          const plane = new Float32Array(n);
          data.copyTo(plane, { planeIndex: c, format: "f32-planar" });
          planes.push(plane);
        }
        const u = Math.round(data.timestamp / frameUs);
        data.close();
        if (audio !== this.audio) return;
        this.decoded++;
        // Frames that never came: the one before, quieter, instead of a hard gap (a click).
        const missing = this.lastOut >= 0 ? Math.min(u - this.lastOut - 1, AUDIO_HOLD) : 0;
        for (let k = 0; k < missing && this.prev; k++) {
          this.concealed++;
          this.sink(this.prev.map((p) => p.slice()), 0.5 / (k + 1));
        }
        this.lastOut = u;
        // The sink may transfer what it is given: keep a copy to cover a gap with.
        this.prev = planes.map((p) => p.slice());
        this.sink(planes, 1);
      },
      error: () => {
        // A new decoder for the same stream, unless they keep failing with nothing played.
        if (audio === this.audio) {
          this.audio = null;
          if (++this.failures >= MAX_DECODER_FAILURES) this.giveUp();
          else this.header(copy);
        }
      },
    };
    try {
      audio = this.codecs.makeDecoder(init);
      audio.configure({ codec: "opus", sampleRate: this.rate, numberOfChannels: channels });
    } catch {
      try {
        audio?.close();
      } catch {
        // Already closed.
      }
      this.giveUp();
      return;
    }
    this.audio = audio;
  }

  private giveUp(): void {
    if (this.gaveUp) return;
    this.gaveUp = true;
    this.onUnavailable?.();
  }

  /** One audio frame: [u16 index][Opus packet]. */
  packet(payload: Uint8Array): void {
    if (!this.audio || this.audio.state !== "configured" || payload.length < 2) return;
    const idx = (payload[0] << 8) | payload[1];
    const data = payload.subarray(2);
    if (this.want < 0) this.want = idx;
    const d = (idx - (this.want & 0xffff)) & 0xffff;
    if (d >= 0x8000) return; // a copy of a frame already played
    if (d > 200) {
      // Far ahead (a long stall): start again from here.
      this.ahead.clear();
      this.want = idx;
      return this.packet(payload);
    }
    const u = this.want + d;
    if (d === 0) {
      this.decode(u, data);
      this.want++;
    } else {
      this.ahead.set(u, data.slice());
      // A frame missing for too long is given up on (the output covers the gap).
      if (this.ahead.size > AUDIO_HOLD) {
        while (!this.ahead.has(this.want)) this.want++;
      }
    }
    for (let next = this.ahead.get(this.want); next; next = this.ahead.get(this.want)) {
      this.ahead.delete(this.want);
      this.decode(this.want, next);
      this.want++;
    }
  }

  private decode(u: number, data: Uint8Array): void {
    this.audio?.decode(this.codecs.makeChunk({ type: "key", timestamp: Math.round(u * (this.frame / this.rate) * 1e6), data }));
  }

  private closeDecoder(): void {
    try {
      this.audio?.close();
    } catch {
      // Already closed.
    }
    this.audio = null;
  }

  /** A new stream follows (the engine sends the header again): the decoder goes. */
  reset(): void {
    this.closeDecoder();
    this.failures = 0;
    this.gaveUp = false;
    this.want = -1;
    this.ahead.clear();
    this.lastOut = -1;
    this.prev = null;
  }

  /** The stream's sample rate (its header's). */
  get sampleRate(): number {
    return this.rate;
  }

  /** Whether a decoder is set up. */
  get open(): boolean {
    return !!this.audio && this.audio.state !== "closed";
  }
}
