// A DualSense's touchpad for Remote Play (#589). The Gamepad API gives a controller's buttons
// and sticks but never the fingers on its touchpad, so they are read from the controller's own
// input reports:
//
//   - the desktop app (macOS, Windows, Linux) reads them natively (src-tauri
//     commands/pad_touchpad.rs) while a stream plays and sends `pad-touchpad` events;
//   - a browser with WebHID (Chrome, Edge) reads them once the person picks the controller
//     (Settings › Controllers, a user gesture), and again on later visits without asking.
//
// Either way the result is one finger (in the console's touchpad space, like the on-screen
// controller's) and the touchpad click, which state/remotePlay.ts adds to the pad it sends.
// The desktop app also reads the pad's motion sensors from the same reports (`pad-motion`
// events), sent with the pad like a phone's (lib/motion.ts).
// Read-only here: the only writes (a DualSense-mode session's triggers) are the app's.

import { listen } from "@tauri-apps/api/event";

import type { MotionReading } from "./motion";

import { invoke } from "./invokeLogged";
import { isMobile } from "./platform";
import { touchAt } from "./remotePlayStream";
import { isTauriEnv, safeUnlisten } from "./tauriEnv";

/** The touchpad as the console gets it: one finger (x 0-1919, y 0-941) and the click. */
export interface PadTouch {
  touch: [number, number] | null;
  click: boolean;
}

const IDLE: PadTouch = { touch: null, click: false };
let state: PadTouch = IDLE;
/** Called on every change (the stream sends the pad then, so a short tap is never missed). */
let notify: (() => void) | null = null;

/** The controller's touchpad right now. */
export function padTouchState(): PadTouch {
  return state;
}

/** The controller's latest motion reading (desktop app), or null. Read with the pad each
 *  frame: a change alone does not send the pad. */
let motion: MotionReading | null = null;

export function padMotion(): MotionReading | null {
  return motion;
}

function update(next: PadTouch): void {
  if (next.click === state.click && next.touch?.[0] === state.touch?.[0] && next.touch?.[1] === state.touch?.[1]) {
    return;
  }
  state = next;
  notify?.();
}

/** A DualSense point (x 0-1919, y 0-1079) in the console's touchpad space. */
export function touchFromRaw(raw: readonly [number, number] | null | undefined): [number, number] | null {
  return raw ? touchAt(raw[0] / 1919, raw[1] / 1079) : null;
}

// ── Report parsing (WebHID; the desktop app has the same in Rust) ──────────────────────────

export const SONY_VID = 0x054c;
/** DualSense, DualSense Edge. */
export const DUALSENSE_PIDS = [0x0ce6, 0x0df2] as const;

export interface Finger {
  id: number;
  x: number;
  y: number;
}

export interface TouchReport {
  fingers: [Finger | null, Finger | null];
  click: boolean;
}

/** The touchpad in a DualSense input report, report id first: USB 0x01 (64 bytes) or
 *  Bluetooth 0x31 (78 bytes); null for anything else (the short Bluetooth 0x01 report has no
 *  touchpad). Same layout as commands/pad_touchpad.rs. */
export function parseDualSenseReport(b: Uint8Array): TouchReport | null {
  let o: number;
  if (b[0] === 0x01 && b.length >= 64) o = 1;
  else if (b[0] === 0x31 && b.length >= 78) o = 2;
  else return null;
  const finger = (at: number): Finger | null => {
    const p = o + at;
    if (b[p] & 0x80) return null;
    return { id: b[p] & 0x7f, x: b[p + 1] | ((b[p + 2] & 0x0f) << 8), y: (b[p + 2] >> 4) | (b[p + 3] << 4) };
  };
  return { fingers: [finger(32), finger(36)], click: (b[o + 9] & 0x02) !== 0 };
}

/** Follows the first finger down until it lifts (a second finger never makes the point jump),
 *  then reports the lift before taking up another. Same rule as the Rust side. */
export class Follower {
  private following: number | null = null;
  private lifted = false;

  next(r: TouchReport): { touch: [number, number] | null; click: boolean } {
    const down = r.fingers.filter((f): f is Finger => !!f);
    let f: Finger | null = null;
    if (this.following !== null) {
      f = down.find((d) => d.id === this.following) ?? null;
      if (!f) {
        this.following = null;
        this.lifted = true;
      }
    } else if (this.lifted) {
      this.lifted = false;
    } else if (down[0]) {
      f = down[0];
      this.following = f.id;
    }
    return { touch: f ? [Math.min(f.x, 1919), Math.min(f.y, 1079)] : null, click: r.click };
  }
}

// ── The desktop app ────────────────────────────────────────────────────────────────────────

/** What the app was last told; null before the first call, so the off sent at start-up always
 *  reaches it (a reader left running by a page reloaded mid-stream is stopped). */
let nativeOn: boolean | null = null;
let nativeUnlisten: (() => unknown) | null = null;
let motionUnlisten: (() => unknown) | null = null;
let chain: Promise<unknown> = Promise.resolve();

function setNative(on: boolean): void {
  if (on === nativeOn) return;
  nativeOn = on;
  chain = chain.then(async () => {
    try {
      if (on) {
        if (!nativeUnlisten) {
          nativeUnlisten = await listen<{ touch?: [number, number] | null; click?: boolean }>("pad-touchpad", (e) =>
            update({ touch: touchFromRaw(e.payload?.touch), click: !!e.payload?.click }),
          );
        }
        if (!motionUnlisten) {
          motionUnlisten = await listen<MotionReading | null>("pad-motion", (e) => {
            motion = e.payload ?? null;
          });
        }
        await invoke("pad_touchpad_set", { on: true });
      } else {
        await invoke("pad_touchpad_set", { on: false });
        if (nativeUnlisten) safeUnlisten(nativeUnlisten);
        nativeUnlisten = null;
        if (motionUnlisten) safeUnlisten(motionUnlisten);
        motionUnlisten = null;
        motion = null;
        update(IDLE);
      }
    } catch {
      // Best effort: the rest of the controller plays the same without it.
    }
  });
}

// ── A browser with WebHID ──────────────────────────────────────────────────────────────────

interface HidInputReportEventLike extends Event {
  reportId: number;
  data: DataView;
}
interface HidDeviceLike extends EventTarget {
  opened: boolean;
  vendorId: number;
  productId: number;
  open(): Promise<void>;
}
interface HidLike {
  getDevices(): Promise<HidDeviceLike[]>;
  requestDevice(o: { filters: Array<{ vendorId: number; productId?: number }> }): Promise<HidDeviceLike[]>;
}

function hid(): HidLike | null {
  if (typeof navigator === "undefined") return null;
  return ((navigator as Navigator & { hid?: HidLike }).hid ?? null) as HidLike | null;
}

/** A browser that can read the touchpad itself once the person picks the controller (not the
 *  app, which reads it natively). */
export function webHidTouchpadAvailable(): boolean {
  return !isTauriEnv() && !!hid();
}

const watched = new WeakSet<HidDeviceLike>();
let hidDevices = 0;

const isDualSense = (d: HidDeviceLike) =>
  d.vendorId === SONY_VID && (DUALSENSE_PIDS as readonly number[]).includes(d.productId);

async function watch(d: HidDeviceLike): Promise<boolean> {
  if (watched.has(d)) return true;
  try {
    if (!d.opened) await d.open();
  } catch {
    return false;
  }
  watched.add(d);
  hidDevices++;
  const follower = new Follower();
  d.addEventListener("inputreport", (ev) => {
    const e = ev as HidInputReportEventLike;
    const bytes = new Uint8Array(e.data.byteLength + 1);
    bytes[0] = e.reportId;
    bytes.set(new Uint8Array(e.data.buffer, e.data.byteOffset, e.data.byteLength), 1);
    const r = parseDualSenseReport(bytes);
    if (!r) return;
    const n = follower.next(r);
    update({ touch: touchFromRaw(n.touch), click: n.click });
  });
  return true;
}

/** The controllers this site was already allowed to read: read them again (no prompt). */
async function reopenGranted(): Promise<void> {
  const h = hid();
  if (!h) return;
  try {
    for (const d of await h.getDevices()) if (isDualSense(d)) await watch(d);
  } catch {
    // Not allowed here (a permissions policy, say).
  }
}

/** Ask the browser for the DualSense (call from a click). True when one is being read. */
export async function connectWebHidTouchpad(): Promise<boolean> {
  const h = hid();
  if (!h) return false;
  try {
    const picked = await h.requestDevice({
      filters: DUALSENSE_PIDS.map((productId) => ({ vendorId: SONY_VID, productId })),
    });
    let ok = false;
    for (const d of picked) if (isDualSense(d) && (await watch(d))) ok = true;
    return ok;
  } catch {
    return false;
  }
}

/** A controller's touchpad is being read in this browser. */
export function webHidTouchpadConnected(): boolean {
  return hidDevices > 0;
}

// ── On and off ─────────────────────────────────────────────────────────────────────────────

/** Read the touchpad while a stream plays (on) or stop (off); `onChange` is called on every
 *  change while on. Repeats are dropped. */
let browserOn = false;

export function setPadTouchpad(on: boolean, onChange?: () => void): void {
  if (onChange) notify = onChange;
  if (isTauriEnv()) {
    if (!isMobile()) setNative(on);
    return;
  }
  if (on === browserOn) return;
  browserOn = on;
  if (on && !hidDevices) void reopenGranted();
}

/** Test hook. */
export function resetPadTouchpadForTest(): void {
  state = IDLE;
  notify = null;
  nativeOn = null;
  nativeUnlisten = null;
  motionUnlisten = null;
  motion = null;
  browserOn = false;
  hidDevices = 0;
  chain = Promise.resolve();
}

/** Test hook: what an event from the app would do. */
export function applyPadTouchForTest(raw: { touch: [number, number] | null; click: boolean }): void {
  update({ touch: touchFromRaw(raw.touch), click: raw.click });
}
