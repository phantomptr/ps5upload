/**
 * The Remote Play mini player (#637): while a stream runs and another screen of the app is on
 * show, the picture stays in a small floating window over it (layout/RemotePlayMini.tsx). The
 * decisions are here, pure, for the tests.
 */
import { safeGetItem, safeSetItem } from "./safeStorage";

/** Which corner of the window it sits in: top/bottom, left/right. */
export type MiniCorner = "tl" | "tr" | "bl" | "br";

const CORNER_KEY = "ps5upload.rp_mini_corner.v1";
const CORNERS: readonly MiniCorner[] = ["tl", "tr", "bl", "br"];

export function loadCorner(): MiniCorner {
  const v = safeGetItem(CORNER_KEY);
  return CORNERS.includes(v as MiniCorner) ? (v as MiniCorner) : "br";
}

export function saveCorner(c: MiniCorner): void {
  safeSetItem(CORNER_KEY, c);
}

/** The corner nearest to where it was dropped (its middle, in a window of that size). */
export function nearestCorner(x: number, y: number, width: number, height: number): MiniCorner {
  const top = y < height / 2;
  const left = x < width / 2;
  return `${top ? "t" : "b"}${left ? "l" : "r"}` as MiniCorner;
}

/** A press that moved less than this is a tap (back to Play), not a drag. */
export const DRAG_SLOP_PX = 6;

/**
 * Whether the mini player shows. Only one place shows the picture at a time: the Play screen
 * while it is on show, else a picture-in-picture window (desktop popup, the browser's own, a
 * video), else Android's picture in picture, else here.
 */
export function miniPlayerVisible(s: {
  /** A stream exists and has not ended. */
  streaming: boolean;
  phase: string | null;
  /** The route on show. */
  pathname: string;
  /** The Play screen shows the stream (state/remotePlay onScreen). */
  onScreen: boolean;
  /** A picture-in-picture window has the picture (state/remotePlay pip). */
  pip: string | null;
  /** The Android app is in picture in picture (lib/androidPip). */
  androidPip: boolean;
  /** Settings › Session › Mini player. */
  enabled: boolean;
  /** Closed with its × until Play is opened again. */
  dismissed: boolean;
}): boolean {
  if (!s.streaming || !s.phase || s.phase === "ended") return false;
  // Another window of this app plays it: nothing is drawn here.
  if (s.phase === "moved") return false;
  if (s.pathname === "/play" || s.onScreen) return false;
  if (s.pip || s.androidPip) return false;
  return s.enabled && !s.dismissed;
}
