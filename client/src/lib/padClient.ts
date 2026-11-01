// A phone as a controller for the Remote Play session another device runs
// (engine/crates/ps5upload-engine/src/pad_link.rs). It sends the pad this phone amounts to (its
// on-screen controller, plus any controller attached to the phone: Backbone, Kishi, GameSir...)
// to the computer's engine on every change, and plays the console's rumble on the phone.

import type { MotionReading } from "./motion";
import { gamepadsOf } from "./nativePad";
import { profileFor } from "./padProfiles";
import { IDLE_PAD, mergePads, padFromGamepads, type PadState } from "./remotePlayStream";

/** Where the link is, as the page shows it. */
export type PadLinkStage =
  | { kind: "connecting" }
  /** Waiting for the computer to allow this phone. */
  | { kind: "asking" }
  /** Joined: `playing` once the computer's session shows a picture; null host = no session.
   *  `passcode`: the PS5 waits for its user's passcode, asked on the computer (never here). */
  | { kind: "joined"; host: string | null; playing: boolean; passcode?: boolean }
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

export class PadClient {
  private ws: WebSocket | null = null;
  private last: PadState = { ...IDLE_PAD };
  private raf = 0;
  private closed = false;
  /** The engine said no and why: the socket closing after that is not a lost connection. */
  private refused = false;
  /** A watcher holds the controller (a player always does). */
  private control = false;

  constructor(private o: PadClientOptions) {}

  start(): void {
    this.o.onStage({ kind: "connecting" });
    let ws: WebSocket;
    try {
      ws = new WebSocket(this.o.url);
    } catch {
      this.o.onStage({ kind: "lost" });
      return;
    }
    this.ws = ws;
    ws.binaryType = "arraybuffer";
    this.control = !this.o.watch;
    ws.onmessage = (e) => {
      if (typeof e.data !== "string") {
        if (e.data instanceof ArrayBuffer) this.o.watch?.onFrame(e.data);
        return;
      }
      if (e.data.includes('"kbm_caps"')) {
        try {
          const v = JSON.parse(e.data) as { kbm_caps?: { mouse?: boolean; keyboard?: boolean; reason?: unknown } };
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
          const v = JSON.parse(e.data) as { watch?: { hevc?: boolean }; control?: boolean };
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
      const m = readStatus(e.data);
      if (!m) return;
      if ("rumble" in m) {
        const [strong, weak] = m.rumble;
        // A phone has one motor: on while either is, for a moment (the next level replaces it).
        navigator.vibrate?.(strong || weak ? 250 : 0);
      } else if ("light" in m) {
        this.o.onLight?.(m.light);
      } else {
        if (m.kind === "refused") this.refused = true;
        this.o.onStage(m);
      }
    };
    ws.onclose = () => {
      this.ws = null;
      if (!this.closed && !this.refused) this.o.onStage({ kind: "lost" });
    };
    ws.onopen = () => this.send(true);
    const tick = () => {
      this.send(false);
      this.raf = window.requestAnimationFrame(tick);
    };
    this.raf = window.requestAnimationFrame(tick);
  }

  /** The pad this phone is now: its on-screen controller and any controller attached to it. */
  current(): PadState {
    const pads = padFromGamepads(gamepadsOf(typeof navigator === "undefined" ? undefined : navigator), profileFor);
    const touch = this.o.touchPad();
    return { ...mergePads(pads, touch), touch: touch.touch ?? null, motion: this.o.motion?.() ?? null };
  }

  /** Send the pad when it changed (or always, `force`). The on-screen controller calls it too. */
  send(force: boolean): void {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN || !this.control) return;
    const p = this.current();
    if (!force && samePad(p, this.last)) return;
    this.last = p;
    ws.send(JSON.stringify({ pad: { ...p, touch: p.touch ?? null, motion: p.motion ?? null } }));
  }

  /** Native keyboard & mouse (the Touchpad & keyboard tab): `{"kbm": …}` or a binary report,
   *  forwarded by the computer's engine to its page. False when it cannot be sent now. */
  sendKbm(m: { kbm: unknown } | Uint8Array<ArrayBuffer>): boolean {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN || !this.control) return false;
    ws.send(m instanceof Uint8Array ? m : JSON.stringify(m));
    return true;
  }

  /** What the on-screen controller calls on every touch. */
  sendPad(): void {
    this.send(false);
  }

  stop(): void {
    this.closed = true;
    window.cancelAnimationFrame(this.raf);
    const ws = this.ws;
    this.ws = null;
    if (ws && ws.readyState === WebSocket.OPEN) {
      // Let go of everything before leaving.
      ws.send(JSON.stringify({ pad: { ...IDLE_PAD, touch: null } }));
    }
    ws?.close();
  }
}
