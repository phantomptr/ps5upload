// Controller button remap: which PS5 button each of a controller's buttons sends, the way the
// keyboard remap (remotePlayStream.ts PAD_ACTIONS) sets what each key does. It works on the pad a
// controller already amounts to (its profile in padProfiles.ts reads it in the standard layout),
// so it is the same for every controller: swap ✕ and ○, put Create on a back button's L3...
// Sticks are left as they are. Saved with the other Remote Play settings (state/remotePlay.ts).

import { PadButton, type PadState } from "./remotePlayStream";

/** A controller button, by the PS5 button it is in the standard layout (L2/R2 are analog). */
export type PadControl =
  | "cross"
  | "circle"
  | "square"
  | "triangle"
  | "up"
  | "down"
  | "left"
  | "right"
  | "l1"
  | "r1"
  | "l2"
  | "r2"
  | "l3"
  | "r3"
  | "options"
  | "create"
  | "ps"
  | "touchpad";

/** What each controller button sends instead of itself; "none" turns it off. A button not in the
 *  map sends itself, so the empty map is the default. */
export type ButtonMap = Partial<Record<PadControl, PadControl | "none">>;

export const DEFAULT_BUTTON_MAP: ButtonMap = {};

/** The controls in the order the settings list them, with their button bit (null: a trigger). */
export const PAD_CONTROLS: ReadonlyArray<{ id: PadControl; label: string; bit: number | null }> = [
  { id: "cross", label: "✕", bit: PadButton.CROSS },
  { id: "circle", label: "○", bit: PadButton.CIRCLE },
  { id: "square", label: "□", bit: PadButton.SQUARE },
  { id: "triangle", label: "△", bit: PadButton.TRIANGLE },
  { id: "up", label: "D-pad up", bit: PadButton.UP },
  { id: "down", label: "D-pad down", bit: PadButton.DOWN },
  { id: "left", label: "D-pad left", bit: PadButton.LEFT },
  { id: "right", label: "D-pad right", bit: PadButton.RIGHT },
  { id: "l1", label: "L1", bit: PadButton.L1 },
  { id: "r1", label: "R1", bit: PadButton.R1 },
  { id: "l2", label: "L2", bit: null },
  { id: "r2", label: "R2", bit: null },
  { id: "l3", label: "L3", bit: PadButton.L3 },
  { id: "r3", label: "R3", bit: PadButton.R3 },
  { id: "options", label: "Options", bit: PadButton.OPTIONS },
  { id: "create", label: "Create", bit: PadButton.SHARE },
  { id: "ps", label: "PS button", bit: PadButton.PS },
  { id: "touchpad", label: "Touchpad press", bit: PadButton.TOUCHPAD },
];

const BY_ID = new Map(PAD_CONTROLS.map((c) => [c.id, c]));
/** Every button bit a control can carry (the ones the remap rewrites). */
const MAPPED_BITS = PAD_CONTROLS.reduce((m, c) => m | (c.bit ?? 0), 0);

/** True when the map sends every button as itself. */
export function isDefaultMap(map: ButtonMap): boolean {
  return Object.entries(map).every(([from, to]) => from === to);
}

/** How far (0-1) `c` is pressed in `p`. */
function level(p: PadState, c: PadControl): number {
  const ctl = BY_ID.get(c);
  if (!ctl) return 0;
  if (ctl.bit === null) return (c === "l2" ? p.l2 : p.r2) / 255;
  return p.buttons & ctl.bit ? 1 : 0;
}

/** `p` with its buttons and triggers sent as `map` says. A trigger sent as a button presses it
 *  past half way; a button sent as a trigger pulls it all the way. Several buttons may send one
 *  (either presses it). Sticks, touch and motion pass through. */
export function remapPad(p: PadState, map: ButtonMap): PadState {
  if (isDefaultMap(map)) return p;
  const out: PadState = { ...p, buttons: p.buttons & ~MAPPED_BITS, l2: 0, r2: 0 };
  for (const ctl of PAD_CONTROLS) {
    const v = level(p, ctl.id);
    if (v <= 0) continue;
    const to = map[ctl.id] ?? ctl.id;
    if (to === "none") continue;
    const target = BY_ID.get(to);
    if (!target) continue;
    if (target.bit === null) {
      const k = to === "l2" ? "l2" : "r2";
      out[k] = Math.max(out[k], Math.round(v * 255));
    } else if (v > 0.5) {
      out.buttons |= target.bit;
    }
  }
  return out;
}

/** `map` with `from` sending `to` (sending itself is dropped from the map). */
export function setMapping(map: ButtonMap, from: PadControl, to: PadControl | "none"): ButtonMap {
  const next = { ...map };
  if (to === from) delete next[from];
  else next[from] = to;
  return next;
}

/** A stored map, checked: unknown buttons and targets are dropped. */
export function readButtonMap(v: unknown): ButtonMap {
  if (!v || typeof v !== "object" || Array.isArray(v)) return DEFAULT_BUTTON_MAP;
  const out: ButtonMap = {};
  for (const [from, to] of Object.entries(v as Record<string, unknown>)) {
    if (!BY_ID.has(from as PadControl) || typeof to !== "string") continue;
    if (to !== "none" && !BY_ID.has(to as PadControl)) continue;
    if (to !== from) out[from as PadControl] = to as PadControl | "none";
  }
  return out;
}
