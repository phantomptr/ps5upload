// Watch party: a phone (or any browser on the LAN) watching the Remote Play stream another
// device plays. The engine sends a watcher the same messages as the player's own page
// (rp_stream_api.rs to_spectators): kind 0 the video's codec header, 1 a video access unit,
// 3 the sound's header, 4 an Opus frame. This decodes them onto a canvas and plays the sound.

import { avcCodecString, isKeyframe, supportedHevc } from "../lib/rpCodec";

export type WatchProblem = "no_hevc" | "decode";

export class WatchPlayer {
  readonly canvas = document.createElement("canvas");
  private ctx = this.canvas.getContext("2d", { alpha: false });
  private hevc = false;
  private header: Uint8Array | null = null;
  private video: VideoDecoder | null = null;
  private opening = false;
  private sawKey = false;
  private ts = 0;
  private audio: AudioDecoder | null = null;
  private actx: AudioContext | null = null;
  private nextAt = 0;
  private soundOn = false;

  constructor(private onProblem: (p: WatchProblem) => void) {
    this.canvas.className = "block h-full w-full object-contain";
    this.canvas.dataset.testid = "watch-canvas";
  }

  /** The engine said which codec the session uses. */
  setHevc(hevc: boolean): void {
    this.hevc = hevc;
  }

  /** Sound needs a tap first (browsers keep a page quiet until then). */
  startSound(): void {
    this.soundOn = true;
    if (!this.actx) this.actx = new AudioContext({ latencyHint: "interactive" });
    void this.actx.resume();
  }

  get sound(): boolean {
    return this.soundOn;
  }

  /** One binary message from the engine. */
  onMessage(buf: ArrayBuffer): void {
    const b = new Uint8Array(buf);
    if (b.length < 1) return;
    const kind = b[0];
    const payload = b.subarray(1);
    if (kind === 0) void this.openVideo(payload.slice());
    else if (kind === 1) this.onVideo(payload);
    else if (kind === 3) this.openAudio(payload.slice());
    else if (kind === 4) this.onAudio(payload);
  }

  private async openVideo(header: Uint8Array): Promise<void> {
    this.header = header;
    if (this.opening) return;
    this.opening = true;
    try {
      const codec = this.hevc ? await supportedHevc(1920, 1080) : avcCodecString(header);
      if (!codec) {
        this.onProblem("no_hevc");
        return;
      }
      try {
        this.video?.close();
      } catch {
        // Already closed.
      }
      const v = new VideoDecoder({
        output: (frame) => {
          const c = this.canvas;
          if (c.width !== frame.displayWidth || c.height !== frame.displayHeight) {
            c.width = frame.displayWidth;
            c.height = frame.displayHeight;
          }
          this.ctx?.drawImage(frame, 0, 0, c.width, c.height);
          frame.close();
        },
        error: () => this.onProblem("decode"),
      });
      v.configure({ codec, optimizeForLatency: true });
      this.video = v;
      this.sawKey = false;
    } catch {
      this.onProblem("decode");
    } finally {
      this.opening = false;
    }
  }

  private onVideo(au: Uint8Array): void {
    const v = this.video;
    if (!v || v.state !== "configured") return;
    const key = isKeyframe(au, this.hevc);
    if (!this.sawKey && !key) return;
    // Behind: a watcher wants now, not the backlog. Wait for the next keyframe.
    if (!key && v.decodeQueueSize > 30) {
      this.sawKey = false;
      return;
    }
    this.sawKey = true;
    let data = au;
    if (key && this.header) {
      data = new Uint8Array(this.header.length + au.length);
      data.set(this.header, 0);
      data.set(au, this.header.length);
    }
    this.ts += 16_667;
    v.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: this.ts, data }));
  }

  private openAudio(header: Uint8Array): void {
    if (header.length < 6) return;
    const channels = header[0];
    const rate = new DataView(header.buffer, header.byteOffset, header.byteLength).getUint32(2);
    try {
      this.audio?.close();
    } catch {
      // Already closed.
    }
    const a = new AudioDecoder({
      output: (d) => this.play(d),
      error: () => {},
    });
    a.configure({ codec: "opus", sampleRate: rate, numberOfChannels: channels });
    this.audio = a;
  }

  private onAudio(frame: Uint8Array): void {
    const a = this.audio;
    if (!a || a.state !== "configured" || !this.soundOn) return;
    a.decode(new EncodedAudioChunk({ type: "key", timestamp: 0, data: frame }));
  }

  private play(d: AudioData): void {
    const ctx = this.actx;
    if (!ctx) {
      d.close();
      return;
    }
    const buf = ctx.createBuffer(d.numberOfChannels, d.numberOfFrames, d.sampleRate);
    for (let c = 0; c < d.numberOfChannels; c++) {
      const ch = new Float32Array(d.numberOfFrames);
      d.copyTo(ch, { planeIndex: c, format: "f32-planar" });
      buf.copyToChannel(ch, c);
    }
    d.close();
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(ctx.destination);
    // A small cushion; far behind (a stall) starts again from now.
    const now = ctx.currentTime;
    if (this.nextAt < now + 0.02 || this.nextAt > now + 0.5) this.nextAt = now + 0.06;
    src.start(this.nextAt);
    this.nextAt += buf.duration;
  }

  stop(): void {
    try {
      this.video?.close();
      this.audio?.close();
    } catch {
      // Already closed.
    }
    void this.actx?.close().catch(() => {});
  }
}
