// Watch party: a phone (or any browser on the LAN) watching the Remote Play stream another
// device plays. The engine sends a watcher the same messages as the player's own page
// (rp_stream_api.rs to_spectators): kind 0 the video's codec header, 1 a video access unit,
// 3 the sound's header, 4 an Opus frame. This decodes them onto a canvas and plays the sound.

import { avcDescription, lengthPrefixed, opusInit, opusSamples, opusSegment, videoInit, videoSegment } from "../lib/fmp4";
import { avcCodecString, isKeyframe, supportedHevc } from "../lib/rpCodec";
import { hevcCodecString, spsVisibleRect } from "../lib/rpSps";

export type WatchProblem = "no_hevc" | "decode";

/** How far behind the live picture a page may fall. A watcher can sit half a second back; a
 *  player (Play Together, #641) wants the picture within ~150 ms of the console, so a backlog is
 *  skipped (to the next keyframe) or caught up (seek, a slightly faster playback) at once. */
export type Latency = "watch" | "player";

/** Frames waiting to be decoded before a page skips to the next keyframe. */
export const BACKLOG_LIMIT: Record<Latency, number> = { watch: 30, player: 4 };

/** Seconds behind the end of what MSE has buffered: past `seek` jump to the edge, past `speed`
 *  play a little faster until caught up. */
export const MSE_EDGE: Record<Latency, { seek: number; speed: number; target: number }> = {
  watch: { seek: 0.5, speed: Infinity, target: 0.05 },
  player: { seek: 0.15, speed: 0.06, target: 0.02 },
};

/** What a page whose picture is `lag` s behind the buffered end does: seek to `to`, play at
 *  `rate`, or nothing. */
export function edgeAction(lag: number, end: number, latency: Latency): { seekTo: number | null; rate: number } {
  const e = MSE_EDGE[latency];
  if (lag > e.seek) return { seekTo: end - e.target, rate: 1 };
  if (lag > e.speed) return { seekTo: null, rate: 1.1 };
  return { seekTo: null, rate: 1 };
}

/** WebCodecs exists only on a secure page (https or localhost); the watch link is plain http
 *  on the LAN, so phones go through Media Source Extensions instead (MsePlayer). */
export function makeWatchPlayer(
  onProblem: (p: WatchProblem) => void,
  latency: Latency = "watch",
  onNeedKey?: () => void,
): WatchPlayer | MsePlayer {
  return typeof VideoDecoder !== "undefined"
    ? new WatchPlayer(onProblem, latency, onNeedKey)
    : new MsePlayer(onProblem, latency, onNeedKey);
}

/** The least time between two "send a keyframe" asks from one player. */
export const KEY_ASK_GAP_MS = 1000;

/** Asks for a keyframe while the picture waits for one (#648): a Play Together guest that
 *  skipped frames it could not keep up with has no other way to get the picture back, since
 *  the console sends a keyframe only when asked. At most one ask per KEY_ASK_GAP_MS. */
export class KeyAsker {
  private last = -Infinity;
  constructor(
    private ask: (() => void) | undefined,
    private now: () => number = () => performance.now(),
  ) {}
  /** The picture is waiting for a keyframe. */
  waiting(): void {
    if (!this.ask) return;
    const t = this.now();
    if (t - this.last < KEY_ASK_GAP_MS) return;
    this.last = t;
    this.ask();
  }
}

export class WatchPlayer {
  readonly canvas = document.createElement("canvas");
  get element(): HTMLElement {
    return this.canvas;
  }
  private ctx = this.canvas.getContext("2d", { alpha: false });
  private hevc = false;
  private header: Uint8Array | null = null;
  private video: VideoDecoder | null = null;
  private opening = false;
  private sawKey = false;
  private keyAsk: KeyAsker;
  private ts = 0;
  private audio: AudioDecoder | null = null;
  private actx: AudioContext | null = null;
  private nextAt = 0;
  private soundOn = false;

  constructor(
    private onProblem: (p: WatchProblem) => void,
    private latency: Latency = "watch",
    onNeedKey?: () => void,
  ) {
    this.keyAsk = new KeyAsker(onNeedKey);
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
      const codec = this.hevc ? await hevcFor(header) : avcCodecString(header);
      if (!codec) {
        this.onProblem("no_hevc");
        return;
      }
      // H.264 in AVC form, its parameter sets as the description (WebKitGTK decodes no Annex B).
      const description = this.hevc ? undefined : (avcDescription(header) ?? undefined);
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
      v.configure(description ? { codec, description, optimizeForLatency: true } : { codec, optimizeForLatency: true });
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
    if (!this.sawKey && !key) return this.keyAsk.waiting();
    // Behind: a watcher wants now, not the backlog. Wait for the next keyframe.
    if (!key && v.decodeQueueSize > BACKLOG_LIMIT[this.latency]) {
      this.sawKey = false;
      return this.keyAsk.waiting();
    }
    this.sawKey = true;
    let data = au;
    if (!this.hevc) data = lengthPrefixed(au, false, true);
    else if (key && this.header) {
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
    const far = this.latency === "player" ? 0.2 : 0.5;
    if (this.nextAt < now + 0.02 || this.nextAt > now + far) this.nextAt = now + (this.latency === "player" ? 0.04 : 0.06);
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

type MseCtor = typeof MediaSource;

/** The MediaSource this browser has: ManagedMediaSource on iPhones, MediaSource elsewhere. */
function mediaSourceCtor(): MseCtor | null {
  const w = window as unknown as { ManagedMediaSource?: MseCtor; MediaSource?: MseCtor };
  return w.ManagedMediaSource ?? w.MediaSource ?? null;
}

/** One <video>/<audio> element fed fragmented MP4 through a MediaSource. */
class MseTrack {
  private ms: MediaSource | null = null;
  private sb: SourceBuffer | null = null;
  private queue: Uint8Array[] = [];
  private mime = "";
  private seq = 1;
  ready = false;

  /** A segment could not be added: the frames after it can't play until a keyframe. */
  dropped = false;

  constructor(
    readonly el: HTMLMediaElement,
    private onError: () => void,
    private latency: Latency = "watch",
  ) {}

  /** (Re)starts the element on a new init segment. */
  open(mime: string, init: Uint8Array): boolean {
    const MS = mediaSourceCtor();
    if (!MS || !MS.isTypeSupported(mime)) return false;
    this.close();
    const ms = new MS();
    this.ms = ms;
    this.mime = mime;
    this.queue = [init];
    this.seq = 1;
    this.ready = false;
    ms.addEventListener("sourceopen", () => {
      if (this.ms !== ms) return;
      try {
        const sb = ms.addSourceBuffer(this.mime);
        sb.mode = "segments";
        sb.addEventListener("updateend", () => this.pump());
        sb.addEventListener("error", () => this.onError());
        this.sb = sb;
        this.ready = true;
        this.pump();
      } catch {
        this.onError();
      }
    });
    if ("ManagedMediaSource" in window && MS === (window as unknown as { ManagedMediaSource: MseCtor }).ManagedMediaSource) {
      // iPhone: a managed source needs remote playback off and srcObject.
      this.el.disableRemotePlayback = true;
      (this.el as HTMLMediaElement & { srcObject: unknown }).srcObject = ms;
    } else {
      this.el.src = URL.createObjectURL(ms);
    }
    return true;
  }

  nextSeq(): number {
    return this.seq++;
  }

  /** How many segments wait to be appended. */
  get backlog(): number {
    return this.queue.length;
  }

  push(seg: Uint8Array): void {
    this.queue.push(seg);
    this.pump();
  }

  private pump(): void {
    const sb = this.sb;
    if (!sb || sb.updating || !this.ready) return;
    const el = this.el;
    // Keep only the last few seconds, and stay at the live edge: a watcher wants now.
    if (sb.buffered.length) {
      const end = sb.buffered.end(sb.buffered.length - 1);
      const start = sb.buffered.start(0);
      const act = edgeAction(end - el.currentTime, end, this.latency);
      if (act.seekTo !== null) el.currentTime = Math.max(start, act.seekTo);
      if (el.playbackRate !== act.rate) el.playbackRate = act.rate;
      if (el.currentTime - start > 10) {
        try {
          sb.remove(start, el.currentTime - 3);
          return;
        } catch {
          // Try again on the next segment.
        }
      }
    }
    const seg = this.queue.shift();
    if (!seg) return;
    try {
      sb.appendBuffer(seg as BufferSource);
    } catch {
      // Quota or a closed source: drop it, the next keyframe starts clean.
      this.dropped = true;
    }
    if (el.paused) void el.play().catch(() => {});
  }

  close(): void {
    this.ready = false;
    this.sb = null;
    this.queue = [];
    const ms = this.ms;
    this.ms = null;
    if (ms && ms.readyState === "open") {
      try {
        ms.endOfStream();
      } catch {
        // Already ending.
      }
    }
    if (this.el.src.startsWith("blob:")) URL.revokeObjectURL(this.el.src);
    this.el.removeAttribute("src");
  }
}

/** The watch player for pages without WebCodecs: the same messages, played through MSE. */
export class MsePlayer {
  private video: MseTrack;
  private audio: MseTrack;
  private wrap = document.createElement("div");
  private hevc = false;
  private header: Uint8Array | null = null;
  private sawKey = false;
  private keyAsk: KeyAsker;
  private vTime = 0;
  private vLast = 0;
  private aTime = 0;
  private aHeader: Uint8Array | null = null;
  private soundOn = false;

  constructor(
    private onProblem: (p: WatchProblem) => void,
    private latency: Latency = "watch",
    onNeedKey?: () => void,
  ) {
    this.keyAsk = new KeyAsker(onNeedKey);
    const v = document.createElement("video");
    v.muted = true;
    v.autoplay = true;
    v.playsInline = true;
    v.className = "block h-full w-full object-contain";
    v.dataset.testid = "watch-video";
    const a = document.createElement("audio");
    this.wrap.className = "h-full w-full";
    this.wrap.append(v, a);
    this.video = new MseTrack(v, () => this.onProblem("decode"), latency);
    this.audio = new MseTrack(a, () => {}, latency);
  }

  get element(): HTMLElement {
    return this.wrap;
  }

  setHevc(hevc: boolean): void {
    this.hevc = hevc;
  }

  startSound(): void {
    this.soundOn = true;
    if (this.aHeader) this.openAudio(this.aHeader);
    void this.audio.el.play().catch(() => {});
  }

  get sound(): boolean {
    return this.soundOn;
  }

  onMessage(buf: ArrayBuffer): void {
    const b = new Uint8Array(buf);
    if (b.length < 1) return;
    const kind = b[0];
    const payload = b.subarray(1);
    if (kind === 0) this.openVideo(payload.slice());
    else if (kind === 1) this.onVideo(payload);
    else if (kind === 3) {
      this.aHeader = payload.slice();
      if (this.soundOn) this.openAudio(this.aHeader);
    } else if (kind === 4) this.onAudio(payload);
  }

  private openVideo(header: Uint8Array): void {
    const same = this.header && this.header.length === header.length && this.header.every((x, i) => x === header[i]);
    if (same && this.video.ready) return;
    this.header = header;
    const rect = spsVisibleRect(header, this.hevc) ?? { w: 1920, h: 1080 };
    const v = videoInit(header, this.hevc, rect);
    if (!v) return;
    if (!this.video.open(v.mime, v.init)) {
      this.onProblem(this.hevc ? "no_hevc" : "decode");
      return;
    }
    this.sawKey = false;
    this.vTime = 0;
    this.vLast = 0;
  }

  private onVideo(au: Uint8Array): void {
    if (!this.video.ready && !this.video.backlog) return;
    const key = isKeyframe(au, this.hevc);
    if (this.video.dropped) {
      this.video.dropped = false;
      this.sawKey = false;
    }
    if (!this.sawKey && !key) return this.keyAsk.waiting();
    // Behind: skip to the next keyframe rather than play the backlog.
    if (!key && this.video.backlog > BACKLOG_LIMIT[this.latency]) {
      this.sawKey = false;
      return this.keyAsk.waiting();
    }
    this.sawKey = true;
    // Times from arrival, so a 30 or 60 fps stream both play at their pace.
    const now = performance.now();
    const dur = this.vLast ? Math.min(9000, Math.max(750, Math.round((now - this.vLast) * 90))) : 1500;
    this.vLast = now;
    this.video.push(videoSegment(this.video.nextSeq(), this.vTime, dur, au, this.hevc, key));
    this.vTime += dur;
  }

  private openAudio(header: Uint8Array): void {
    if (header.length < 6) return;
    const channels = header[0];
    const rate = new DataView(header.buffer, header.byteOffset, header.byteLength).getUint32(2);
    if (this.audio.open('audio/mp4; codecs="opus"', opusInit(channels, rate))) this.aTime = 0;
  }

  private onAudio(frame: Uint8Array): void {
    if (!this.soundOn || (!this.audio.ready && !this.audio.backlog)) return;
    const seg = opusSegment(this.audio.nextSeq(), this.aTime, frame);
    this.aTime += opusSamples(frame);
    this.audio.push(seg);
  }

  stop(): void {
    this.video.close();
    this.audio.close();
  }
}

/** The HEVC codec string for this stream: the one its parameter sets name (HDR sessions send
 *  10-bit Main10, which the plain Main strings do not decode), else the first Main one this
 *  browser decodes. */
async function hevcFor(header: Uint8Array): Promise<string | null> {
  const own = hevcCodecString(header);
  if (own && typeof VideoDecoder !== "undefined") {
    try {
      if ((await VideoDecoder.isConfigSupported({ codec: own, codedWidth: 1920, codedHeight: 1080 })).supported) return own;
    } catch {
      // Fall back to the usual strings.
    }
  }
  return supportedHevc(1920, 1080);
}
