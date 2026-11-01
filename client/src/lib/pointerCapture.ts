// The mouse captured by the stream's picture: one answer for "is it captured?" for native keyboard
// & mouse (useNativeKbm.ts) and mouse aim (StreamPanel.tsx).
//
// Two ways to capture:
//  - "lock": the browser's Pointer Lock on the picture (Chrome, Edge, Firefox, WebView2 on Windows).
//  - "grab": the desktop app's own grab, for web views that never grant a lock (WKWebView in the
//    macOS app: requestPointerLock is accepted and nothing happens). The window's cursor is grabbed
//    and hidden (Tauri's setCursorGrab / setCursorVisible). On macOS the grab detaches the cursor
//    from the mouse (CGAssociateMouseAndMouseCursorPosition(false), the same call WebKit's own
//    pointer lock makes), so the cursor stays put and mousemove carries the mouse's deltas in
//    movementX/movementY. Where the grab does nothing (Linux: tao has no grab) the cursor moves,
//    and its screen position gives the deltas instead (captureDelta).
//
// The lock is asked for first; in the desktop app, with no lock LOCK_WAIT_MS later, the grab is
// used. Every release puts the cursor back (grab off, visible) whatever state it was in, and never
// throws: a cursor left grabbed or hidden would take the user's whole Mac.

import { isMacOrLinuxDesktop } from "./platform";
import { isTauriEnv } from "./tauriEnv";

export type CaptureKind = "none" | "lock" | "grab";

export interface CaptureState {
  kind: CaptureKind;
  /** The element the mouse is captured by (the stage). */
  el: Element | null;
  /** The last ask got neither a lock nor a grab (a browser that refuses the lock). */
  failed: boolean;
}

/** How long a lock may take to arrive before the desktop app grabs instead. */
export const LOCK_WAIT_MS = 150;

/** The desktop app's grab: on (grab + hide) or off (release + show). */
export type NativeGrab = (on: boolean) => Promise<void>;

const tauriGrab: NativeGrab = async (on) => {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const w = getCurrentWindow();
  if (on) {
    await w.setCursorGrab(true);
    await w.setCursorVisible(false);
    return;
  }
  // Both, even when one fails: never leave the cursor grabbed or hidden.
  const r = await Promise.allSettled([w.setCursorGrab(false), w.setCursorVisible(true)]);
  const bad = r.find((x) => x.status === "rejected");
  if (bad) throw (bad as PromiseRejectedResult).reason;
};

let grabImpl: NativeGrab = tauriGrab;
/** The app where the grab stands in for the lock: macOS (WKWebView never locks) and Linux
 *  (WebKitGTK may not). Windows' WebView2 locks; a lock it refuses (asked again right after Esc)
 *  stays refused there, as in Chrome. */
const grabHere = () => isTauriEnv() && isMacOrLinuxDesktop();
let desktopFn: () => boolean = grabHere;
let state: CaptureState = { kind: "none", el: null, failed: false };
const subs = new Set<() => void>();
let timer: ReturnType<typeof setTimeout> | null = null;
/** The element a capture was asked for and not yet given. */
let pending: HTMLElement | null = null;
/** Grab calls run in order: an "off" never overtakes the "on" before it. */
let grabChain: Promise<void> = Promise.resolve();
/** The grab is (or is being turned) on in the window. */
let grabOn = false;
let lastScreen: { x: number; y: number } | null = null;
let listening = false;

function set(next: Partial<CaptureState>): void {
  const s = { ...state, ...next };
  if (s.kind === state.kind && s.el === state.el && s.failed === state.failed) return;
  state = s;
  for (const fn of [...subs]) fn();
}

function runGrab(on: boolean): void {
  grabOn = on;
  grabChain = grabChain
    .then(() => grabImpl(on))
    .catch((e) => {
      console.warn(`[pointerCapture] native grab ${on ? "on" : "off"} failed`, e);
      // A grab that did not take: nothing is captured (and the cursor is put back).
      if (on && state.kind === "grab") {
        grabOn = false;
        hideCursor(null);
        set({ kind: "none", el: null, failed: true });
        return grabImpl(false).catch(() => {});
      }
    });
}

/** The cursor hidden over the captured element (WebKit sets its own cursor on every move, so the
 *  window's hidden cursor alone would not stay hidden over the picture). */
let styled: { el: HTMLElement; cursor: string } | null = null;
function hideCursor(el: HTMLElement | null): void {
  if (styled) {
    styled.el.style.cursor = styled.cursor;
    styled.el.removeAttribute("data-mouse-grab");
    styled = null;
  }
  if (el) {
    styled = { el, cursor: el.style.cursor };
    el.style.cursor = "none";
    el.setAttribute("data-mouse-grab", "");
  }
}

function clearTimer(): void {
  if (timer !== null) clearTimeout(timer);
  timer = null;
}

function onLockChange(): void {
  const locked = typeof document !== "undefined" ? document.pointerLockElement : null;
  if (locked && (locked === pending || locked === state.el)) {
    clearTimer();
    pending = null;
    // A lock that came late wins over the grab.
    if (grabOn) {
      hideCursor(null);
      runGrab(false);
    }
    set({ kind: "lock", el: locked, failed: false });
    return;
  }
  if (!locked && state.kind === "lock") {
    lastScreen = null;
    set({ kind: "none", el: null });
  }
}

function listen(): void {
  if (listening || typeof document === "undefined") return;
  listening = true;
  document.addEventListener("pointerlockchange", onLockChange);
  // A reload or a closed window gives the cursor back too.
  window.addEventListener("pagehide", () => releaseCapture());
}

export function captureState(): CaptureState {
  return state;
}

export function subscribeCapture(fn: () => void): () => void {
  subs.add(fn);
  return () => {
    subs.delete(fn);
  };
}

/** The mouse is captured (by `el`, when given). */
export function isCaptured(el?: Element | null): boolean {
  if (state.kind === "none") return false;
  return el === undefined || (el !== null && state.el === el);
}

/** Asks for the mouse on `el`: the browser's lock, else (in the desktop app) the window's grab. */
export function requestCapture(el: HTMLElement): void {
  listen();
  if (isCaptured(el) || pending === el) return;
  if (state.kind !== "none") releaseCapture();
  pending = el;
  lastScreen = null;
  let refused = false;
  try {
    // A promise in current browsers (refused: not focused, too soon after Esc); older ones throw.
    const r = el.requestPointerLock?.() as unknown as Promise<void> | undefined;
    r?.catch?.(() => {});
  } catch {
    refused = true;
  }
  clearTimer();
  const fallBack = () => {
    timer = null;
    if (pending !== el) return;
    if (typeof document !== "undefined" && document.pointerLockElement === el) return onLockChange();
    pending = null;
    if (!desktopFn()) {
      set({ failed: true });
      return;
    }
    hideCursor(el);
    set({ kind: "grab", el, failed: false });
    runGrab(true);
  };
  if (refused && desktopFn()) fallBack();
  else timer = setTimeout(fallBack, LOCK_WAIT_MS);
}

/** Gives the mouse back: the lock left, the grab off and the cursor shown. Safe to call any time. */
export function releaseCapture(): void {
  clearTimer();
  pending = null;
  lastScreen = null;
  try {
    if (typeof document !== "undefined" && document.pointerLockElement) document.exitPointerLock?.();
  } catch {
    // Nothing to give back.
  }
  hideCursor(null);
  // Off whenever a grab may be on: also after a failed or half-done one.
  if (grabOn || state.kind === "grab") runGrab(false);
  set({ kind: "none", el: null });
}

/** The mouse's movement in a mousemove: movementX/Y, or (where those are 0 and the cursor moves,
 *  as under a grab that does not hold the cursor) the change of its screen position. */
export function captureDelta(e: Pick<MouseEvent, "movementX" | "movementY" | "screenX" | "screenY">): [number, number] {
  const prev = lastScreen;
  lastScreen = { x: e.screenX, y: e.screenY };
  const mx = e.movementX || 0;
  const my = e.movementY || 0;
  if (mx || my || !prev) return [mx, my];
  return [e.screenX - prev.x, e.screenY - prev.y];
}

/** Tests: the grab and "is this the desktop app" without Tauri, and a clean slate. */
export function _resetCaptureForTest(opts: { grab?: NativeGrab; desktop?: boolean } = {}): void {
  clearTimer();
  hideCursor(null);
  grabImpl = opts.grab ?? tauriGrab;
  desktopFn = opts.desktop === undefined ? grabHere : () => !!opts.desktop;
  state = { kind: "none", el: null, failed: false };
  pending = null;
  grabOn = false;
  grabChain = Promise.resolve();
  lastScreen = null;
  subs.clear();
  listening = false;
}

/** Tests: waits for queued grab calls. */
export function _grabSettled(): Promise<void> {
  return grabChain;
}
