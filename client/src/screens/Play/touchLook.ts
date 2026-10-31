import type { RpPrefs } from "../../state/remotePlay";

/** The on-screen controller's look: how big and how solid it is drawn. Pure, so it is tested
 *  without a page. */

/** Size setting → scale of the drawing, on top of the fit to the screen. */
export const SIZE_SCALE: Record<RpPrefs["touchSize"], number> = { s: 0.85, m: 1, l: 1.15 };

/** The faintest the controller may be set (a fainter one is hard to find on a bright game). */
export const MIN_TOUCH_OPACITY = 0.25;

/** A saved opacity as drawn: 0.25 to 1; anything odd (an older or hand-edited setting) is 1. */
export function touchOpacity(v: unknown): number {
  if (typeof v !== "number" || !Number.isFinite(v)) return 1;
  return Math.min(1, Math.max(MIN_TOUCH_OPACITY, v));
}

/**
 * The space each layout is drawn for at scale 1, CSS px: a phone held sideways for the
 * controller over the picture, and the black under the picture of a phone held upright (deck).
 */
export const DESIGN_SIZE = {
  overlay: { width: 844, height: 390 },
  deck: { width: 390, height: 480 },
} as const;

/** Never drawn smaller or bigger than this for the screen (before the size setting). */
const FIT_MIN = 0.7;
const FIT_MAX = 1.4;

/**
 * How much bigger than its design the controller is drawn in a `width` × `height` space: fitted
 * to the space (a tablet gets bigger keys, a small phone smaller ones), times the size setting.
 * An unknown space (not measured yet) is the size setting alone.
 */
export function controllerScale(
  size: RpPrefs["touchSize"],
  width: number,
  height: number,
  deck: boolean,
): number {
  const s = SIZE_SCALE[size] ?? 1;
  if (!(width > 0) || !(height > 0)) return s;
  const d = deck ? DESIGN_SIZE.deck : DESIGN_SIZE.overlay;
  const fit = Math.min(FIT_MAX, Math.max(FIT_MIN, Math.min(width / d.width, height / d.height)));
  return Math.round(fit * s * 1000) / 1000;
}

/**
 * Where a stick's thumb disc is drawn, px from the ring's centre, for an axis pair in
 * -32767..32767: inside a circle of `travel` (the axes are clamped as a square, the disc never
 * leaves the ring).
 */
export function thumbOffset(x: number, y: number, travel: number): { dx: number; dy: number } {
  const nx = x / 32767;
  const ny = y / 32767;
  const len = Math.hypot(nx, ny);
  const k = len > 1 ? 1 / len : 1;
  return { dx: nx * k * travel, dy: ny * k * travel };
}
