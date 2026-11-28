// Play Rooms voice in a browser or the apps' web views (design 7A, API.md "Play Rooms chat and
// voice"): the mic → voice gate → push-to-talk → WebCodecs Opus (mono, at the kbps and frame
// length the engine says, DTX + FEC) → `[0x0A]` frames on the connection; `[0x0B]` bundles in → a jitter buffer per speaker → the wasm Opus decoder → each
// person's own volume → the speakers. Nothing is recorded or kept.
//
// The mic needs a secure page (https, or the apps): on a plain-http page this says "insecure" and
// the page tells the person to open the secure invite link. Capture uses a ScriptProcessor (it
// works in every web view and in the single-file guest page, which can't load a worklet file).

import { JitterBuffer, VOICE_FRAME_SAMPLES, VOICE_RATE, VoiceGate, VoiceMeter, dbfs, levelByte, personGain, readVoiceBundle, talking, voiceEncoderConfig, voiceUpFrame, type TalkState } from "./voice";

export type VoiceMicState = "off" | "starting" | "live" | "denied" | "insecure" | "unsupported" | "error";

/** Where voice goes and how the engine hears the mic switch. */
export interface VoiceWire {
  sendBinary(b: Uint8Array<ArrayBuffer>): boolean;
  mic(m: "on" | "off" | "ptt_down" | "ptt_up"): boolean;
}

/** Whether this page may use a mic at all (design 7A.6): a secure context with getUserMedia and
 *  a WebCodecs Opus encoder. */
export function micAvailability(): "ok" | "insecure" | "unsupported" {
  if (typeof window === "undefined") return "unsupported";
  if (!window.isSecureContext) return "insecure";
  if (!navigator.mediaDevices?.getUserMedia || typeof AudioEncoder === "undefined" || typeof AudioData === "undefined") return "unsupported";
  return "ok";
}

type OpusDec = { decodeFrame(d: Uint8Array): { channelData: Float32Array[]; samplesDecoded: number }; ready: Promise<void>; free(): void };

interface Speaker {
  jitter: JitterBuffer;
  gain: GainNode | null;
  playhead: number;
  pct: number;
  muted: boolean;
}

export class RoomVoice {
  private st: VoiceMicState = "off";
  private listeners = new Set<() => void>();
  private ctx: AudioContext | null = null;
  private media: MediaStream | null = null;
  private proc: ScriptProcessorNode | null = null;
  private encoder: AudioEncoder | null = null;
  private gate = new VoiceGate();
  private seq = 0;
  private samples = 0;
  private frameSamples = VOICE_FRAME_SAMPLES;
  private kbps: 24 | 12 = 24;
  private buf = new Float32Array(VOICE_FRAME_SAMPLES);
  private fill = 0;
  private level = 0;
  private releasedAt: number | null = null;
  private speakers = new Map<number, Speaker>();
  private decoder: OpusDec | null = null;
  private loading: Promise<void> | null = null;
  private tick: ReturnType<typeof setInterval> | null = null;
  readonly meter = new VoiceMeter();
  talk: TalkState = { mic: false, how: "ptt", held: false, room: "open", hostMuted: false, paused: false };

  constructor(
    private readonly wire: VoiceWire,
    private readonly loadDecoder: () => Promise<{ OpusDecoder: new (o: { channels: number; sampleRate: number }) => OpusDec }> = () =>
      import("opus-decoder") as unknown as Promise<{ OpusDecoder: new (o: { channels: number; sampleRate: number }) => OpusDec }>,
  ) {}

  readonly subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  readonly getState = (): VoiceMicState => this.st;
  /** Bumped on every change (talk state, volumes): what a view subscribes to. */
  version = 0;
  readonly getVersion = (): number => this.version;

  private set(s: VoiceMicState): void {
    this.st = s;
    this.emit();
  }

  private emit(): void {
    this.version++;
    for (const l of this.listeners) l();
  }

  /** The room's rules and this person's choices, as they change. */
  setTalk(p: Partial<TalkState>): void {
    this.talk = { ...this.talk, ...p };
    this.emit();
  }

  /** The engine's rate for this person (`voice.kbps`, `voice.frame_ms`); 0 = paused. */
  setRate(kbps: number, frameMs: number): void {
    const k: 24 | 12 = kbps === 12 ? 12 : 24;
    const f = frameMs === 40 ? 40 : 20;
    const samples = (VOICE_RATE * f) / 1000;
    if (k === this.kbps && samples === this.frameSamples) return;
    this.kbps = k;
    this.frameSamples = samples;
    this.buf = new Float32Array(samples);
    this.fill = 0;
    try {
      if (this.encoder && this.encoder.state === "configured") this.encoder.configure(voiceEncoderConfig(k, 0, f as 20 | 40));
    } catch {
      // Kept at the old rate: the engine drops what doesn't fit.
    }
  }

  /** Push-to-talk key/button held or let go. */
  hold(on: boolean): void {
    if (on === this.talk.held) return;
    this.talk = { ...this.talk, held: on };
    if (!on) this.releasedAt = performance.now();
    if (this.talk.mic) this.wire.mic(on ? "ptt_down" : "ptt_up");
    this.emit();
  }

  /** Turn the mic on (from the press, for the permission prompt). */
  async start(): Promise<void> {
    if (this.st === "live" || this.st === "starting") return;
    const a = micAvailability();
    if (a !== "ok") {
      this.set(a);
      return;
    }
    this.set("starting");
    try {
      this.media = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: { ideal: 1 }, sampleRate: { ideal: VOICE_RATE } },
      });
    } catch (e) {
      const name = (e as { name?: string })?.name ?? "";
      this.set(name === "NotAllowedError" || name === "SecurityError" ? "denied" : "error");
      return;
    }
    try {
      const ctx = this.audio();
      const enc = new AudioEncoder({
        output: (chunk) => {
          const b = new Uint8Array(chunk.byteLength);
          chunk.copyTo(b);
          const frame = voiceUpFrame(this.seq, this.level, b);
          this.seq = (this.seq + 1) & 0xffff;
          if (this.wire.sendBinary(frame)) this.meter.out(frame.length + 50);
        },
        error: () => this.set("error"),
      });
      enc.configure(voiceEncoderConfig(this.kbps, 0, this.frameSamples === VOICE_FRAME_SAMPLES ? 20 : 40));
      this.encoder = enc;
      const src = ctx.createMediaStreamSource(this.media);
      const proc = ctx.createScriptProcessor(1024, 1, 1);
      const sink = ctx.createGain();
      sink.gain.value = 0;
      proc.onaudioprocess = (e) => this.onSamples(e.inputBuffer.getChannelData(0));
      src.connect(proc);
      proc.connect(sink);
      sink.connect(ctx.destination);
      this.proc = proc;
      await ctx.resume();
      this.talk = { ...this.talk, mic: true };
      this.wire.mic("on");
      this.set("live");
    } catch {
      this.release();
      this.set("error");
    }
  }

  stop(): void {
    if (this.talk.mic) this.wire.mic("off");
    this.talk = { ...this.talk, mic: false, held: false };
    this.release();
    this.set("off");
  }

  private release(): void {
    if (this.proc) this.proc.onaudioprocess = null;
    this.proc?.disconnect();
    this.proc = null;
    this.media?.getTracks().forEach((t) => t.stop());
    this.media = null;
    try {
      if (this.encoder && this.encoder.state !== "closed") this.encoder.close();
    } catch {
      // Closed already.
    }
    this.encoder = null;
  }

  private onSamples(input: Float32Array): void {
    let i = 0;
    while (i < input.length) {
      const n = Math.min(this.frameSamples - this.fill, input.length - i);
      this.buf.set(input.subarray(i, i + n), this.fill);
      this.fill += n;
      i += n;
      if (this.fill === this.frameSamples) {
        this.fill = 0;
        this.frame(this.buf.slice());
      }
    }
  }

  private frame(pcm: Float32Array<ArrayBuffer>): void {
    const enc = this.encoder;
    const now = performance.now();
    const db = dbfs(pcm);
    this.level = levelByte(db);
    const open = this.gate.feed(db, now);
    const may = talking(this.talk, this.releasedAt == null ? null : now - this.releasedAt);
    if (!enc || enc.state !== "configured" || !open || !may) return;
    const data = new AudioData({
      format: "f32-planar",
      sampleRate: VOICE_RATE,
      numberOfFrames: pcm.length,
      numberOfChannels: 1,
      timestamp: Math.round((this.samples * 1e6) / VOICE_RATE),
      data: pcm,
    });
    this.samples += pcm.length;
    enc.encode(data);
    data.close();
  }

  private audio(): AudioContext {
    this.ctx ??= new AudioContext({ sampleRate: VOICE_RATE, latencyHint: "interactive" });
    return this.ctx;
  }

  /** A bundle from the engine (kind 7). */
  onBundle(data: ArrayBuffer): void {
    const parts = readVoiceBundle(data);
    if (!parts) return;
    this.meter.in(data.byteLength + 50);
    for (const p of parts) this.speaker(p.speaker).jitter.push(p.seq, p.opus.slice());
    this.ensurePlaying();
  }

  private speaker(id: number): Speaker {
    let s = this.speakers.get(id);
    if (!s) {
      s = { jitter: new JitterBuffer(), gain: null, playhead: 0, pct: 100, muted: false };
      this.speakers.set(id, s);
    }
    return s;
  }

  /** This person's volume for one speaker (0–200 %) and their own mute (local only). */
  setPerson(id: number, o: { pct?: number; muted?: boolean }): void {
    const s = this.speaker(id);
    if (o.pct != null) s.pct = o.pct;
    if (o.muted != null) s.muted = o.muted;
    if (s.gain) s.gain.gain.value = personGain(s.pct, s.muted);
    this.emit();
  }

  person(id: number): { pct: number; muted: boolean } {
    const s = this.speakers.get(id);
    return { pct: s?.pct ?? 100, muted: s?.muted ?? false };
  }

  private ensurePlaying(): void {
    if (!this.loading) {
      this.loading = this.loadDecoder()
        .then(async (m) => {
          const d = new m.OpusDecoder({ channels: 1, sampleRate: VOICE_RATE });
          await d.ready;
          this.decoder = d;
        })
        .catch(() => {
          this.loading = null;
        });
    }
    this.tick ??= setInterval(() => this.play(), 20);
  }

  private play(): void {
    const d = this.decoder;
    if (!d) return;
    const ctx = this.audio();
    let any = false;
    for (const s of this.speakers.values()) {
      const f = s.jitter.pop();
      if (f == null) continue;
      any = true;
      if (!s.gain) {
        s.gain = ctx.createGain();
        s.gain.gain.value = personGain(s.pct, s.muted);
        s.gain.connect(ctx.destination);
      }
      const at = Math.max(ctx.currentTime + 0.02, s.playhead);
      s.playhead = at + VOICE_FRAME_SAMPLES / VOICE_RATE;
      // A lost frame is a short gap (the decoder in the page has no concealment call).
      if (f === "lost") continue;
      let out: { channelData: Float32Array[]; samplesDecoded: number };
      try {
        out = d.decodeFrame(f);
      } catch {
        continue;
      }
      if (!out.samplesDecoded) continue;
      const b = ctx.createBuffer(1, out.samplesDecoded, VOICE_RATE);
      b.copyToChannel(out.channelData[0].slice(0, out.samplesDecoded) as Float32Array<ArrayBuffer>, 0);
      const src = ctx.createBufferSource();
      src.buffer = b;
      src.connect(s.gain);
      src.start(at);
    }
    if (!any && this.speakers.size && [...this.speakers.values()].every((s) => s.jitter.depth === 0)) {
      // Quiet: stop ticking until the next bundle.
      if (this.tick) clearInterval(this.tick);
      this.tick = null;
    }
  }

  close(): void {
    this.stop();
    if (this.tick) clearInterval(this.tick);
    this.tick = null;
    try {
      this.decoder?.free();
    } catch {
      // Freed already.
    }
    this.decoder = null;
    void this.ctx?.close().catch(() => {});
    this.ctx = null;
  }
}
