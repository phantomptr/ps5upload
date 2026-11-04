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
export interface Edge {
  seek: number;
  speed: number;
  target: number;
}

export const MSE_EDGE: Record<Latency, Edge> = {
  watch: { seek: 0.5, speed: Infinity, target: 0.05 },
  player: { seek: 0.15, speed: 0.06, target: 0.02 },
};

/** The sound's edge (#657). A seek restarts a browser's audio output, which takes longer to
 *  start than an Opus frame lasts (iPhone Safari most of all): chasing the sound as tightly
 *  as the picture seeked it on nearly every frame and it never got to play. It catches up by
 *  playing faster, and seeks only after a real stall. */
export const SOUND_EDGE: Edge = { seek: 0.8, speed: 0.25, target: 0.12 };

/** What a page whose picture is `lag` s behind the buffered end does: seek to `to`, play at
 *  `rate`, or nothing. */
export function edgeAction(lag: number, end: number, latency: Latency | Edge): { seekTo: number | null; rate: number } {
  const e = typeof latency === "string" ? MSE_EDGE[latency] : latency;
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
  /** The decoder has given sound (a later decode error is not a missing codec). */
  private heard = false;
  /** The sound through MSE where this page has VideoDecoder but no AudioDecoder (Safari before
   *  26 on a secure page, the iPhone app among them). */
  private mseSound: MseSound | null = null;
  /** The sound cannot play on this device. */
  onSoundFail: (() => void) | null = null;

  constructor(
    private onProblem: (p: WatchProblem) => void,
    private latency: Latency = "watch",
    onNeedKey?: () => void,
  ) {
    this.keyAsk = new KeyAsker(onNeedKey);
    // Made now, so the tap that starts the sound also starts this element (iPhone).
    if (typeof AudioDecoder === "undefined") this.mseSound = new MseSound(latency, () => this.onSoundFail?.());
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
    playThroughSilentSwitch();
    if (this.mseSound) {
      this.mseSound.start();
      return;
    }
    try {
      if (!this.actx) this.actx = new AudioContext({ latencyHint: "interactive" });
      void this.actx.resume().catch(() => {});
    } catch {
      this.onSoundFail?.();
    }
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
    this.audio = null;
    if (!this.mseSound) {
      try {
        const a = new AudioDecoder({
          output: (d) => this.play(d),
          // No Opus in this decoder: MSE plays it, if it can.
          error: () => {
            if (this.audio === a && !this.heard) this.soundThroughMse(header);
          },
        });
        a.configure({ codec: "opus", sampleRate: rate, numberOfChannels: channels });
        this.audio = a;
        return;
      } catch {
        // No Opus in this decoder: MSE plays it, if it can.
      }
    }
    this.soundThroughMse(header);
  }

  private soundThroughMse(header: Uint8Array): void {
    this.audio = null;
    if (!this.mseSound) {
      this.mseSound = new MseSound(this.latency, () => this.onSoundFail?.());
      if (this.soundOn) this.mseSound.start();
    }
    this.mseSound.setHeader(header);
  }

  private onAudio(frame: Uint8Array): void {
    if (this.mseSound) return this.mseSound.frame(frame);
    const a = this.audio;
    if (!a || a.state !== "configured" || !this.soundOn) return;
    a.decode(new EncodedAudioChunk({ type: "key", timestamp: 0, data: frame }));
  }

  private play(d: AudioData): void {
    this.heard = true;
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
    this.mseSound?.stop();
  }
}

/** iPhone: the ring/silent switch mutes Web Audio unless the page says it plays media
 *  (Safari 16.4+ has navigator.audioSession). Elsewhere a no-op. */
function playThroughSilentSwitch(): void {
  try {
    const s = (navigator as Navigator & { audioSession?: { type: string } }).audioSession;
    if (s && s.type !== "playback") s.type = "playback";
  } catch {
    // Not here.
  }
}

type MseCtor = typeof MediaSource;

/** Whether a MediaSource takes `mime` (a throwing isTypeSupported counts as no). */
function mseSupports(MS: MseCtor, mime: string): boolean {
  try {
    return MS.isTypeSupported(mime);
  } catch {
    return false;
  }
}

/** The MIME type the sound is sent to MSE as: Opus in fragmented MP4. */
export const OPUS_MP4 = 'audio/mp4; codecs="opus"';

/**
 * The sound through Media Source Extensions: an <audio> element fed Opus in fragmented MP4.
 * Browsers keep a page quiet until a tap, so nothing opens before start() (call it from the
 * tap itself: iPhone Safari lets an element play only when play() runs inside the gesture).
 * `onFail` says the sound cannot play here (no Opus in MP4, or the element refused it).
 */
export class MseSound {
  readonly el: HTMLAudioElement;
  private track: MseTrack;
  private header: Uint8Array | null = null;
  private time = 0;
  private on = false;
  private failed = false;

  constructor(
    latency: Latency,
    private onFail: () => void,
    el: HTMLAudioElement = document.createElement("audio"),
  ) {
    this.el = el;
    // Never the picture's edge: see SOUND_EDGE. A watcher's looser edge stays as it was.
    this.track = new MseTrack(el, () => this.fail(), latency === "player" ? SOUND_EDGE : latency);
  }

  get playing(): boolean {
    return this.on && !this.failed;
  }

  /** The codec header (channels, rate) arrived. */
  setHeader(header: Uint8Array): void {
    this.header = header;
    if (this.on) this.open();
  }

  /** The tap: open the track if the header is here, and play inside the gesture. */
  start(): void {
    if (this.on) return;
    this.on = true;
    if (this.header) this.open();
    if (this.failed) return;
    try {
      void this.el.play()?.catch?.(() => {});
    } catch {
      // No media here (a test page): the next segment's play() tries again.
    }
  }

  frame(frame: Uint8Array): void {
    if (!this.on || this.failed || (!this.track.ready && !this.track.backlog)) return;
    const seg = opusSegment(this.track.nextSeq(), this.time, frame);
    this.time += opusSamples(frame);
    this.track.push(seg);
  }

  stop(): void {
    this.on = false;
    this.track.close();
  }

  private open(): void {
    const h = this.header;
    if (!h || h.length < 6 || this.failed) return;
    const channels = h[0];
    const rate = new DataView(h.buffer, h.byteOffset, h.byteLength).getUint32(2);
    if (this.track.open(OPUS_MP4, opusInit(channels, rate))) this.time = 0;
    else this.fail();
  }

  private fail(): void {
    if (this.failed) return;
    this.failed = true;
    this.onFail();
  }
}

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
    private latency: Latency | Edge = "watch",
  ) {}

  /** (Re)starts the element on a new init segment. */
  open(mime: string, init: Uint8Array): boolean {
    const MS = mediaSourceCtor();
    if (!MS || !mseSupports(MS, mime)) return false;
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
  private audio: MseSound;
  private wrap = document.createElement("div");
  private hevc = false;
  private header: Uint8Array | null = null;
  private sawKey = false;
  private keyAsk: KeyAsker;
  private vTime = 0;
  private vLast = 0;
  private soundOn = false;
  /** The sound cannot play on this device. */
  onSoundFail: (() => void) | null = null;

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
    this.audio = new MseSound(latency, () => this.onSoundFail?.());
    this.wrap.className = "h-full w-full";
    this.wrap.append(v, this.audio.el);
    this.video = new MseTrack(v, () => this.onProblem("decode"), latency);
  }

  get element(): HTMLElement {
    return this.wrap;
  }

  setHevc(hevc: boolean): void {
    this.hevc = hevc;
  }

  startSound(): void {
    this.soundOn = true;
    this.audio.start();
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
    else if (kind === 3) this.audio.setHeader(payload.slice());
    else if (kind === 4) this.audio.frame(payload);
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

  stop(): void {
    this.video.close();
    this.audio.stop();
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
