// Controllers a browser does not lay out the standard way (Gamepad.mapping === ""): a desktop
// Chrome, Edge, Firefox or Safari reading a pad it has no mapping of its own for (an 8BitDo in
// D-input mode, a Switch Pro in Firefox on some systems, a generic "USB Gamepad"...). The raw
// buttons and axes then come in the controller's own order. padMappingTable.ts (from SDL's
// GameControllerDB) says, per controller, where each standard button and stick is; this turns
// that into the PadProfile the stream reads. A pad in neither list plays in the standard order as
// a guess (guessProfile) until it is checked by hand (join/PadCheck.tsx, Play's ControllerSetup).
//
// Position over labels: the DB maps by position, so a Nintendo-layout pad's bottom button (B on
// the pad) is ✕, as the PS5 expects, whatever is printed on it.

import { PAD_MAPS, PAD_TABLE } from "./padMappingTable";
import {
  HAT_DOWN,
  HAT_LEFT,
  HAT_RIGHT,
  HAT_UP,
  STANDARD_BUTTON_BITS,
  type PadAxis,
  type PadProfile,
  type PadSource,
} from "./remotePlayStream";

export interface PadIdentity {
  /** USB vendor and product id, 4 lowercase hex digits each. */
  vendor: string;
  product: string;
  /** The name the browser gave, without the numbers. */
  name: string;
}

const hex4 = (s: string) => s.toLowerCase().padStart(4, "0");

/** Vendor, product and name from a Gamepad.id, in each browser's shape:
 *  - Chrome, Edge (and Chrome on Android): "DualSense Wireless Controller (STANDARD GAMEPAD Vendor: 054c Product: 0ce6)"
 *    or "8BitDo Pro 2 (Vendor: 2dc8 Product: 6006)"
 *  - Firefox: "054c-0ce6-DualSense Wireless Controller" (older Firefox on Windows: unpadded "54c-ce6-…")
 *  - Safari on macOS for a pad the GameController framework does not know: "2dc8-6006-8BitDo Pro 2"
 *    (the same as Firefox); for one it knows, only a name, and the standard layout.
 *  Null when the id carries no numbers. */
export function parseGamepadId(id: string): PadIdentity | null {
  const chrome = /^(.*?)\s*\((?:[^()]*?\s)?Vendor:\s*([0-9a-f]{1,4})\s+Product:\s*([0-9a-f]{1,4})\s*\)\s*$/i.exec(id);
  if (chrome) return { vendor: hex4(chrome[2]), product: hex4(chrome[3]), name: chrome[1].trim() };
  const dashed = /^([0-9a-f]{1,4})-([0-9a-f]{1,4})-(.*)$/i.exec(id.trim());
  if (dashed) return { vendor: hex4(dashed[1]), product: hex4(dashed[2]), name: dashed[3].trim() };
  return null;
}

/** A controller's name without the browser's vendor/product numbers. */
export function padName(id: string): string {
  const n = parseGamepadId(id)?.name ?? id.replace(/\s*\((?:XInput\s*)?STANDARD GAMEPAD\)\s*$/i, "");
  return n.trim() || id;
}

export type PadPlatform = "w" | "m" | "l";

/** The desktop system this browser runs on, as the table names it; null on a phone or tablet
 *  (Android and iOS lay out every controller they support the standard way). */
export function padPlatform(nav: { userAgent?: string; maxTouchPoints?: number } | undefined = globalThis.navigator): PadPlatform | null {
  const ua = nav?.userAgent ?? "";
  if (/Android|iPhone|iPad|iPod/i.test(ua)) return null;
  // iPadOS asks for the desktop site as a Mac: one with a touch screen is an iPad.
  if (/Macintosh|Mac OS X/i.test(ua)) return (nav?.maxTouchPoints ?? 0) > 1 ? null : "m";
  if (/Windows/i.test(ua)) return "w";
  if (/Linux|CrOS|X11/i.test(ua)) return "l";
  return null;
}

/** The table's mapping for a pad on `platform`, or null. Browsers read a HID pad the same way on
 *  Windows and macOS, as SDL does on macOS (its Windows lines are DirectInput's order), so the
 *  macOS line comes first for both. */
export function tableMapping(ident: PadIdentity, platform: PadPlatform | null): string | null {
  if (!platform) return null;
  const order: PadPlatform[] = platform === "l" ? ["l"] : ["m", "w"];
  for (const p of order) {
    const at = PAD_TABLE[`${p}${ident.vendor}${ident.product}`];
    if (at !== undefined) return PAD_MAPS[at] ?? null;
  }
  return null;
}

/** 8BitDo's USB vendor id: its pads in D-input (and some in macOS) mode come through as these. */
export const VENDOR_8BITDO = "2dc8";

/** An 8BitDo pad in a mode the browser does not lay out the standard way. */
export function isWeak8BitDo(g: Pick<Gamepad, "id" | "mapping">): boolean {
  return g.mapping !== "standard" && parseGamepadId(g.id)?.vendor === VENDOR_8BITDO;
}

/** How an 8BitDo model gets to X-input mode, from 8BitDo's manuals (support.8bitdo.com):
 *  - "switch": the mode switch on the back to X, then Start (Pro 2)
 *  - "startx": turned on with Start + X (SN30 Pro, SN30 Pro+)
 *  - "receiver": X-input is the 2.4G receiver or a USB cable (Ultimate, Ultimate C, 2C, 2)
 *  - "none": no X-input mode (Lite 2, Micro)
 *  - "manual": a model not listed here. */
export type EightBitDoHow = "switch" | "startx" | "receiver" | "none" | "manual";

const EIGHTBITDO_HOW: Record<string, EightBitDoHow> = {
  "6003": "switch", "6006": "switch", "6103": "switch", "3010": "switch",
  "6001": "startx", "6002": "startx", "6101": "startx", "6102": "startx",
  "3011": "receiver", "3012": "receiver", "3013": "receiver", "3015": "receiver", "3016": "receiver",
  "3017": "receiver", "301b": "receiver", "301c": "receiver", "301d": "receiver", "6012": "receiver",
  "5112": "none", "9020": "none",
};

export function eightBitDoHow(id: string): EightBitDoHow {
  const ident = parseGamepadId(id);
  return (ident && EIGHTBITDO_HOW[ident.product]) || "manual";
}

const HAT_AT: Record<string, number> = { "1": HAT_UP, "2": HAT_RIGHT, "4": HAT_DOWN, "8": HAT_LEFT };

/** Where the browser put the controller's axes, worked out from one reading. */
interface Layout {
  /** The hat's axis (one axis, 8 positions, resting outside -1..1), or -1. */
  hat: number;
  /** The hat as two axes after the others (Linux), x then y, or -1. */
  hatXY: number;
  /** DB axis index → the browser's. */
  axis: (k: number) => number;
}

/** How many axes a mapping names (the highest index + 1), and whether it uses the hat. */
function shape(tokens: string[]): { n: number; hat: boolean } {
  let n = 0;
  let hat = false;
  for (const t of tokens) {
    const m = /a(\d+)/.exec(t);
    if (m) n = Math.max(n, Number(m[1]) + 1);
    if (t.startsWith("h")) hat = true;
  }
  return { n, hat };
}

/**
 * The DB counts a controller's axes one after another. Chrome and Edge on Windows and macOS put a
 * HID axis at its usage number instead (X 0, Y 1, Z 2, Rx 3, Ry 4, Rz 5, the hat 9), leaving
 * gaps, and fill the gaps with axes from other pages (a trigger's Accelerator and Brake). Firefox,
 * Safari and Linux count them in order, as the DB does (Linux adds the hat as two axes at the end).
 * Which one this is shows in where the hat rests, or in how many axes there are.
 */
function layoutOf(g: Gamepad, tokens: string[], platform: PadPlatform): Layout | null {
  const { n, hat: usesHat } = shape(tokens);
  const at = STANDARD_BUTTON_BITS.length;
  const hat = g.axes.findIndex((v) => Math.abs(v) > 1.05);
  const identity = (k: number) => k;
  if (usesHat && hat < 0) {
    if (platform === "l" && g.axes.length >= n + 2) return { hat: -1, hatXY: n, axis: identity };
    // The hat is somewhere but not at rest (a d-pad held as it connected): ask again next time.
    return null;
  }
  const sparse = usesHat ? hat > n : platform !== "l" && g.axes.length >= n + 2;
  if (!sparse) return { hat, hatXY: -1, axis: identity };
  if (n <= 4) return { hat, hatXY: -1, axis: (k) => [0, 1, 2, 5][k] ?? k };
  // Six, the right stick on the DB's a2/a3 and the triggers after it on a4/a5: sticks on X Y Z Rz
  // and the triggers on Accelerator and Brake (most HID pads, 8BitDo's D-input among them), which
  // land as X Y Z Accel Brake Rz. Otherwise all six are X Y Z Rx Ry Rz (a PlayStation pad: right
  // stick a2/a5, triggers a3/a4), in the same places either way.
  const rightOn23 = tokens[at + 2]?.replace("~", "") === "a2" && tokens[at + 3]?.replace("~", "") === "a3";
  const triggersOn45 = [tokens[6], tokens[7]].every((t) => /^a[45]~?$/.test(t ?? ""));
  if (n === 6 && rightOn23 && triggersOn45) return { hat, hatXY: -1, axis: (k) => [0, 1, 2, 5, 3, 4][k] ?? k };
  return { hat, hatXY: -1, axis: identity };
}

/** One DB source as a source of a button (or a trigger, `full` reading a whole axis as 0-1). */
function sourceOf(t: string, l: Layout): PadSource | null {
  let m = /^b(\d+)$/.exec(t);
  if (m) return { b: Number(m[1]) };
  m = /^([+-]?)a(\d+)(~?)$/.exec(t);
  if (m) {
    const a = l.axis(Number(m[2]));
    if (m[1] === "+") return { a, d: 1 };
    if (m[1] === "-") return { a, d: -1 };
    // A whole axis as a trigger: it rests at one end.
    return m[3] ? { a, d: -1, rest: 1 } : { a, d: 1, rest: -1 };
  }
  m = /^h0\.(\d)$/.exec(t);
  if (m) {
    const at = HAT_AT[m[1]];
    if (at === undefined) return null;
    if (l.hat >= 0) return { h: l.hat, at };
    if (l.hatXY >= 0) {
      if (at === HAT_UP) return { a: l.hatXY + 1, d: -1 };
      if (at === HAT_DOWN) return { a: l.hatXY + 1, d: 1 };
      if (at === HAT_LEFT) return { a: l.hatXY, d: -1 };
      return { a: l.hatXY, d: 1 };
    }
  }
  return null;
}

function axisOf(t: string, l: Layout): PadAxis | null {
  const m = /^a(\d+)(~?)$/.exec(t);
  if (m) return m[2] ? { a: l.axis(Number(m[1])), inv: true } : { a: l.axis(Number(m[1])) };
  // A Joy-Con held sideways: its hat is the stick.
  if (t === "hx" || t === "hy") {
    if (l.hat >= 0) return { a: l.hat, hat: t === "hx" ? "x" : "y" };
    if (l.hatXY >= 0) return { a: l.hatXY + (t === "hy" ? 1 : 0) };
  }
  return null;
}

/** A table mapping read for one controller as the browser shows it now; null while that cannot
 *  be told yet (its hat not at rest). */
export function profileFromMapping(map: string, g: Gamepad, platform: PadPlatform): PadProfile | null {
  const tokens = map.split(",");
  const l = layoutOf(g, tokens, platform);
  if (!l) return null;
  const prof: PadProfile = { buttons: [], l2: null, r2: null, lx: null, ly: null, rx: null, ry: null };
  for (let i = 0; i < STANDARD_BUTTON_BITS.length; i++) {
    const t = tokens[i];
    if (!t) continue;
    const src = sourceOf(t, l);
    if (!src) continue;
    if (i === 6) prof.l2 = src;
    else if (i === 7) prof.r2 = src;
    else if (STANDARD_BUTTON_BITS[i]) prof.buttons.push([STANDARD_BUTTON_BITS[i], src]);
  }
  const at = STANDARD_BUTTON_BITS.length;
  prof.lx = axisOf(tokens[at] ?? "", l);
  prof.ly = axisOf(tokens[at + 1] ?? "", l);
  prof.rx = axisOf(tokens[at + 2] ?? "", l);
  prof.ry = axisOf(tokens[at + 3] ?? "", l);
  return prof;
}

/** Worked out once per controller (its layout does not change while connected). */
const known = new Map<string, PadProfile | null>();

/** The table's profile for a controller the browser does not lay out the standard way, or null
 *  (standard already, not in the table, or on a phone). */
export function knownProfile(g: Gamepad, platform: PadPlatform | null = padPlatform()): PadProfile | null {
  if (g.mapping === "standard" || !platform) return null;
  const key = `${platform}|${g.id}|${g.axes.length}`;
  if (known.has(key)) return known.get(key) ?? null;
  const ident = parseGamepadId(g.id);
  const map = ident ? tableMapping(ident, platform) : null;
  if (!map) {
    known.set(key, null);
    return null;
  }
  const prof = profileFromMapping(map, g, platform);
  if (prof) known.set(key, prof);
  return prof;
}

/** For tests: forget what was worked out. */
export function resetKnownProfiles(): void {
  known.clear();
}

/** How a controller plays: the browser's standard layout, a mapping saved on this device, the
 *  table's, or a guess (not recognized: worth checking). */
export type PadKind = "standard" | "saved" | "known" | "guess";
