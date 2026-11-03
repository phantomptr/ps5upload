/**
 * Android's picture in picture for Remote Play (#637): leaving the Android app (home button or
 * gesture) while a stream plays keeps the picture floating over other apps.
 *
 * The Android app's MainActivity (scripts/release/android-postinit-patch.sh, step 12) exposes
 * `window.PS5UploadPip`. This page tells it whether a stream is playing and the picture's shape;
 * the activity goes into picture in picture on leaving only then, and dispatches a
 * `ps5upload-pip` window event (`detail.inPip`) on the way in and out. While in it, the page
 * shows only the picture (layout/RemotePlayMini AndroidPipPicture).
 *
 * Everywhere else (desktop, iOS, a browser, an older Android app) there is no bridge and all of
 * this does nothing.
 */
import { create } from "zustand";

import { useRemotePlayStore, type RpPrefs } from "../state/remotePlay";

/** The activity's JavaScript interface (MainActivity, step 12 of android-postinit-patch.sh). */
export interface AndroidPipBridge {
  setActive(active: boolean, aspectW: number, aspectH: number): void;
  /** Whether the activity is in picture in picture now. */
  inPip?(): boolean;
  /** Whether this device has picture in picture at all (Android 8+, and not every device). */
  supported?(): boolean;
}

/** The window event the activity dispatches on entering and leaving picture in picture. */
export const PIP_EVENT = "ps5upload-pip";

/** Android allows shapes between 1:2.39 and 2.39:1. */
const MAX_SHAPE = 2.39;

export function androidPipBridge(): AndroidPipBridge | null {
  if (typeof window === "undefined") return null;
  const b = (window as unknown as { PS5UploadPip?: AndroidPipBridge }).PS5UploadPip;
  return b && typeof b.setActive === "function" ? b : null;
}

/** Whether this is the Android app with picture in picture (for the setting). */
export function androidPipAvailable(): boolean {
  const b = androidPipBridge();
  if (!b) return false;
  try {
    return b.supported ? !!b.supported() : true;
  } catch {
    return false;
  }
}

/** The page is in Android's picture in picture: only the picture shows. */
export const useAndroidPipStore = create<{ inPip: boolean }>(() => ({ inPip: false }));

/** The picture's shape as whole numbers for Android (width, height), within what it allows;
 *  16:9 when the shape is unknown. */
export function aspectOf(shape: number): [number, number] {
  if (!Number.isFinite(shape) || shape <= 0) return [16, 9];
  const s = Math.min(MAX_SHAPE, Math.max(1 / MAX_SHAPE, shape));
  if (Math.abs(s - 16 / 9) < 0.001) return [16, 9];
  return [Math.round(s * 1000), 1000];
}

/** The shape the stream's canvas is drawn at (lib/remotePlayStream sets --rp-shape). */
function canvasShape(canvas: HTMLCanvasElement | null | undefined): number {
  const v = canvas?.style?.getPropertyValue("--rp-shape");
  return v ? Number.parseFloat(v) : NaN;
}

/** What the activity is told: picture in picture on leaving only while a stream plays and the
 *  setting is on. */
export function pipRequest(s: {
  stream: { canvas: HTMLCanvasElement } | null;
  phase: string | null;
  prefs: Pick<RpPrefs, "systemPip">;
}): { active: boolean; w: number; h: number } {
  const active = !!s.stream && s.phase === "playing" && s.prefs.systemPip !== false;
  const [w, h] = aspectOf(canvasShape(s.stream?.canvas));
  return { active, w, h };
}

let installed: (() => void) | null = null;

/** Keep the activity told about the stream, and the page told about picture in picture. Once
 *  per page; returns the undo (tests). A no-op without the bridge. */
export function installAndroidPip(): () => void {
  if (installed) return installed;
  const bridge = androidPipBridge();
  if (!bridge) return () => {};
  let last = "";
  const push = () => {
    const r = pipRequest(useRemotePlayStore.getState());
    const key = `${r.active}:${r.w}:${r.h}`;
    if (key === last) return;
    last = key;
    try {
      bridge.setActive(r.active, r.w, r.h);
    } catch {
      // An activity going away: the next change tries again.
      last = "";
    }
  };
  push();
  const unsub = useRemotePlayStore.subscribe(push);
  const onPip = (e: Event) => {
    const inPip = !!(e as CustomEvent<{ inPip?: boolean }>).detail?.inPip;
    useAndroidPipStore.setState({ inPip });
  };
  window.addEventListener(PIP_EVENT, onPip);
  try {
    if (bridge.inPip?.()) useAndroidPipStore.setState({ inPip: true });
  } catch {
    // Not answered: the event says it when it changes.
  }
  installed = () => {
    installed = null;
    unsub();
    window.removeEventListener(PIP_EVENT, onPip);
    try {
      bridge.setActive(false, 16, 9);
    } catch {
      // Gone already.
    }
    useAndroidPipStore.setState({ inPip: false });
  };
  return installed;
}
