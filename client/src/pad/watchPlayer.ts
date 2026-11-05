// Watch party: a phone (or any browser on the LAN) watching the Remote Play stream another
// device plays. The engine sends a watcher the same messages as the player's own page
// (rp_stream_api.rs to_spectators): kind 0 the video's codec header, 1 a video access unit,
// 3 the sound's header, 4 an Opus frame. This decodes them onto a canvas and plays the sound.

import { avcDescription, lengthPrefixed, opusInit, opusSamples, opusSegment, videoInit, videoSegment } from "../lib/fmp4";
import { avcCodecString, isKeyframe, supportedHevc } from "../lib/rpCodec";
import { OpusPipe } from "../lib/rpAudioPipe";
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
  /** A problem with the picture; null when it is showing again. */
  onProblem: (p: WatchProblem | null) => void,
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

/** One sound frame as the engine sends it (kind 4): the console's u16 BE frame index, then one
 *  Opus packet (rp_stream StreamSink::audio). The index is not Opus: a decoder fed it as part of
 *  the packet makes noise or nothing. Null for a frame too short to hold a packet. */
export function opusFrame(payload: Uint8Array): { index: number; packet: Uint8Array } | null {
  if (payload.length < 3) return null;
  return { index: (payload[0] << 8) | payload[1], packet: payload.subarray(2) };
}

/** Which sound frames MSE plays: each index once, in order. The engine sends a lost frame's
 *  recovered copy (FEC) after the frames that follow it; MSE times are back to back, so a late
 *  or repeated frame would play twice or out of place, and is left out. */
export class FrameOrder {
  private last = -1;
  /** Whether frame `index` comes after every frame taken so far (16-bit, wrapping). */
  take(index: number): boolean {
    if (this.last >= 0) {
      const d = (index - this.last) & 0xffff;
      if (d === 0 || d >= 0x8000) return false;
    }
    this.last = index;
    return true;
  }
  /** A new stream (the header again). */
  reset(): void {
    this.last = -1;
  }
}

/** The audio header, as long as OpusPipe reads it: [channels][?][u32 rate][u32 samples per frame]
 *  (a shorter one gets 10 ms frames). */
function pipeHeader(h: Uint8Array): Uint8Array {
  if (h.length >= 10) return h;
  const out = new Uint8Array(10);
  out.set(h.subarray(0, 6));
  const dv = new DataView(out.buffer);
  dv.setUint32(6, Math.round(dv.getUint32(2) / 100) || 480);
  return out;
}

/** When each decoded piece of a guest's sound starts (AudioContext seconds). A cushion ahead of
 *  now absorbs the network's jitter: every time the sound runs dry it grows by 20 ms (up to
 *  `max`), and after 10 s without it shrinks back by 10 ms (down to `min`). Sound that would sit
 *  further ahead than the cushion plus SOUND_SLACK is dropped, never played over what is already
 *  scheduled (a burst after a stall used to start on top of it: the same sound twice). A burst
 *  smaller than that still leaves the sound behind the picture, so while the least lead over
 *  TRIM_WINDOW stays more than TRIM_OVER past the cushion, one piece a window is left out until
 *  it is back (measured live: a watcher's first burst kept the sound 210 ms late for good). */
export class SoundClock {
  private next = 0;
  private calmSince = 0;
  /** The least lead (s) since `windowAt`. */
  private minLead = Infinity;
  private windowAt = 0;
  cushion: number;
  underruns = 0;
  dropped = 0;
  constructor(
    private readonly min: number,
    private readonly max = 0.2,
  ) {
    this.cushion = min;
  }
  /** Where `dur` s of sound decoded at `now` plays, or null (dropped). */
  at(now: number, dur: number): number | null {
    if (this.next < now + 0.005) {
      if (this.next > 0) {
        this.underruns++;
        this.cushion = Math.min(this.max, this.cushion + 0.02);
      }
      this.calmSince = now;
      this.next = now + this.cushion;
      this.minLead = Infinity;
      this.windowAt = now;
    } else if (now - this.calmSince > 10 && this.cushion > this.min) {
      this.cushion = Math.max(this.min, this.cushion - 0.01);
      this.calmSince = now;
    }
    const lead = this.next - now;
    if (lead > this.cushion + SOUND_SLACK) {
      this.dropped++;
      return null;
    }
    this.minLead = Math.min(this.minLead, lead);
    if (now - this.windowAt >= TRIM_WINDOW) {
      const trim = this.minLead > this.cushion + TRIM_OVER;
      this.minLead = Infinity;
      this.windowAt = now;
      if (trim) {
        this.dropped++;
        return null;
      }
    }
    const t = this.next;
    this.next += dur;
    return t;
  }
  /** Start over at the next piece (the output was suspended, or a new stream). */
  reset(): void {
    this.next = 0;
  }
}

/** How far past its cushion the sound may run ahead before pieces are dropped, s. */
export const SOUND_SLACK = 0.15;
/** The window (s) over which a lead above the cushion is measured, and by how much (s) the least
 *  lead in it must pass the cushion for one piece to be left out. */
export const TRIM_WINDOW = 0.25;
export const TRIM_OVER = 0.02;

/** Whether this page's AudioDecoder decodes Opus at `rate` and `channels`. */
async function decodesOpus(rate: number, channels: number): Promise<boolean> {
  try {
    if (typeof AudioDecoder === "undefined") return false;
    return !!(await AudioDecoder.isConfigSupported({ codec: "opus", sampleRate: rate, numberOfChannels: channels })).supported;
  } catch {
    return false;
  }
}

/** The events a browser lets start sound (a touch's pointerdown is not one). */
const GESTURES = ["pointerup", "touchend", "click", "keydown"] as const;

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
  /** A problem is on show until the next picture. */
  private problemShown = false;
  private ts = 0;
  /** Sound frames back in order, decoded (lib/rpAudioPipe, as the host's page does). */
  private pipe: OpusPipe | null = null;
  /** Bumped by every audio header: a slower Opus check for an older one is ignored. */
  private audioGen = 0;
  private actx: AudioContext | null = null;
  private clock: SoundClock;
  private soundOn = false;
  /** Gesture listeners waiting to resume a suspended output (iPhone after a call, a tab away). */
  private unarm: (() => void) | null = null;
  /** The sound through MSE where this page has VideoDecoder but no AudioDecoder (Safari before
   *  26 on a secure page, the iPhone app among them). */
  private mseSound: MseSound | null = null;
  /** The sound cannot play on this device. */
  onSoundFail: (() => void) | null = null;

  constructor(
    private onProblem: (p: WatchProblem | null) => void,
    private latency: Latency = "watch",
    onNeedKey?: () => void,
  ) {
    this.keyAsk = new KeyAsker(onNeedKey);
    this.clock = new SoundClock(latency === "player" ? 0.05 : 0.08);
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
      if (!this.actx) {
        const ctx = new AudioContext({ latencyHint: "interactive" });
        this.actx = ctx;
        // Suspended or interrupted later (a call, the tab away): the next tap resumes it.
        ctx.onstatechange = () => {
          if (this.actx === ctx && ctx.state !== "running" && ctx.state !== "closed") this.armResume();
        };
      }
      void this.actx.resume().catch(() => {});
    } catch {
      this.onSoundFail?.();
    }
  }

  get sound(): boolean {
    return this.soundOn;
  }

  /** Sound muted by the guest (the picture and the timing go on). */
  private muted = false;
  setMuted(m: boolean): void {
    this.muted = m;
    this.mseSound?.setMuted(m);
  }

  /** The page is back (visible, focused): sound that stopped meanwhile starts again. */
  resumeSound(): void {
    if (!this.soundOn) return;
    if (this.mseSound) return this.mseSound.resume();
    const ctx = this.actx;
    if (!ctx || ctx.state === "running" || ctx.state === "closed") return;
    this.clock.reset();
    void ctx.resume().catch(() => {});
    this.armResume();
  }

  /** What the sound has done since the start (a session's health, tests). */
  get soundStats(): { decoded: number; concealed: number; underruns: number; dropped: number; cushionMs: number } {
    return {
      decoded: this.pipe?.decoded ?? 0,
      concealed: this.pipe?.concealed ?? 0,
      underruns: this.clock.underruns,
      dropped: this.clock.dropped,
      cushionMs: Math.round(this.clock.cushion * 1000),
    };
  }

  /** Resume the output on the next gesture (a browser may refuse it without one). */
  private armResume(): void {
    if (this.unarm || typeof window === "undefined" || !window.addEventListener) return;
    const go = () => {
      const ctx = this.actx;
      if (!ctx || ctx.state === "closed") return this.unarm?.();
      this.clock.reset();
      void ctx
        .resume()
        .then(() => {
          if (ctx.state === "running") this.unarm?.();
        })
        .catch(() => {});
    };
    for (const e of GESTURES) window.addEventListener(e, go, true);
    this.unarm = () => {
      for (const e of GESTURES) window.removeEventListener(e, go, true);
      this.unarm = null;
    };
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
          if (this.problemShown) {
            this.problemShown = false;
            this.onProblem(null);
          }
        },
        error: () => {
          if (this.video !== v) return;
          this.problemShown = true;
          this.onProblem("decode");
          this.rebuild();
        },
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

  /** The decoder failed, or the browser took it back (a hidden page's hardware decoder): a new one
   *  from the header, and the next keyframe. Without this the picture stays frozen for good. */
  private rebuild(): void {
    this.video = null;
    this.sawKey = false;
    if (!this.header) return;
    void this.openVideo(this.header).then(() => this.keyAsk.waiting());
  }

  private onVideo(au: Uint8Array): void {
    const v = this.video;
    if (v?.state === "closed") {
      if (!this.opening) this.rebuild();
      return;
    }
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
    const gen = ++this.audioGen;
    this.pipe?.reset();
    this.pipe = null;
    this.clock.reset();
    if (this.mseSound) return this.mseSound.setHeader(header);
    const channels = header[0];
    const rate = new DataView(header.buffer, header.byteOffset, header.byteLength).getUint32(2);
    void decodesOpus(rate, channels).then((ok) => {
      if (gen !== this.audioGen) return;
      // No Opus in this decoder: MSE plays it, if it can.
      if (!ok) return this.soundThroughMse(header);
      const pipe = new OpusPipe((planes, scale) => this.play(planes, scale, pipe.sampleRate));
      try {
        pipe.header(pipeHeader(header));
        this.pipe = pipe;
      } catch {
        this.soundThroughMse(header);
      }
    });
  }

  private soundThroughMse(header: Uint8Array): void {
    this.pipe = null;
    if (!this.mseSound) {
      this.mseSound = new MseSound(this.latency, () => this.onSoundFail?.());
      if (this.soundOn) this.mseSound.start();
    }
    this.mseSound.setHeader(header);
  }

  private onAudio(frame: Uint8Array): void {
    if (this.mseSound) return this.mseSound.frame(frame);
    if (!this.soundOn || !this.pipe) return;
    this.pipe.packet(frame);
  }

  /** Decoded sound: scheduled `clock`'s cushion ahead (`scale` < 1: a lost frame covered). */
  private play(planes: Float32Array[], scale: number, rate: number): void {
    const ctx = this.actx;
    if (!ctx || ctx.state !== "running" || !planes.length) return;
    const n = planes[0].length;
    const at = this.clock.at(ctx.currentTime, n / rate);
    if (at === null) return;
    const buf = ctx.createBuffer(planes.length, n, rate);
    const k = this.muted ? 0 : scale;
    planes.forEach((p, c) => buf.copyToChannel((k === 1 ? p : p.map((v) => v * k)) as Float32Array<ArrayBuffer>, c));
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(ctx.destination);
    src.start(at);
  }

  stop(): void {
    try {
      this.video?.close();
    } catch {
      // Already closed.
    }
    this.audioGen++;
    this.pipe?.reset();
    this.pipe = null;
    this.unarm?.();
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

/** The MIME type the sound is sent to MSE as: Opus in fragmented MP4. "Opus" as the MP4 mapping
 *  names it (its sample entry): Safari's MediaSource takes only that spelling (measured, WebKit
 *  27.2: "opus" false, "Opus" true), Chrome takes both. */
export const OPUS_MP4 = 'audio/mp4; codecs="Opus"';
/** The spelling 11.0.2 asked for, tried next (a browser that knows only it). */
export const OPUS_MP4_LOWER = 'audio/mp4; codecs="opus"';

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
  private order = new FrameOrder();

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

  /** One sound frame as the engine sends it ([u16 index][Opus packet], see opusFrame). */
  frame(payload: Uint8Array): void {
    if (!this.on || this.failed || (!this.track.ready && !this.track.backlog)) return;
    const f = opusFrame(payload);
    if (!f || !this.order.take(f.index)) return;
    const seg = opusSegment(this.track.nextSeq(), this.time, f.packet);
    this.time += opusSamples(f.packet);
    this.track.push(seg);
  }

  /** Sound muted by the guest. */
  setMuted(m: boolean): void {
    this.el.muted = m;
  }

  /** The page is back: an element the browser paused meanwhile plays again. */
  resume(): void {
    if (!this.playing || !this.el.paused) return;
    try {
      void this.el.play()?.catch?.(() => {});
    } catch {
      // The next segment's play() tries again.
    }
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
    const init = opusInit(channels, rate);
    if (this.track.open(OPUS_MP4, init) || this.track.open(OPUS_MP4_LOWER, init)) {
      this.time = 0;
      this.order.reset();
    } else this.fail();
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

/** Seconds a track holds behind the playhead before old frames go, and how many it keeps. */
const TRIM_AFTER_S = 10;
const TRIM_KEEP_S = 3;

/** Where old video can be cut: the last keyframe at least `keep` s behind the playhead. MSE removes
 *  everything up to the next keyframe after a cut, and a Remote Play stream sends one only when
 *  asked, so a cut anywhere else takes the frames being played with it and the picture freezes
 *  (#649). Null when no keyframe qualifies. */
export function trimPoint(keys: readonly number[], start: number, now: number, keep = TRIM_KEEP_S): number | null {
  let cut: number | null = null;
  for (const k of keys) if (k > start && k <= now - keep) cut = k;
  return cut;
}

/** One <video>/<audio> element fed fragmented MP4 through a MediaSource. */
export class MseTrack {
  private ms: MediaSource | null = null;
  private sb: SourceBuffer | null = null;
  private queue: Uint8Array[] = [];
  private mime = "";
  private seq = 1;
  ready = false;

  /** A segment could not be added: the frames after it can't play until a keyframe. */
  dropped = false;
  /** Video: when each keyframe plays. Null for sound, where any frame can start. */
  private keys: number[] | null = null;

  constructor(
    readonly el: HTMLMediaElement,
    private onError: () => void,
    private latency: Latency | Edge = "watch",
    /** Video: there's no keyframe to cut at yet, so ask for one. */
    private onNeedKey?: () => void,
  ) {
    if (onNeedKey) this.keys = [];
  }

  /** A keyframe was pushed that plays at `t` seconds. */
  markKey(t: number): void {
    this.keys?.push(t);
  }

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
    if (this.keys) this.keys = [];
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
      if (el.currentTime - start > TRIM_AFTER_S && this.trim(sb, start)) return;
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

  /** Removes what has played, if it can. True when a removal started: `updateend` pumps again. */
  private trim(sb: SourceBuffer, start: number): boolean {
    const now = this.el.currentTime;
    let cut: number | null = now - TRIM_KEEP_S;
    if (this.keys) {
      cut = trimPoint(this.keys, start, now);
      if (cut === null) {
        // A newer keyframe will qualify in a moment: only ask when there's none coming.
        const last = this.keys[this.keys.length - 1] ?? -Infinity;
        if (last <= now - TRIM_KEEP_S) this.onNeedKey?.();
        return false;
      }
      const from = cut;
      this.keys = this.keys.filter((k) => k >= from);
      // Just before it, so a rounded time can't land past the keyframe and take the next stretch.
      cut -= 0.001;
    }
    try {
      sb.remove(start, cut);
      return true;
    } catch {
      return false; // Try again on the next segment.
    }
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
  private problemShown = false;
  private vTime = 0;
  private vLast = 0;
  private soundOn = false;
  /** The sound cannot play on this device. */
  onSoundFail: (() => void) | null = null;

  constructor(
    private onProblem: (p: WatchProblem | null) => void,
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
    this.video = new MseTrack(v, () => this.onProblem("decode"), latency, () => this.keyAsk.waiting());
    // A decode error closes the source, and every segment after it would be dropped in silence.
    v.addEventListener("error", () => this.restartVideo());
    v.addEventListener("playing", () => {
      if (!this.problemShown) return;
      this.problemShown = false;
      this.onProblem(null);
    });
  }

  /** The element failed: open it again from the init segment and wait for the next keyframe. */
  private restartVideo(): void {
    this.problemShown = true;
    this.onProblem("decode");
    const h = this.header;
    if (!h) return;
    this.header = null;
    this.openVideo(h);
    this.sawKey = false;
    this.keyAsk.waiting();
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

  /** The page is back (visible, focused): sound the browser paused meanwhile plays again. */
  resumeSound(): void {
    if (this.soundOn) this.audio.resume();
  }

  /** Sound muted by the guest. */
  setMuted(m: boolean): void {
    this.audio.setMuted(m);
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
    if (key) this.video.markKey(this.vTime / 90000);
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
