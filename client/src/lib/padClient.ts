// A phone as a controller for the Remote Play session another device runs
// (engine/crates/ps5upload-engine/src/pad_link.rs). It sends the pad this phone amounts to (its
// on-screen controller, plus any controller attached to the phone: Backbone, Kishi, GameSir...)
// to the computer's engine on every change and at least every 250 ms, and plays the console's
// rumble on the phone. A dropped or silent socket comes back by itself (a Wi-Fi blip, a phone
// screen that was off) for as long as the computer lets this phone in; let go (neutral) while
// the page is hidden or loses the focus.

import { backoffMs, netFailure } from "./joinProtocol";
import type { MotionReading } from "./motion";
import { gamepadsOf } from "./nativePad";
import { profileFor } from "./padProfiles";
import { IDLE_PAD, mergePads, padFromGamepads, type PadState } from "./remotePlayStream";

/** Keepalive: the pad is sent at least this often (the engine lets go after a second of nothing). */
export const PAD_KEEPALIVE_MS = 250;
/** No message for this long while joined (the engine says its state every second): the link is dead. */
export const PAD_SILENCE_MS = 3500;
/** How long a dropped link is tried again before the page says it is lost. */
export const PAD_RECONNECT_FOR_MS = 120_000;

/** Where the link is, as the page shows it. */
export type PadLinkStage =
  | { kind: "connecting" }
  /** Waiting for the computer to allow this phone. */
  | { kind: "asking" }
  /** Joined: `playing` once the computer's session shows a picture; null host = no session.
   *  `passcode`: the PS5 waits for its user's passcode, asked on the computer (never here). */
  | { kind: "joined"; host: string | null; playing: boolean; passcode?: boolean }
  /** The socket dropped: trying again, the controller stays on screen. */
  | { kind: "reconnecting"; attempt: number }
  | { kind: "refused"; why: string }
  | { kind: "lost" };

export interface PadClientOptions {
  url: string;
  onStage: (s: PadLinkStage) => void;
  /** The on-screen controller's pad (read on every change and every frame). */
  touchPad: () => PadState;
  onLight?: (rgb: [number, number, number]) => void;
  /** The phone's latest motion reading, while motion controls are on. */
  motion?: () => MotionReading | null;
  /** What the console's native keyboard & mouse can do (`{"kbm_caps": …}`): the Touchpad &
   *  keyboard tab is offered only with a mouse. */
  onKbmCaps?: (c: { mouse: boolean; keyboard: boolean; reason: string | null }) => void;
  /** Watch party: this page watches the stream (binary picture and sound arrive) and plays only
   *  once the computer hands it the controller. */
  watch?: {
    onFrame: (data: ArrayBuffer) => void;
    onCodec: (hevc: boolean) => void;
    onControl: (on: boolean) => void;
  };
  /** An internet link (the website's copy of this page): the first message on each new socket,
   *  `{"hello": …}`, carrying what the LAN socket's address has (`t`, `name`, `d`, `w`). */
  hello?: Record<string, string | number>;
  /** The engine allowed this internet phone and gave it a reconnect token (`rt`, added to `hello`
   *  for every later socket): the page keeps it for this link's room. */
  onReconnectToken?: (rt: string) => void;
  /** The socket class: the LAN WebSocket (default), or an internet link's WebRTC transport
   *  (join/rtcTransport, a "pad" channel), whose failures close as `net:<why>`. Tests: a fake. */
  WebSocketImpl?: typeof WebSocket;
  now?: () => number;
  raf?: (cb: () => void) => number;
  cancelRaf?: (id: number) => void;
}

/** The message a stage status from the engine is. */
export function readStatus(text: string): PadLinkStage | { rumble: [number, number] } | { light: [number, number, number] } | null {
  let v: { stage?: string; why?: string; session?: { host?: string; playing?: boolean; passcode?: boolean } | null; rumble?: [number, number]; light?: [number, number, number] };
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (Array.isArray(v.rumble)) return { rumble: [Number(v.rumble[0]) || 0, Number(v.rumble[1]) || 0] };
  if (Array.isArray(v.light)) return { light: v.light };
  if (v.stage === "asking") return { kind: "asking" };
  if (v.stage === "refused") return { kind: "refused", why: String(v.why ?? "") };
  if (v.stage === "joined") {
    return {
      kind: "joined",
      host: v.session?.host ?? null,
      playing: !!v.session?.playing,
      passcode: !!v.session?.passcode,
    };
  }
  return null;
}

/** Same pads (nothing to send). */
export function samePad(a: PadState, b: PadState): boolean {
  return (
    a.buttons === b.buttons &&
    a.l2 === b.l2 &&
    a.r2 === b.r2 &&
    a.lx === b.lx &&
    a.ly === b.ly &&
    a.rx === b.rx &&
    a.ry === b.ry &&
    (a.touch?.[0] ?? -1) === (b.touch?.[0] ?? -1) &&
    (a.touch?.[1] ?? -1) === (b.touch?.[1] ?? -1) &&
    (a.motion?.t ?? -1) === (b.motion?.t ?? -1)
  );
}

type Timer = ReturnType<typeof setTimeout>;

export class PadClient {
  private ws: WebSocket | null = null;
  private last: PadState = { ...IDLE_PAD };
  private lastSent = 0;
  private raf = 0;
  private keepalive: Timer | null = null;
  private watchdog: Timer | null = null;
  private retry: Timer | null = null;
  /** Stop, a refusal or giving up: no more sockets. */
  private done = false;
  private stage: PadLinkStage = { kind: "connecting" };
  /** This phone has been in: a lost socket comes back. */
  private wasIn = false;
  private attempt = 0;
  private deadline = 0;
  private lastHeard = 0;
  /** A watcher holds the controller (a player always does). */
  private control = false;
  /** Let go of everything: the page is hidden or lost the focus. */
  private neutral = false;
  private readonly now: () => number;
  private readonly WS: typeof WebSocket;

  constructor(private o: PadClientOptions) {
    this.now = o.now ?? (() => Date.now());
    this.WS = o.WebSocketImpl ?? WebSocket;
  }

  get currentStage(): PadLinkStage {
    return this.stage;
  }

  private setStage(s: PadLinkStage): void {
    this.stage = s;
    this.o.onStage(s);
  }

  start(): void {
    this.done = false;
    this.setStage({ kind: "connecting" });
    this.connect();
    const raf = this.o.raf ?? ((cb) => window.requestAnimationFrame(cb));
    const tick = () => {
      this.send(false);
      this.raf = raf(tick);
    };
    this.raf = raf(tick);
    // Animation frames stop on a page in the background; timers carry on, more slowly.
    this.keepalive = setInterval(() => this.send(false), PAD_KEEPALIVE_MS);
    this.watchdog = setInterval(() => this.checkSilence(), 1000);
  }

  private connect(): void {
    let ws: WebSocket;
    this.lastHeard = this.now();
    try {
      ws = new this.WS(this.o.url);
    } catch {
      this.dropped();
      return;
    }
    this.ws = ws;
    ws.binaryType = "arraybuffer";
    // A new socket is a new phone to the engine: a watcher is handed the controller again.
    this.control = !this.o.watch;
    if (this.o.watch) this.o.watch.onControl(false);
    ws.onmessage = (e) => {
      if (this.ws !== ws) return;
      this.lastHeard = this.now();
      this.onMessage(e.data);
    };
    ws.onclose = (e) => {
      if (this.ws !== ws) return;
      this.ws = null;
      // An internet link that could not connect at all says why (`refused` "net:<why>").
      const why = netFailure(e);
      if (why && !this.wasIn) {
        this.finish({ kind: "refused", why: `net:${why}` });
        return;
      }
      this.dropped();
    };
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.lastHeard = this.now();
      if (this.o.hello) ws.send(JSON.stringify({ hello: this.o.hello }));
      this.send(true);
    };
  }

  private onMessage(data: unknown): void {
    if (typeof data !== "string") {
      if (data instanceof ArrayBuffer) this.o.watch?.onFrame(data);
      return;
    }
    if (this.o.hello && data.startsWith('{"rt":')) {
      // Allowed over the internet: the engine's own token for coming back after a drop without
      // asking again. Every later socket's hello carries it; the page keeps it for this link.
      try {
        const v = JSON.parse(data) as { rt?: unknown };
        if (typeof v.rt === "string" && /^[0-9a-f]{32}$/.test(v.rt)) {
          this.o.hello.rt = v.rt;
          this.o.onReconnectToken?.(v.rt);
          return;
        }
      } catch {
        // Not JSON: not for us.
      }
    }
    if (data.includes('"kbm_caps"')) {
      try {
        const v = JSON.parse(data) as { kbm_caps?: { mouse?: boolean; keyboard?: boolean; reason?: unknown } };
        if (v.kbm_caps) {
          const c = v.kbm_caps;
          this.o.onKbmCaps?.({ mouse: c.mouse === true, keyboard: c.keyboard === true, reason: typeof c.reason === "string" ? c.reason : null });
          return;
        }
      } catch {
        // Not JSON: not for us.
      }
    }
    if (this.o.watch) {
      try {
        const v = JSON.parse(data) as { watch?: { hevc?: boolean }; control?: boolean };
        if (v.watch) this.o.watch.onCodec(!!v.watch.hevc);
        if (typeof v.control === "boolean") {
          this.control = v.control;
          this.o.watch.onControl(v.control);
          if (v.control) this.send(true);
          return;
        }
      } catch {
        // Not JSON: not for us.
      }
    }
    const m = readStatus(data);
    if (!m) return;
    if ("rumble" in m) {
      const [strong, weak] = m.rumble;
      // A phone has one motor: on while either is, for a moment (the next level replaces it).
      navigator.vibrate?.(strong || weak ? 250 : 0);
    } else if ("light" in m) {
      this.o.onLight?.(m.light);
    } else if (m.kind === "refused") {
      // The engine said no and why: the socket closing after this is not a lost connection.
      this.finish(m);
    } else {
      if (m.kind === "joined") {
        // In (again): the first state goes at once, an asking phone sent none that counted.
        const first = this.stage.kind !== "joined";
        this.wasIn = true;
        this.attempt = 0;
        this.setStage(m);
        if (first) this.send(true);
        return;
      }
      this.setStage(m);
    }
  }

  /** The socket is gone (or never came up): come back if this phone has been in, else stop. */
  private dropped(): void {
    if (this.done) return;
    if (!this.wasIn) {
      this.finish({ kind: "lost" });
      return;
    }
    if (this.stage.kind !== "reconnecting") this.deadline = this.now() + PAD_RECONNECT_FOR_MS;
    if (this.now() >= this.deadline) {
      this.finish({ kind: "lost" });
      return;
    }
    const wait = backoffMs(this.attempt);
    this.setStage({ kind: "reconnecting", attempt: this.attempt });
    this.attempt += 1;
    if (this.retry) clearTimeout(this.retry);
    this.retry = setTimeout(() => {
      this.retry = null;
      if (!this.done) this.connect();
    }, wait);
  }

  /** A socket that went quiet without closing (Wi-Fi gone, a stalled reconnect): drop it. While
   *  asking, nothing comes until the computer answers. */
  private checkSilence(): void {
    const ws = this.ws;
    if (!ws || this.done) return;
    if (this.stage.kind !== "joined" && this.stage.kind !== "reconnecting") return;
    if (this.now() - this.lastHeard <= PAD_SILENCE_MS) return;
    this.ws = null;
    try {
      ws.close();
    } catch {
      // Already closing.
    }
    this.dropped();
  }

  /** The page is back in front: a link that was dropped comes back now, however long it was away
   *  (timers hardly run on a phone with its screen off). */
  wake(): void {
    if (this.stage.kind === "lost" && this.wasIn) {
      this.attempt = 0;
      this.start();
      return;
    }
    if (this.stage.kind === "reconnecting") {
      this.deadline = this.now() + PAD_RECONNECT_FOR_MS;
      this.attempt = 0;
      if (this.retry) {
        clearTimeout(this.retry);
        this.retry = null;
        this.connect();
      }
    }
  }

  /** Let go of everything (the page is hidden or lost the focus), or take the pad back. */
  setNeutral(on: boolean): void {
    if (this.neutral === on) return;
    this.neutral = on;
    this.send(true);
  }

  /** The pad this phone is now: its on-screen controller and any controller attached to it. */
  current(): PadState {
    const pads = padFromGamepads(gamepadsOf(typeof navigator === "undefined" ? undefined : navigator), profileFor);
    const touch = this.o.touchPad();
    return { ...mergePads(pads, touch), touch: touch.touch ?? null, motion: this.o.motion?.() ?? null };
  }

  /** Send the pad when it changed, the keepalive is due, or always (`force`). The on-screen
   *  controller calls it too. `ka`: this page keeps sending, so the engine may take its silence
   *  to mean it is gone (an older app's page sends only changes). */
  send(force: boolean): void {
    const ws = this.ws;
    if (!ws || ws.readyState !== this.WS.OPEN || !this.control) return;
    const p = this.neutral ? { ...IDLE_PAD, touch: null, motion: null } : this.current();
    const now = this.now();
    if (!force && samePad(p, this.last) && now - this.lastSent < PAD_KEEPALIVE_MS) return;
    this.last = p;
    this.lastSent = now;
    ws.send(JSON.stringify({ pad: { ...p, touch: p.touch ?? null, motion: p.motion ?? null }, ka: 1 }));
  }

  /** A watcher's picture needs a keyframe (the player spaces these out, and so does the engine). */
  askKeyframe(): void {
    const ws = this.ws;
    if (ws && ws.readyState === this.WS.OPEN) ws.send(JSON.stringify({ keyframe: true }));
  }

  /** Native keyboard & mouse (the Touchpad & keyboard tab): `{"kbm": …}` or a binary report,
   *  forwarded by the computer's engine to its page. False when it cannot be sent now. */
  sendKbm(m: { kbm: unknown } | Uint8Array<ArrayBuffer>): boolean {
    const ws = this.ws;
    if (!ws || ws.readyState !== this.WS.OPEN || !this.control) return false;
    ws.send(m instanceof Uint8Array ? m : JSON.stringify(m));
    return true;
  }

  /** What the on-screen controller calls on every touch. */
  sendPad(): void {
    this.send(false);
  }

  private clearTimers(): void {
    (this.o.cancelRaf ?? ((id) => window.cancelAnimationFrame(id)))(this.raf);
    if (this.keepalive) clearInterval(this.keepalive);
    if (this.watchdog) clearInterval(this.watchdog);
    if (this.retry) clearTimeout(this.retry);
    this.keepalive = this.watchdog = this.retry = null;
  }

  /** No more sockets: `s` says why. */
  private finish(s: PadLinkStage): void {
    this.done = true;
    this.clearTimers();
    const ws = this.ws;
    this.ws = null;
    try {
      ws?.close();
    } catch {
      // Already closed.
    }
    this.setStage(s);
  }

  stop(): void {
    this.done = true;
    this.clearTimers();
    const ws = this.ws;
    this.ws = null;
    if (ws && ws.readyState === this.WS.OPEN) {
      // Let go of everything before leaving.
      ws.send(JSON.stringify({ pad: { ...IDLE_PAD, touch: null } }));
    }
    ws?.close();
  }
}

/** Where this device's own id lives: how the computer knows it again on a new socket (it let it in
 *  once, or took it out). */
const DEVICE_KEY = "ps5upload.pad.device";

/** This device's id for the link: random, kept on the device, never sent anywhere else. */
export function padDeviceId(
  store: { get: (k: string) => string | null; set: (k: string, v: string) => void },
  random: (a: Uint8Array<ArrayBuffer>) => void = (a) => crypto.getRandomValues(a),
): string {
  const kept = store.get(DEVICE_KEY);
  if (kept && /^[0-9a-f]{32}$/.test(kept)) return kept;
  const b = new Uint8Array(16);
  random(b);
  const id = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  store.set(DEVICE_KEY, id);
  return id;
}
