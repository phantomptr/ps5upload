// Play Rooms voice (design 7A): the wire frames, the per-speaker jitter buffer, the voice gate and
// push-to-talk. Pure logic, tested without audio; lib/room/roomVoice.ts wires it to the mic,
// WebCodecs (encode) and the wasm Opus decoder (decode, with loss concealment).
//
// Wire (little-endian, like the pad frames):
//   guest → engine  [6][seq u16][level u8][Opus 20 ms mono]
//   engine → guest  [7][n][(speaker u8, seq u16, len u8, Opus) × n]   one bundle per 20 ms tick
// level: loudness 0–255 (255 = full scale, 0 = silence or below −90 dBFS).

export const VOICE_UP = 6;
export const VOICE_DOWN = 7;
export const VOICE_RATE = 48000;
export const VOICE_FRAME_MS = 20;
export const VOICE_FRAME_SAMPLES = (VOICE_RATE * VOICE_FRAME_MS) / 1000;

/** WebCodecs Opus for speech (design 7A.1b): mono, wideband, 24 kbit/s, DTX + in-band FEC. */
export function voiceEncoderConfig(kbps: 24 | 12 = 24, lossPct = 0): AudioEncoderConfig {
  return {
    codec: "opus",
    sampleRate: VOICE_RATE,
    numberOfChannels: 1,
    bitrate: kbps * 1000,
    opus: {
      frameDuration: VOICE_FRAME_MS * 1000,
      complexity: 5,
      signal: "voice",
      usedtx: true,
      useinbandfec: true,
      packetlossperc: Math.max(0, Math.min(100, Math.round(lossPct))),
      application: "voip",
    } as OpusEncoderConfig,
  };
}

/** One mic frame to the engine. */
export function voiceUpFrame(seq: number, level: number, opus: Uint8Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(4 + opus.length);
  out[0] = VOICE_UP;
  out[1] = seq & 0xff;
  out[2] = (seq >> 8) & 0xff;
  out[3] = Math.max(0, Math.min(255, Math.round(level)));
  out.set(opus, 4);
  return out;
}

export interface VoicePart {
  speaker: number;
  seq: number;
  opus: Uint8Array;
}

/** A bundle from the engine (kind 7), or null when it isn't one or is cut short. */
export function readVoiceBundle(buf: ArrayBuffer | Uint8Array): VoicePart[] | null {
  const b = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  if (b.length < 2 || b[0] !== VOICE_DOWN) return null;
  const n = b[1];
  const out: VoicePart[] = [];
  let o = 2;
  for (let i = 0; i < n; i++) {
    if (o + 4 > b.length) return null;
    const speaker = b[o];
    const seq = b[o + 1] | (b[o + 2] << 8);
    const len = b[o + 3];
    o += 4;
    if (o + len > b.length) return null;
    out.push({ speaker, seq, opus: b.subarray(o, o + len) });
    o += len;
  }
  return out;
}

/** The engine's bundle, built (tests and the stand-in engine). */
export function voiceBundle(parts: VoicePart[]): Uint8Array<ArrayBuffer> {
  const size = 2 + parts.reduce((a, p) => a + 4 + p.opus.length, 0);
  const out = new Uint8Array(size);
  out[0] = VOICE_DOWN;
  out[1] = parts.length;
  let o = 2;
  for (const p of parts) {
    out[o] = p.speaker;
    out[o + 1] = p.seq & 0xff;
    out[o + 2] = (p.seq >> 8) & 0xff;
    out[o + 3] = p.opus.length;
    out.set(p.opus, o + 4);
    o += 4 + p.opus.length;
  }
  return out;
}

/** dBFS of a block of samples (−Infinity for silence). */
export function dbfs(samples: Float32Array): number {
  let sum = 0;
  for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i];
  const rms = Math.sqrt(sum / Math.max(1, samples.length));
  return rms > 0 ? 20 * Math.log10(rms) : -Infinity;
}

/** The level byte from dBFS. */
export function levelByte(db: number): number {
  if (!Number.isFinite(db)) return 0;
  return Math.max(0, Math.min(255, Math.round(((db + 90) * 255) / 90)));
}

const seqDiff = (a: number, b: number) => ((a - b + 0x10000 + 0x8000) & 0xffff) - 0x8000;

/**
 * One speaker's jitter buffer: frames by sequence, played every 20 ms. It waits until `target`
 * frames are in before a talk spurt starts, grows the target on late frames (up to 100 ms on a
 * jittery route) and shrinks it slowly. `pop` gives the next frame, `"lost"` for a gap the decoder
 * fills (FEC/concealment), or null while there is nothing to play.
 */
export class JitterBuffer {
  private frames = new Map<number, Uint8Array>();
  private next: number | null = null;
  private playing = false;
  private lateCount = 0;
  private steady = 0;
  /** Frames held before playing (40–60 ms normally, up to 100). */
  target = 2;

  constructor(
    private readonly minTarget = 2,
    private readonly maxTarget = 5,
  ) {
    this.target = minTarget;
  }

  push(seq: number, opus: Uint8Array): void {
    if (this.next != null && this.playing && seqDiff(seq, this.next) < 0) {
      // Too late to play: count it, a jittery link gets a deeper buffer.
      if (++this.lateCount >= 2 && this.target < this.maxTarget) {
        this.target++;
        this.lateCount = 0;
      }
      this.steady = 0;
      return;
    }
    if (this.frames.size >= 50) return;
    this.frames.set(seq, opus);
    if (this.next == null) this.next = seq;
    else if (!this.playing && seqDiff(seq, this.next) < 0) this.next = seq;
  }

  /** Frames waiting. */
  get depth(): number {
    return this.frames.size;
  }

  pop(): Uint8Array | "lost" | null {
    if (this.next == null) return null;
    if (!this.playing) {
      if (this.frames.size < this.target) return null;
      this.playing = true;
    }
    const f = this.frames.get(this.next);
    if (f) {
      this.frames.delete(this.next);
      this.next = (this.next + 1) & 0xffff;
      if (++this.steady >= 250 && this.target > this.minTarget) {
        // Five seconds with nothing late: a frame less of delay.
        this.target--;
        this.steady = 0;
      }
      return f;
    }
    if (this.frames.size === 0) {
      // The talk spurt ended (the sender's gate closed): wait for the next one.
      this.playing = false;
      this.next = null;
      return null;
    }
    this.next = (this.next + 1) & 0xffff;
    return "lost";
  }
}

/** The voice modes a room allows (the host's setting). */
export type RoomVoiceMode = "off" | "ptt" | "open";

export interface TalkState {
  /** This person's mic (off on every join: opt-in each time). */
  mic: boolean;
  /** How they talk: push-to-talk or open mic (open only when the room allows it). */
  how: "ptt" | "open";
  /** The talk key or button is held. */
  held: boolean;
  room: RoomVoiceMode;
  hostMuted: boolean;
  /** The connection is Poor: voice paused for the game's sake. */
  paused: boolean;
}

/** Whether frames go out now (the voice gate decides within that). Push-to-talk keeps 300 ms
 *  after the key is let go so the last word isn't cut. */
export function talking(s: TalkState, releasedAgoMs: number | null = null): boolean {
  if (!s.mic || s.room === "off" || s.hostMuted || s.paused) return false;
  const ptt = s.room === "ptt" || s.how === "ptt";
  if (!ptt) return true;
  return s.held || (releasedAgoMs != null && releasedAgoMs < 300);
}

/** The voice gate (VAD) with a 300 ms hangover: frames over the threshold open it. */
export class VoiceGate {
  private openUntil = 0;
  constructor(private readonly thresholdDb = -48) {}
  /** True while frames should be sent. */
  feed(db: number, now: number): boolean {
    if (db > this.thresholdDb) this.openUntil = now + 300;
    return now < this.openUntil;
  }
}

/** Voice for a connection tier (design 7A.1b): 24 kbit/s, 12 on Playable, paused on Poor. */
export function voiceForTier(tier: "excellent" | "good" | "playable" | "poor" | null): { kbps: 24 | 12; paused: boolean } {
  if (tier === "poor") return { kbps: 12, paused: true };
  if (tier === "playable") return { kbps: 12, paused: false };
  return { kbps: 24, paused: false };
}

/** The game sound while others talk (ducking), as a gain: Off 1, A little −6 dB, More −12 dB. */
export function duckGain(setting: "off" | "little" | "more", someoneTalking: boolean): number {
  if (!someoneTalking || setting === "off") return 1;
  return setting === "more" ? 0.25 : 0.5;
}

/** The push-to-talk key (default the backquote): a key the keyboard-and-mouse play mode already
 *  uses is refused. */
export function pttKeyOk(code: string, mapped: ReadonlySet<string>): boolean {
  return !!code && code !== "Escape" && !mapped.has(code);
}

export const DEFAULT_PTT_KEY = "Backquote";

/** Per-person volume, 0–200 %, as a gain. */
export function personGain(pct: number, muted: boolean): number {
  if (muted) return 0;
  return Math.max(0, Math.min(2, pct / 100));
}

/** Counts bytes both ways for the stats overlay ("voice 24 kb/s in · 12 out"). */
export class VoiceMeter {
  private inBytes: Array<[number, number]> = [];
  private outBytes: Array<[number, number]> = [];
  constructor(private readonly now: () => number = () => Date.now()) {}
  in(n: number): void {
    this.inBytes.push([this.now(), n]);
  }
  out(n: number): void {
    this.outBytes.push([this.now(), n]);
  }
  /** kbit/s over the last 2 s. */
  read(): { inKbps: number; outKbps: number } {
    const t = this.now();
    const keep = (a: Array<[number, number]>) => a.filter(([at]) => t - at < 2000);
    this.inBytes = keep(this.inBytes);
    this.outBytes = keep(this.outBytes);
    const kb = (a: Array<[number, number]>) => Math.round((a.reduce((s, [, n]) => s + n, 0) * 8) / 2000);
    return { inKbps: kb(this.inBytes), outKbps: kb(this.outBytes) };
  }
}
