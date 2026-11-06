// Remote Play in the app: one stream from the engine's /api/ps5/rp/ws, decoded with WebCodecs.
//
// The engine runs the session with the console (ps5upload_core::rp_stream, a port of chiaki-ng)
// and relays it over a WebSocket: binary records out ([kind][payload]: 0 video header, 1 video
// access unit, 3 audio header, 4 audio frame, 6 haptics, 7 video access unit after the engine's
// u64 BE µs time it had it whole — sent only because this page asks with "timing": an older
// engine sends 1), JSON status lines out, pad state in. This file
// turns that into pixels on a canvas, sound, and input; the screen only drives it.

import { isPasscode } from "./passcode";
import { DEFAULT_PICTURE, PicturePass, PlaneReader, chromaBlank, cropRect, isPlaneFormat, needsPass, sharpens, upscaleSize, upscales, type PictureTools } from "./rpPicture";
import { isLinuxDesktop } from "./platform";
import { linuxCompositingOff } from "./linuxGraphics";
import { decoderStarting, linuxSoftwareVideo, withDecoderStart, type DecoderStart } from "./videoDecoderGuard";
import { isTauriEnv } from "./tauriEnv";
import { HdrPresenter, hdrSupport, isHdrFrame, pickCodec, supportedHevcMain10, type CodecPref, type StreamCodec } from "./rpHdr";
import type { MotionReading } from "./motion";
import type { DualSenseSink } from "./dualSense";
import { getEngineUrl } from "../state/engine";
import { log } from "../state/logs";
import { hevcCodecString, spsVisibleRect, visibleFix, type VisibleRect } from "./rpSps";
import { safeGetItem, safeRemoveItem, safeSetItem } from "./safeStorage";
import { ClockSync, inputEstimate, LatencyStages, stagesLogText, type StageSummary } from "./rpLatency";
import { decoderVerdict, FED_RECENT_MS, MAX_REBUILDS } from "./rpDecoderWatch";
import type { RpKeyboardMsg, RpKeyboardSend } from "./rpKeyboard";
import { parseSnapshotRequest, takeSnapshot } from "./rpSnapshot";
import { OpusPipe } from "./rpAudioPipe";
import { relayoutElement, watchRestore } from "./repaintOnRestore";
// A file of the app's own: the apps' Content-Security-Policy refuses a worklet from a blob: URL.
import audioWorkletUrl from "./rpAudioWorklet.js?url&no-inline";

/** Button bits, as the console (and chiaki) number them. */
export const PadButton = {
  CROSS: 1,
  CIRCLE: 2,
  SQUARE: 4,
  TRIANGLE: 8,
  LEFT: 16,
  RIGHT: 32,
  UP: 64,
  DOWN: 128,
  L1: 256,
  R1: 512,
  L3: 1024,
  R3: 2048,
  OPTIONS: 4096,
  SHARE: 8192,
  TOUCHPAD: 16384,
  PS: 32768,
} as const;

export interface PadState {
  buttons: number;
  l2: number;
  r2: number;
  lx: number;
  ly: number;
  rx: number;
  ry: number;
  /** One finger on the touchpad, in its 1920×942 space, or none. */
  touch?: [number, number] | null;
  /** A phone's motion sensors in the controller's axes (lib/motion.ts), or none. */
  motion?: MotionReading | null;
  /** The buttons the game's turbo is pressing above 10 a second: the engine holds them about a
   *  frame instead of three (rp_stream/pad_pace.rs, lib/gameControls.ts). */
  turbo?: number;
}

export const IDLE_PAD: PadState = { buttons: 0, l2: 0, r2: 0, lx: 0, ly: 0, rx: 0, ry: 0 };

/** The touchpad point for a position on the picture (fractions 0-1 across and down). */
export function touchAt(fx: number, fy: number): [number, number] {
  const c = (v: number) => Math.max(0, Math.min(1, v));
  return [Math.round(c(fx) * 1919), Math.round(c(fy) * 941)];
}

/** What a key does: a button, a stick direction, or a trigger. */
export type KeyAction =
  | { button: number }
  | { stick: "lx" | "ly" | "rx" | "ry"; dir: -1 | 1 }
  | { trigger: "l2" | "r2" };

/** The default keyboard map (KeyboardEvent.code → action). */
export const DEFAULT_KEYMAP: Record<string, KeyAction> = {
  ArrowUp: { button: PadButton.UP },
  ArrowDown: { button: PadButton.DOWN },
  ArrowLeft: { button: PadButton.LEFT },
  ArrowRight: { button: PadButton.RIGHT },
  Enter: { button: PadButton.CROSS },
  Space: { button: PadButton.CROSS },
  Backspace: { button: PadButton.CIRCLE },
  Escape: { button: PadButton.CIRCLE },
  KeyQ: { button: PadButton.SQUARE },
  KeyE: { button: PadButton.TRIANGLE },
  KeyZ: { button: PadButton.L1 },
  KeyC: { button: PadButton.R1 },
  Digit1: { trigger: "l2" },
  Digit3: { trigger: "r2" },
  KeyF: { button: PadButton.L3 },
  KeyG: { button: PadButton.R3 },
  Tab: { button: PadButton.OPTIONS },
  Backquote: { button: PadButton.SHARE },
  KeyH: { button: PadButton.PS },
  KeyT: { button: PadButton.TOUCHPAD },
  KeyW: { stick: "ly", dir: -1 },
  KeyS: { stick: "ly", dir: 1 },
  KeyA: { stick: "lx", dir: -1 },
  KeyD: { stick: "lx", dir: 1 },
  KeyI: { stick: "ry", dir: -1 },
  KeyK: { stick: "ry", dir: 1 },
  KeyJ: { stick: "rx", dir: -1 },
  KeyL: { stick: "rx", dir: 1 },
};

/** The actions a key can be set to, in the order the remap list shows them. */
export const PAD_ACTIONS: ReadonlyArray<{ id: string; label: string; action: KeyAction }> = [
  { id: "up", label: "D-pad up", action: { button: PadButton.UP } },
  { id: "down", label: "D-pad down", action: { button: PadButton.DOWN } },
  { id: "left", label: "D-pad left", action: { button: PadButton.LEFT } },
  { id: "right", label: "D-pad right", action: { button: PadButton.RIGHT } },
  { id: "cross", label: "✕", action: { button: PadButton.CROSS } },
  { id: "circle", label: "○", action: { button: PadButton.CIRCLE } },
  { id: "square", label: "□", action: { button: PadButton.SQUARE } },
  { id: "triangle", label: "△", action: { button: PadButton.TRIANGLE } },
  { id: "l1", label: "L1", action: { button: PadButton.L1 } },
  { id: "r1", label: "R1", action: { button: PadButton.R1 } },
  { id: "l2", label: "L2", action: { trigger: "l2" } },
  { id: "r2", label: "R2", action: { trigger: "r2" } },
  { id: "l3", label: "L3", action: { button: PadButton.L3 } },
  { id: "r3", label: "R3", action: { button: PadButton.R3 } },
  { id: "options", label: "Options", action: { button: PadButton.OPTIONS } },
  { id: "create", label: "Create", action: { button: PadButton.SHARE } },
  { id: "ps", label: "PS button", action: { button: PadButton.PS } },
  { id: "touchpad", label: "Touchpad press", action: { button: PadButton.TOUCHPAD } },
  { id: "ls_up", label: "Left stick up", action: { stick: "ly", dir: -1 } },
  { id: "ls_down", label: "Left stick down", action: { stick: "ly", dir: 1 } },
  { id: "ls_left", label: "Left stick left", action: { stick: "lx", dir: -1 } },
  { id: "ls_right", label: "Left stick right", action: { stick: "lx", dir: 1 } },
  { id: "rs_up", label: "Right stick up", action: { stick: "ry", dir: -1 } },
  { id: "rs_down", label: "Right stick down", action: { stick: "ry", dir: 1 } },
  { id: "rs_left", label: "Right stick left", action: { stick: "rx", dir: -1 } },
  { id: "rs_right", label: "Right stick right", action: { stick: "rx", dir: 1 } },
];

const sameAction = (a: KeyAction, b: KeyAction) => JSON.stringify(a) === JSON.stringify(b);

/** The keys bound to an action. */
export function keysFor(keymap: Record<string, KeyAction>, action: KeyAction): string[] {
  return Object.entries(keymap)
    .filter(([, a]) => sameAction(a, action))
    .map(([k]) => k);
}

/** `keymap` with `code` as the one key for `action` (taken off whatever it did before). */
export function rebind(keymap: Record<string, KeyAction>, action: KeyAction, code: string): Record<string, KeyAction> {
  const next: Record<string, KeyAction> = {};
  for (const [k, a] of Object.entries(keymap)) {
    if (k !== code && !sameAction(a, action)) next[k] = a;
  }
  next[code] = action;
  return next;
}

const KEYMAP_KEY = "ps5upload.rp_keymap.v1";

/** The keyboard map in use on this device (the default until changed). */
export function loadKeymap(): Record<string, KeyAction> {
  try {
    const raw = safeGetItem(KEYMAP_KEY);
    const v = raw ? (JSON.parse(raw) as Record<string, KeyAction>) : null;
    return v && typeof v === "object" && Object.keys(v).length ? v : DEFAULT_KEYMAP;
  } catch {
    return DEFAULT_KEYMAP;
  }
}

export function saveKeymap(keymap: Record<string, KeyAction> | null): void {
  if (keymap) safeSetItem(KEYMAP_KEY, JSON.stringify(keymap));
  else safeRemoveItem(KEYMAP_KEY);
}

const clampAxis = (v: number) => Math.max(-32768, Math.min(32767, Math.round(v)));

/** The pad the held keys amount to. */
export function padFromKeys(held: Iterable<string>, keymap: Record<string, KeyAction> = DEFAULT_KEYMAP): PadState {
  const p = { ...IDLE_PAD };
  for (const code of held) {
    const a = keymap[code];
    if (!a) continue;
    if ("button" in a) p.buttons |= a.button;
    else if ("stick" in a) p[a.stick] = clampAxis(p[a.stick] + a.dir * 32767);
    else p[a.trigger] = 255;
  }
  return p;
}

/** Gamepad API "standard" mapping, button index → pad bit (6/7 are the analog triggers). */
const STANDARD_BUTTONS = [
  PadButton.CROSS, PadButton.CIRCLE, PadButton.SQUARE, PadButton.TRIANGLE, PadButton.L1, PadButton.R1, 0, 0,
  PadButton.SHARE, PadButton.OPTIONS, PadButton.L3, PadButton.R3, PadButton.UP, PadButton.DOWN, PadButton.LEFT,
  PadButton.RIGHT, PadButton.PS, PadButton.TOUCHPAD,
];

/** Where one control of the console's pad comes from on a controller: a button (by index), or
 *  half of an axis (index, the direction it moves when pressed, and where it rests: -1 for a
 *  trigger reported as an axis that rests at one end). */
export type PadSource = { b: number } | { a: number; d: 1 | -1; rest?: number } | HatSource;

/** A d-pad reported as one axis (non-standard pads in Chromium and Firefox): 8 positions in
 *  sevenths from -1 (up) clockwise to 1 (up left), resting outside -1..1 (9/7). `at`: the position,
 *  0-7, this source means; its two neighbours (the diagonals) count too. */
export interface HatSource {
  h: number;
  at: number;
}

/** Up, right, down and left on a hat. */
export const HAT_UP = 0;
export const HAT_RIGHT = 2;
export const HAT_DOWN = 4;
export const HAT_LEFT = 6;

/** Where a hat axis's reading is, 0-7, or null at rest. */
export function hatPosition(v: number): number | null {
  if (!(Math.abs(v) <= 1.01)) return null;
  return Math.round((v + 1) * 3.5) % 8;
}

/** A stick axis: the controller's axis index, inverted when it reads the other way round. */
export interface PadAxis {
  a: number;
  inv?: boolean;
}

/** How a controller's buttons and axes become the console's pad. Several sources may drive one
 *  button (an extra back button set to ✕ as well, say). */
export interface PadProfile {
  buttons: Array<[bit: number, src: PadSource]>;
  l2: PadSource | null;
  r2: PadSource | null;
  lx: PadAxis | null;
  ly: PadAxis | null;
  rx: PadAxis | null;
  ry: PadAxis | null;
}

/** The browser's "standard" layout (W3C Gamepad standard mapping). */
export const STANDARD_PROFILE: PadProfile = {
  buttons: STANDARD_BUTTONS.flatMap((bit, i): Array<[number, PadSource]> => (bit ? [[bit, { b: i }]] : [])),
  l2: { b: 6 },
  r2: { b: 7 },
  lx: { a: 0 },
  ly: { a: 1 },
  rx: { a: 2 },
  ry: { a: 3 },
};

/** How far (0-1) a source is pressed on `g`. */
export function sourceValue(g: Gamepad, src: PadSource): number {
  if ("b" in src) {
    const b = g.buttons[src.b];
    return b ? Math.max(b.pressed ? 1 : 0, b.value) : 0;
  }
  if ("h" in src) {
    const at = hatPosition(g.axes[src.h] ?? 9);
    if (at === null) return 0;
    const off = (at - src.at + 8) % 8;
    return off === 0 || off === 1 || off === 7 ? 1 : 0;
  }
  const v = g.axes[src.a];
  if (v === undefined) return 0;
  const rest = src.rest ?? 0;
  return Math.min(1, Math.max(0, (v - rest) / (src.d - rest)));
}

/** A controller the browser lays out the standard way, or one it does not recognize: then the
 *  standard order is the best guess (most controllers follow it), until it is mapped. */
export function guessProfile(g: Gamepad): PadProfile {
  if (g.mapping === "standard") return STANDARD_PROFILE;
  const known = hatGuesses.get(g.id);
  if (known) return known;
  // Only a hat at rest reads outside -1..1: one seen once is that pad's for this run.
  const hat = g.axes.findIndex((v) => Math.abs(v) > 1.05);
  if (hat < 0) return STANDARD_PROFILE;
  const prof: PadProfile = {
    ...STANDARD_PROFILE,
    buttons: [
      ...STANDARD_PROFILE.buttons,
      [PadButton.UP, { h: hat, at: HAT_UP }],
      [PadButton.RIGHT, { h: hat, at: HAT_RIGHT }],
      [PadButton.DOWN, { h: hat, at: HAT_DOWN }],
      [PadButton.LEFT, { h: hat, at: HAT_LEFT }],
    ],
  };
  hatGuesses.set(g.id, prof);
  return prof;
}

/** The usual layout plus a hat d-pad, per controller seen with one. */
const hatGuesses = new Map<string, PadProfile>();

/** The pad the connected controllers amount to (all of them, merged), each read through its
 *  profile. */
export function padFromGamepads(
  pads: ReadonlyArray<Gamepad | null>,
  profileOf: (g: Gamepad) => PadProfile = guessProfile,
): PadState {
  const p = { ...IDLE_PAD };
  const axis = (v: number) => (Math.abs(v) < 0.08 ? 0 : clampAxis(v * 32767));
  // Several pads: the stick pushed furthest wins, so a resting pad does not cancel another.
  const pick = (cur: number, v: number) => (Math.abs(v) > Math.abs(cur) ? v : cur);
  const stick = (g: Gamepad, ax: PadAxis | null) => (ax ? axis((g.axes[ax.a] ?? 0) * (ax.inv ? -1 : 1)) : 0);
  for (const g of pads) {
    if (!g || !g.connected) continue;
    const prof = profileOf(g);
    for (const [bit, src] of prof.buttons) if (sourceValue(g, src) > 0.5) p.buttons |= bit;
    if (prof.l2) p.l2 = Math.max(p.l2, Math.round(sourceValue(g, prof.l2) * 255));
    if (prof.r2) p.r2 = Math.max(p.r2, Math.round(sourceValue(g, prof.r2) * 255));
    p.lx = pick(p.lx, stick(g, prof.lx));
    p.ly = pick(p.ly, stick(g, prof.ly));
    p.rx = pick(p.rx, stick(g, prof.rx));
    p.ry = pick(p.ry, stick(g, prof.ry));
  }
  return p;
}

export function mergePads(a: PadState, b: PadState): PadState {
  return {
    buttons: a.buttons | b.buttons,
    l2: Math.max(a.l2, b.l2),
    r2: Math.max(a.r2, b.r2),
    lx: clampAxis(a.lx + b.lx),
    ly: clampAxis(a.ly + b.ly),
    rx: clampAxis(a.rx + b.rx),
    ry: clampAxis(a.ry + b.ry),
    touch: a.touch ?? b.touch ?? null,
    motion: a.motion ?? b.motion ?? null,
  };
}

/** Play the console's rumble on every controller that can (Gamepad API "dual-rumble").
 *  Each message is the current level, so it is played for a while and replaced by the next;
 *  zero stops it. */
export function playRumble(
  pads: ReadonlyArray<Gamepad | null>,
  strong: number,
  weak: number,
  /** The browser's own list: the app's controller readers (lib/nativePad.ts) have no motors, and
   *  the browser may see the same controller and be able to shake it. */
  browserPads: () => ReadonlyArray<Gamepad | null> = () => globalThis.navigator?.getGamepads?.() ?? [],
): void {
  const actuator = (g: Gamepad | null) =>
    (g as (Gamepad & { vibrationActuator?: GamepadHapticActuator | null }) | null)?.vibrationActuator;
  for (const g of pads.some(actuator) ? pads : browserPads()) {
    const act = actuator(g);
    if (!act) continue;
    if (strong === 0 && weak === 0) {
      void act.reset?.();
      continue;
    }
    void act.playEffect?.("dual-rumble", {
      duration: 1000,
      strongMagnitude: strong / 255,
      weakMagnitude: weak / 255,
    });
  }
}

export { avcCodecString, isKeyframe, supportedHevc } from "./rpCodec";
import { avcCodecString, isKeyframe, noPictureStep, supportedHevc } from "./rpCodec";
import { avcDescription, lengthPrefixed } from "./fmp4";

function sameBytes(a: Uint8Array | null, b: Uint8Array): boolean {
  return !!a && a.length === b.length && a.every((x, i) => x === b[i]);
}

/** H.264 as the PS5 sends it (Main, up to level 5.0 for 1080p), then plainer forms. */
const AVC_CANDIDATES = ["avc1.4d4032", "avc1.640032", "avc1.4d4028", "avc1.42e01f"];

/** False only when this device answers that it decodes none of the H.264 forms; a check that
 *  throws is not a no, so a device with a quirky check still gets to try. */
export async function canDecodeAvc(width: number, height: number): Promise<boolean> {
  if (typeof VideoDecoder === "undefined") return false;
  return withDecoderStart(async () => {
    let answered = false;
    for (const codec of AVC_CANDIDATES) {
      try {
        if ((await VideoDecoder.isConfigSupported({ codec, codedWidth: width, codedHeight: height })).supported) {
          return true;
        }
        answered = true;
      } catch {
        // Not an answer.
      }
    }
    return !answered;
  });
}

/** True for a decoder error that means "no decoder for this codec here", which starting over
 *  cannot fix (Linux without GStreamer's H.264 decoder: "No decoder found for codec …"). */
export function isNoDecoderError(e: unknown): boolean {
  const name = (e as { name?: string } | null)?.name ?? "";
  return name === "NotSupportedError" || /no decoder/i.test(String(e));
}

/** HEVC whenever this device can decode it (same picture in fewer bits); else H.264. The PS5
 *  offers only these two. */
export async function chooseCodec(width: number, height: number): Promise<"h265" | "h264"> {
  return (await withDecoderStart(() => supportedHevc(width, height))) ? "h265" : "h264";
}

/** How the picture fills the place it is shown in: the whole picture with black bars (fit),
 *  no bars with the edges cut off (fill), or no bars with the picture pulled out of shape
 *  (stretch). */
export type PictureFit = "fit" | "fill" | "stretch";

const FIT_CLASS: Record<PictureFit, string> = {
  fit: "object-contain",
  fill: "object-cover",
  stretch: "object-fill",
};

/** The picture sizes in the order the menus list them. */
export const FIT_ORDER: PictureFit[] = ["fit", "fill", "stretch"];

/** The canvas's classes for a fit. It never takes a pointer: presses belong to the stage it is
 *  on and to the controls drawn over it. The browser sizes the picture, as a video player does:
 *  rp-picture (index.css) against the place it is in, by the fit in its data-fit and the
 *  picture's shape in --rp-shape. Nothing here measures the place to lay it out (upscaling reads
 *  the box's size only to pick how many pixels to draw). */
export function canvasClass(fit: PictureFit): string {
  return `rp-picture pointer-events-none ${FIT_CLASS[fit] ?? FIT_CLASS.fit}`;
}

/**
 * Nobody can see this frame and nothing reads it: the window is hidden, or the canvas is off the
 * page (another screen, mini player off). Drawing it would still be a GPU upload and a shader pass,
 * 60 times a second, on battery. The next frame someone can see draws as usual.
 */
export function pictureUnseen(s: {
  hidden: boolean;
  detached: boolean;
  /** Someone waits for the next drawn frame (a snapshot). */
  waiting: boolean;
  holds: number;
}): boolean {
  return (s.hidden || s.detached) && !s.waiting && s.holds === 0;
}

/** True when this device can play a stream at all. */
export function canStream(): boolean {
  return typeof VideoDecoder !== "undefined" && typeof WebSocket !== "undefined";
}

/** Why a stream can't play here: browsers only give VideoDecoder to a secure page, so on a plain
 *  http:// address (the Docker web UI opened from another device) even a current Chrome has none,
 *  and telling that visitor to get a newer browser can't help. */
export function streamRefusal(): "insecure_page" | "no_decoder" | null {
  if (canStream()) return null;
  return typeof window !== "undefined" && window.isSecureContext === false ? "insecure_page" : "no_decoder";
}

export interface StreamStats {
  /** The browser holds the sound back until the page is clicked or a key pressed (autoplay
   *  rules: the session started without a click right before it). */
  soundBlocked?: boolean;
  /** This device can't decode the stream's sound, so there is none. */
  soundUnavailable?: boolean;
  fps: number;
  mbps: number;
  decodeMs: number;
  codec: string;
  /** The stream is the console's HDR one. */
  hdr?: boolean;
  lossPct: number;
  rebuilt: number;
  lost: number;
  /** Sound waiting to play, in ms, and how often it ran dry since the start. */
  audioMs: number;
  underruns: number;
  /** Round trip from this page to the engine and back, ms (null until measured, or an engine
   *  that does not answer pings). The console's leg cannot be measured: no shared clock. */
  rttMs: number | null;
  /** The engine's round trip to the console and back, ms (null until measured, or an older
   *  engine): its Takion messages out and their acknowledgements in. */
  consoleRttMs?: number | null;
  /** Each frame's stages over the last 2 s (rpLatency.ts); null until a frame was timed. */
  stages?: StageSummary | null;
  /** Times the video decoder was rebuilt because it stopped giving pictures (rpDecoderWatch.ts). */
  decoderRestarts?: number;
  /** How the sound has played since the start. */
  audio?: AudioHealth;
}

/** How the sound plays and has played since the start (rpAudioWorklet.js). */
export interface AudioHealth {
  /** "worker": decoded off the page's main thread, played from the worklet's buffer; "page":
   *  decoded on the page, played from the worklet's buffer; "scheduled": no worklet here, each
   *  frame scheduled from the page as its own buffer (a busy page leaves gaps). */
  path: "worker" | "page" | "scheduled";
  /** The buffer while playing: on average and at its lowest, ms. */
  avgMs: number;
  minMs: number;
  /** Time played a little faster or slower to keep the buffer at its size, ms. */
  corrMs: number;
  /** Frames that never came, covered by the one before. */
  concealed: number;
  /** Sound dropped to catch up after the page stalled, ms. */
  trimmedMs: number;
}

/** The sound's part of the latency log line. */
export function audioLogText(h: AudioHealth, underruns: number, targetMs: number): string {
  const where =
    h.path === "worker"
      ? "decoded in a worker, played by the worklet"
      : h.path === "page"
        ? "decoded on the page, played by the worklet"
        : "decoded on the page, each frame scheduled (no worklet)";
  const s = (ms: number) => (ms >= 1000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms)} ms`);
  return (
    `sound ${where}: ${underruns} underruns, buffer avg ${Math.round(h.avgMs)} / min ${Math.round(h.minMs)} ms ` +
    `(target ${Math.round(targetMs)}), speed corrected for ${s(h.corrMs)}, ${h.concealed} frames covered` +
    (h.trimmedMs > 0 ? `, ${s(h.trimmedMs)} dropped after stalls` : "")
  );
}

/** A round trip from a ping's own timestamp, smoothed into the last value (a spike moves it a
 *  quarter of the way); null when `sentAt` is not a number. */
export function nextRtt(prev: number | null, sentAt: unknown, now: number): number | null {
  if (typeof sentAt !== "number" || !Number.isFinite(sentAt)) return prev;
  const rtt = Math.max(0, now - sentAt);
  return prev === null ? rtt : prev + (rtt - prev) / 4;
}

export type StreamPhase =
  | "connecting"
  | "waking"
  | "starting"
  | "playing"
  /** The socket to the engine dropped; the session is still running and this rejoins it. */
  | "reconnecting"
  /** Another window took the stream; `takeOver()` brings it back here. */
  | "moved"
  | "ended"
  | "error";

export interface StreamError {
  /** Stable, from the engine: busy, in_use (another device has the console), busy_other, asleep,
   *  unreachable, keys, wake_failed, passcode_cancelled, passcode_timeout, video_refused, lost
   *  (dropped mid-stream), other — or client-side: decode, no_decoder, socket, console_ended (the
   *  console ended it; `detail` is what it said), no_session (over while the socket was away). */
  code: string;
  retryS?: number;
  /** busy_other: the console that is streaming. */
  host?: string;
  detail: string;
}

/** The console waits for the user's sign-in passcode. */
export interface PasscodeAsk {
  retry: boolean;
  waitS: number;
}

export { isPasscode };

export interface StreamOptions {
  host: string;
  registKey: string;
  rpKey: string;
  width: number;
  height: number;
  fps?: number;
  /** How long the engine keeps the session for this app to come back to. */
  keepAliveS?: number;
  /** Sound kept ready to play: lower is less delay, higher rides out a busy network. */
  audioBufferMs?: number;
  /** Join the session the engine already runs for this console; never start one. */
  attach?: boolean;
  /** Ask the engine for a shorter video queue and skip to the next keyframe sooner. */
  lowLatency?: boolean;
  /** Set the session up for the microphone (experimental; Settings). Only then is `mic`
   *  in the start message, so a session without it starts exactly as before. */
  mic?: boolean;
  /** Instant replay window in seconds (15/30/60), 0 = off. */
  replayS?: number;
  /** A DualSense is attached (desktop app): start the session in DualSense mode and hand its
   *  trigger effects and haptics here (lib/dualSense.ts). Absent, the session is as before. */
  dualSense?: DualSenseSink;
  /** Picture tools to start with (rpPicture.ts). */
  picture?: PictureTools;
  /** "software": never the hardware decoder (Settings › Video decoding). */
  decoding?: "auto" | "software";
  /** "h264": never ask for HEVC; "hdr": the console's HDR stream where it can be shown in HDR
   *  here (Settings › Video format). */
  codec?: CodecPref;
  /** The video bitrate to ask the console for, kbit/s (Settings › Picture); absent = the
   *  engine's default for the resolution. */
  bitrateKbps?: number;
  /** The picture tools' GPU pass turned itself off (no WebGL, or the device lags). */
  onPictureOff?: (why: "failed" | "slow" | "linux") => void;
  /** HDR was asked for and did not work here (`started`: the session is in HDR, shown as SDR). */
  onHdrOff?: (why: string, started: boolean) => void;
  /** This device showed no picture from the console's HEVC, so the session ended: the next start
   *  should ask for H.264. */
  onHevcFailed?: () => void;
  /** How the picture fills the place it is shown in (default: the whole picture). */
  fit?: PictureFit;
  /** False: this session starts with no picture, sound or controller on this device (#670,
   *  #649). Absent: whatever the session already has (a page joining a running one). */
  pictureHere?: boolean;
  /** The picture on this device went on or off, from here or from the engine's API. */
  onPictureHere?: (on: boolean) => void;
  /** False: the console's rumble is not played on the controllers. */
  rumble?: () => boolean;
  onPhase: (p: StreamPhase) => void;
  onStats: (s: StreamStats) => void;
  onError: (e: StreamError) => void;
  /** The console's light-bar colour for this pad. Browsers cannot set a controller's light
   *  bar (the Gamepad API has no such control), so the screen shows it. */
  onLight?: (rgb: [number, number, number]) => void;
  /** The page went to the background: let go of what only it can let go of. */
  onHidden?: () => void;
  /** The engine's answer about native keyboard & mouse (`{"kbm": …}`, lib/kbmInput.ts). */
  onKbm?: (s: unknown) => void;
  /** A phone's touchpad & keyboard, forwarded by the engine (`{"pad_kbm": …}`, lib/kbmInput.ts). */
  onPadKbm?: (m: unknown) => void;
  /** The console's on-screen keyboard opened, changed or closed (lib/rpKeyboard.ts). */
  onKeyboard?: (m: RpKeyboardMsg) => void;
  /** The console asks for the user's sign-in passcode before it starts (`retry`: the last one
   *  was wrong; `waitS`: how long the engine waits), or null once it no longer does. Answer with
   *  `sendPasscode` or `cancelPasscode`. */
  onPasscode?: (ask: PasscodeAsk | null) => void;
  /** The pad state to send; polled every animation frame and on key events. */
  readPad: () => PadState;
  /** The controllers rumble is played on (default: this page's). A picture-in-picture window
   *  has its own list where the engine gives controllers only to the window in front. */
  gamepads?: () => ReadonlyArray<Gamepad | null>;
}

/** Waits between tries to rejoin after the socket dropped (the last one repeats), and for how
 *  long in all. */
const RECONNECT_WAITS_MS = [250, 500, 1000, 2000, 4000];
const RECONNECT_FOR_MS = 30_000;
/** A first socket that never opened is tried this many more times, this far apart. */
const FIRST_SOCKET_TRIES = 2;
const FIRST_SOCKET_WAIT_MS = 1000;
/** Frames kept for a decoder not ready yet: 10 s at 60 fps. */
const MAX_QUEUED_FRAMES = 600;
/** Frames whose times are kept for the latency stages: ~2 s at 60 fps, far more than in flight. */
const FRAME_SLOTS = 128;
/** A "latency:" line in the logs this often while playing (and so in a bug report's logs). */
const LATENCY_LOG_MS = 60_000;
/** The decoder this far behind: skip to the next keyframe rather than show old pictures. */
const VIDEO_BACKLOG = 4;
const VIDEO_BACKLOG_LOW_LATENCY = 2;

/** End the session the engine runs for `host` without showing it (it moved to another window,
 *  or this page was reloaded). */
export function stopEngineSession(host: string): void {
  const ws = new WebSocket(getEngineUrl().replace(/^http/, "ws") + "/api/ps5/rp/ws");
  ws.onopen = () => {
    ws.send(JSON.stringify({ host, regist_key: "", rp_key: "", attach_only: true }));
    ws.send(JSON.stringify({ stop: true }));
    window.setTimeout(() => ws.close(), 1000);
  };
}

/** One stream. It belongs to the app, not to a screen: leaving the screen keeps it (and the
 *  console's session) running. `stop()` ends it. */
export class RemotePlayStream {
  /** The picture; the screen puts it on the page wherever it shows the stream. It is replaced
   *  (in place on the page) only when its drawing has to fall back to the 2D canvas. */
  get canvas(): HTMLCanvasElement {
    return this.cv;
  }
  private cv: HTMLCanvasElement;
  private ws: WebSocket | null = null;
  private video: VideoDecoder | null = null;
  /** The decoder is being made (videoDecoderGuard): ended by its first picture or the stream's end. */
  private decoderStart: DecoderStart | null = null;
  private header: Uint8Array | null = null;
  private hevc = false;
  private codecChoice: StreamCodec = "h264";
  private sawKey = false;
  private ts = 0;
  private queued: EncodedVideoChunk[] = [];
  /** Per-frame times, in slots by the chunk's timestamp (no allocation per frame): the chunk's
   *  timestamp, when it arrived, when it went to the decoder, and when the engine had it whole
   *  (page clock; NaN when unknown). */
  private slotTs = new Float64Array(FRAME_SLOTS).fill(-1);
  private slotRecv = new Float64Array(FRAME_SLOTS);
  private slotSubmit = new Float64Array(FRAME_SLOTS);
  private slotEngine = new Float64Array(FRAME_SLOTS);
  private stages = new LatencyStages();
  private clock = new ClockSync();
  /** A picture given by the decoder and not yet shown (the time it came out), for "draw". */
  private outputPending = 0;
  /** The decoder watchdog (rpDecoderWatch.ts). */
  private lastFedAt = 0;
  private waitingSince = 0;
  private rebuilds = 0;
  private gaveUp = false;
  private decoderRestarts = 0;
  private lastLatencyLog = 0;
  private audioCtx: AudioContext | null = null;
  private gainNode: GainNode | null = null;
  private worklet: AudioWorkletNode | null = null;
  /** The worklet's module is still loading: decoded sound is left out until it is ready. */
  private workletLoading = false;
  /** Stops listening for the click that starts held-back sound (watchSoundBlocked). */
  private soundUnlockOff: (() => void) | null = null;
  /** Stops laying the picture out again after the Linux app's window comes back. */
  private restoreOff: (() => void) | null = null;
  private hiddenOff: (() => void) | null = null;
  /** A socket has reached the engine (a later drop is a session to rejoin). */
  private socketOpened = false;
  private firstSocketTries = 0;
  /** The sound decoded on the page (no decoding worker here), made at the first header. */
  private audioPipe: OpusPipe | null = null;
  /** The sound decoded in a worker that feeds the worklet itself (rpAudioWorker.ts). */
  private audioWorker: Worker | null = null;
  /** The last audio header, for the page to take over if the worker fails. */
  private audioHeader: Uint8Array | null = null;
  private soundUnavailable = false;
  /** No worklet: when the next scheduled buffer starts (AudioContext time). */
  private audioNext = 0;
  private audioMs = 0;
  private underruns = 0;
  private audioTargetMs = 60;
  private audioHealth: AudioHealth = { path: "scheduled", avgMs: 0, minMs: 0, corrMs: 0, concealed: 0, trimmedMs: 0 };
  private workerConcealed = 0;
  private volume = 1;
  private lastPad = "";
  private raf = 0;
  /** The window whose animation frames poll the pad: this page's, or a picture-in-picture
   *  window's, which keeps them coming while this page is hidden or minimized. */
  private frameWin: Window | null = null;
  private rafWin: Window = window;
  private ticking = false;
  private decoded = 0;
  private bytes = 0;
  private decodeMs = 0;
  private codec = "";
  private statsTimer = 0;
  private lastLoss = { lossPct: 0, rebuilt: 0, lost: 0 };
  private consoleRttMs: number | null = null;
  /** The console's last connection-quality report, raw (engine rp_stream proto.rs), for logs. */
  private quality: unknown = null;
  private rttMs: number | null = null;
  private phase: StreamPhase = "connecting";
  /** The session is over (stopped here, ended by the engine or the console). */
  private ended = false;
  private stopping = false;
  private engineEnded = false;
  /** The console asked for the passcode and has not moved on yet. */
  private passcodeAsked = false;
  private moved = false;
  private reconnectSince = 0;
  private reconnectTry = 0;
  private reconnectTimer = 0;
  /** The page's mic is on (remotePlayMic.ts); said again on every reattach. */
  private micOn = false;
  /** Pictures drawn on this page and when the last one was (Date.now()), for a local automation
   *  client's snapshot (rpSnapshot.ts), and who waits for the next one. */
  private frameSeq = 0;
  private frameAt = 0;
  /** See `holdPicture`. */
  private pictureHolds = 0;
  private frameWaiters = new Set<() => void>();

  /** The visible picture the stream's SPS describes (rpSps.ts), to put right a decoder that
   *  reports a smaller one (Edge on Windows). */
  private spsRect: VisibleRect | null = null;
  private loggedVisibleFix = false;
  /** The hardware decoder gave a picture that could not be put right: software from now on. */
  private softwareAfterBadFrame = false;
  /** Decoder failures in a row before any picture came. */
  private noPictureFails = 0;

  /** The frame with the stream's own visible picture where the decoder reported a smaller one
   *  (hardware HEVC decoding in Chromium browsers on Windows: Edge, so the Windows app, and
   *  Opera GX; the top-left of the picture
   *  showed enlarged and cut off, #538/#575). The new frame shares the decoded pixels; the
   *  original is closed. Any other frame comes back as it is. */
  private withRealVisibleRect(frame: VideoFrame): VideoFrame {
    const fix = visibleFix(frame, this.spsRect);
    if (!fix) {
      // Smaller than the stream says, but not fixable here (the coded size is small too): say
      // so once, for a report.
      const v = frame.visibleRect;
      const sps = this.spsRect;
      if (sps && v && (v.width < sps.w - 16 || v.height < sps.h - 16)) {
        if (!this.softwareAfterBadFrame && this.o.decoding !== "software") {
          // Start the decoder over in software, which reports the picture right.
          this.softwareAfterBadFrame = true;
          log.warn(
            "remote-play",
            `the decoder reported a ${v.width}x${v.height} picture in a ${frame.codedWidth}x${frame.codedHeight} frame; ` +
              `the stream says ${sps.w}x${sps.h}, which does not fit: decoding in software from now on`,
          );
          const bad = this.video;
          queueMicrotask(() => {
            if (this.video !== bad) return;
            try {
              bad?.close();
            } catch {
              // Already closed.
            }
            this.video = null;
            this.sawKey = false;
            if (this.header) this.openVideo();
            this.ws?.send(JSON.stringify({ keyframe: true }));
          });
        } else if (!this.loggedVisibleFix) {
          this.loggedVisibleFix = true;
          log.warn(
            "remote-play",
            `the decoder reported a ${v.width}x${v.height} picture in a ${frame.codedWidth}x${frame.codedHeight} frame; ` +
              `the stream says ${sps.w}x${sps.h}, also in software: drawn as reported`,
          );
        }
      }
      return frame;
    }
    try {
      const fixed = new VideoFrame(frame, {
        visibleRect: { x: fix.x, y: fix.y, width: fix.w, height: fix.h },
        displayWidth: fix.w,
        displayHeight: fix.h,
      });
      if (!this.loggedVisibleFix) {
        this.loggedVisibleFix = true;
        const v = frame.visibleRect;
        log.warn(
          "remote-play",
          `the decoder reported a ${v?.width}x${v?.height} picture in a ${frame.codedWidth}x${frame.codedHeight} frame; ` +
            `the stream says ${fix.w}x${fix.h}: drawing that`,
        );
      }
      frame.close();
      return fixed;
    } catch {
      return frame;
    }
  }

  /** The picture's shape (width / height) its layout follows, as last set on the canvas. */
  private shape = 16 / 9;

  /** The picture on this device (#670, #649). Off, nothing runs for it: no decoder, canvas
   *  context, sound, or per-frame pad polling, and the engine sends none of it. */
  private pictureOn = true;
  /** What this page asked of the engine, repeated on every attach; null: it has not asked (a
   *  reload takes what the session has). */
  private pictureAsked: boolean | null = null;
  private hdrOpening = false;

  constructor(private readonly o: StreamOptions) {
    if (o.pictureHere === false) {
      this.pictureOn = false;
      this.pictureAsked = false;
    }
    this.cv = document.createElement("canvas");
    this.cv.dataset.testid = "rp-canvas";
    // Until the first frame, a 16:9 picture.
    this.cv.style.setProperty("--rp-shape", String(16 / 9));
    this.setFit(o.fit ?? "fit");
    this.replayS = o.replayS ?? 0;
    if (o.picture) this.setPicture(o.picture);
    // The canvas gets its context at the first frame (drawFrame), or at the start for HDR.
  }

  /** The video's codec string as its decoder was set up with ("" before the first picture). */
  get videoCodec(): string {
    return this.codec;
  }

  /** Picture tools (rpPicture.ts). */
  private tools: PictureTools = DEFAULT_PICTURE;
  /** How frames are drawn, chosen at the first frame: WebGPU in HDR, WebGL 2 (the picture tools
   *  run there; with none on it is one plain pass), or the 2D canvas where WebGL 2 is missing or
   *  failed. Not `desynchronized` in any: Chromium honours it only on Windows and ChromeOS,
   *  where the canvas is presented through its own swap chain/overlay instead of the page's
   *  compositor. The Windows app (only there) showed the picture enlarged from the top-left
   *  corner, cropped on the right and bottom (#538): the canvas's CSS size and object-fit were
   *  not applied. The page's compositor, as on every other platform, scales it to its box. */
  private drawer:
    | { kind: "hdr"; hdr: HdrPresenter }
    | { kind: "gl"; pass: PicturePass }
    | { kind: "2d"; ctx: CanvasRenderingContext2D | null }
    | null = null;
  /** WebGL failed on this stream: the 2D canvas from now on. The Linux app starts there when
   *  WebKitGTK's compositing is off (linux_gpu.rs: older stacks, Safe graphics mode): WebKit then
   *  copies a WebGL canvas back from the GPU every frame, which made the stream lag badly. */
  private noGl = linuxApp();
  /** The HDR presenter made at the start, used from the first frame. */
  private hdr: HdrPresenter | null = null;
  private loggedHdrFrame = false;
  /** The canvas size and shape last logged (logLayout). */
  private laidOut = "";
  /** WebGL's context went away: how long it gets to come back before the 2D canvas. */
  private glLostTimer = 0;
  /** The picture tools stopped for good this session ("failed": no WebGL; "slow": the device
   *  lags). The picture is then drawn plain. */
  private passOff: "failed" | "slow" | "linux" | null = null;
  private slowFrames = 0;
  /** The Linux app's frames go to WebGL as their YUV planes (rpPicture.ts PlaneReader): handed
   *  over as frames, WebKitGTK converts each NV12 frame to RGB on the CPU in texImage2D (12 ms of
   *  the page's 16.7 at 1080p60 on an RTX 3080 PC: frames backed up and the rate fell to ~40).
   *  Null elsewhere, or after a copy failed (frames as frames from then on). */
  private planes: PlaneReader | null = isTauriEnv() && isLinuxDesktop() ? new PlaneReader() : null;
  /** A copy is under way; the newest frame that came meanwhile waits (older ones are dropped). */
  private planesBusy = false;
  private planesNext: VideoFrame | null = null;
  /** Copies in a row that came back empty (chromaBlank): a few and frames go as frames. */
  private planesBlank = 0;
  /** How the last frame reached WebGL, for the log: "frame" (texImage2D) or "<format>-planes". */
  private input = "";

  /** Upscaling: the picture's box on screen, in CSS pixels (times the pixel ratio of the
   *  window it is in, at each frame, is what it is upscaled to). */
  private boxCss: { w: number; h: number } | null = null;
  private boxWatch: ResizeObserver | null = null;

  /** Watch the picture's size on screen while upscaling is on (layout stays CSS: the box is
   *  sized by the picture's shape and its place, never by the canvas's pixel size). */
  private watchBox(on: boolean): void {
    if (!on) {
      this.boxWatch?.disconnect();
      this.boxWatch = null;
      this.boxCss = null;
      return;
    }
    if (this.boxWatch || typeof ResizeObserver === "undefined") return;
    // CSS size, not device-pixel-content-box: the ratio is read per frame instead, which also
    // follows the window to a screen with another scaling (no resize there).
    const ro = new ResizeObserver((entries) => {
      const r = entries[entries.length - 1].contentRect;
      // A minimized window's 0x0 is not the picture's size (the next real size replaces it).
      if (r.width > 0 && r.height > 0) this.boxCss = { w: r.width, h: r.height };
    });
    ro.observe(this.cv);
    this.boxWatch = ro;
  }

  /** The picture's box on screen in device pixels, or null when not known. */
  private boxPx(): { w: number; h: number } | null {
    const b = this.boxCss;
    if (!b) return null;
    const r = this.cv.ownerDocument.defaultView?.devicePixelRatio ?? 1;
    return { w: Math.round(b.w * r), h: Math.round(b.h * r) };
  }

  /** Set the picture tools; they apply from the next frame. */
  setPicture(p: PictureTools): void {
    this.tools = p;
    this.cv.style.imageRendering = p.pixelated ? "pixelated" : "";
    this.watchBox(upscales(p));
  }

  /** Why the picture tools are off this session, or null. */
  get pictureOff(): "failed" | "slow" | "linux" | null {
    return this.passOff;
  }

  private picturePassOff(why: "failed" | "slow" | "linux"): void {
    if (this.passOff) return;
    this.passOff = why;
    this.o.onPictureOff?.(why);
  }

  /** The drawer for the next frame, made on first use. */
  private pickDrawer(): NonNullable<RemotePlayStream["drawer"]> {
    if (linuxApp() && sharpens(this.tools)) this.picturePassOff("linux");
    if (this.hdr && !this.hdr.lost && this.hdr.canvas === this.cv) return (this.drawer = { kind: "hdr", hdr: this.hdr });
    if (!this.noGl) {
      try {
        const pass = new PicturePass(this.cv);
        this.watchGl(this.cv);
        return (this.drawer = { kind: "gl", pass });
      } catch (e) {
        log.warn("remote-play", `no WebGL 2 for the picture (${String(e)}): drawing with the 2D canvas`);
        this.noGl = true;
      }
    }
    let ctx = this.cv.getContext("2d", { alpha: false });
    if (!ctx) {
      // The canvas already holds another kind of context (WebGL made, then a shader failed).
      this.replaceCanvas();
      ctx = this.cv.getContext("2d", { alpha: false });
    }
    return (this.drawer = { kind: "2d", ctx });
  }

  /** WebGL's context can be lost (a driver reset, the GPU process restarting, a move between
   *  windows): wait a moment for it to come back, then go on with the 2D canvas. */
  private watchGl(c: HTMLCanvasElement): void {
    c.addEventListener("webglcontextlost", (e) => {
      // Ended: the context was given back on purpose (releasePicture).
      if (c !== this.cv || this.ended) return;
      e.preventDefault(); // so it can be restored
      log.warn("remote-play", "the picture's WebGL context was lost; waiting for it to come back");
      window.clearTimeout(this.glLostTimer);
      this.glLostTimer = window.setTimeout(() => {
        if (c !== this.cv || !(this.drawer?.kind === "gl" && this.drawer.pass.lost)) return;
        this.glFailed("its WebGL context did not come back");
      }, 3000);
    });
    c.addEventListener("webglcontextrestored", () => {
      if (c !== this.cv || this.noGl || this.ended) return;
      window.clearTimeout(this.glLostTimer);
      try {
        this.drawer = { kind: "gl", pass: new PicturePass(c) };
        log.info("remote-play", "the picture's WebGL context is back");
      } catch (e) {
        this.glFailed(String(e));
      }
    });
  }

  /** WebGL cannot draw the picture: the 2D canvas from now on, on a new canvas element. */
  private glFailed(why: string): void {
    log.warn("remote-play", `the picture could not be drawn with WebGL (${why}): drawing with the 2D canvas`);
    this.noGl = true;
    this.replaceCanvas();
  }

  /** The HDR display failed mid-stream: the session stays HDR, drawn by WebGL or 2D (the
   *  browser converts it to SDR); the next start asks for SDR (the store sets that). */
  private hdrFailed(why: string): void {
    log.warn("remote-play", `HDR could not be shown (${why}): showing the picture as SDR`);
    this.hdr?.dispose();
    this.hdr = null;
    this.replaceCanvas();
    this.o.onHdrOff?.(why, true);
  }

  /** A fresh canvas in the old one's place: a canvas keeps the first kind of context it gave,
   *  so changing how frames are drawn needs a new element. It takes over the old one's classes,
   *  fit, shape and size; the screens find it as `canvas` (picture-in-picture as a video keeps
   *  showing the old one until it is opened again). */
  private replaceCanvas(): void {
    const old = this.cv;
    // The old canvas's context goes now, not when the canvas is collected.
    if (this.drawer?.kind === "gl") this.drawer.pass.release();
    this.drawer = null;
    const c = document.createElement("canvas");
    c.className = old.className;
    for (const [k, v] of Object.entries(old.dataset)) if (v !== undefined) c.dataset[k] = v;
    c.style.cssText = old.style.cssText;
    c.width = old.width;
    c.height = old.height;
    this.cv = c;
    try {
      old.replaceWith(c);
    } catch {
      // Not on a page.
    }
    if (this.boxWatch) {
      this.watchBox(false);
      this.watchBox(upscales(this.tools));
    }
  }

  /** The instant replay window asked for now (it can change while the stream runs). */
  private replayS = 0;

  /** Instant replay on (seconds) or off (0), for the running session; also the next start's. */
  setReplay(seconds: number): void {
    if (this.replayS === seconds) return;
    this.replayS = seconds;
    const ws = this.ws;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ replay_s: seconds }));
  }

  /** One line in the logs each time the picture changes size: where the canvas is laid out
   *  against what it draws, so a report of a cut-off picture (#538, #575) says which. */
  private logLayout(streamW: number, streamH: number): void {
    try {
      const c = this.cv;
      const win = c.ownerDocument.defaultView;
      const r = c.getBoundingClientRect();
      const place = c.parentElement?.getBoundingClientRect();
      const n = (v: number | undefined) => (v === undefined ? "?" : v.toFixed(0));
      log.info(
        "remote-play",
        `picture: stream ${streamW}x${streamH}, canvas ${c.width}x${c.height} px, shown ${n(r.width)}x${n(r.height)} ` +
          `in ${n(place?.width)}x${n(place?.height)} css px, fit ${c.dataset.fit}, ratio ${win?.devicePixelRatio ?? "?"}, ` +
          `window ${win?.innerWidth ?? "?"}x${win?.innerHeight ?? "?"}${win !== window ? " (pop-out)" : ""}, ` +
          `drawn with ${this.drawer?.kind ?? "?"}${this.drawer?.kind === "gl" && this.input ? `, input ${this.input}` : ""}, tools ${JSON.stringify(this.tools)}${this.passOff ? ` (off: ${this.passOff})` : ""}, ` +
          `latency ${stagesLogText(this.stages.summary(performance.now()))}`,
      );
    } catch {
      // Only a log line.
    }
  }

  /** How the picture fills the place it is shown in (see PictureFit). */
  setFit(fit: PictureFit): void {
    const f: PictureFit = fit in FIT_CLASS ? fit : "fit";
    this.cv.className = canvasClass(f);
    this.cv.dataset.fit = f;
  }

  get host(): string {
    return this.o.host;
  }

  get currentPhase(): StreamPhase {
    return this.phase;
  }

  /** The sound's context and player. Made at the start, and when the picture comes back on. */
  private openSound(): void {
    try {
      // The console sends 48 kHz: a context at that rate never resamples 10 ms pieces.
      this.audioCtx = new AudioContext({ sampleRate: 48000, latencyHint: "interactive" });
    } catch {
      this.audioCtx = new AudioContext({ latencyHint: "interactive" });
    }
    void this.audioCtx.resume();
    this.watchSoundBlocked(this.audioCtx);
    this.gainNode = this.audioCtx.createGain();
    this.gainNode.gain.value = this.volume;
    this.gainNode.connect(this.audioCtx.destination);
    // The sound's player loads alongside: the session does not wait for it (the first moment's
    // sound, before it is ready, is left out).
    this.workletLoading = true;
    void this.openWorklet().finally(() => {
      this.workletLoading = false;
    });
  }

  /** Whether this device shows the stream (see `pictureOn`). */
  get pictureHere(): boolean {
    return this.pictureOn;
  }

  /** Turn the picture, sound and controller on this device off, or back on, for this session.
   *  The session and everyone else's stream go on; back on needs no restart (the engine sends
   *  the headers again and the next keyframe). */
  setPictureHere(on: boolean): void {
    if (on === this.pictureOn) return;
    this.pictureAsked = on;
    this.applyPicture(on);
    const ws = this.ws;
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ picture: on }));
      // An older engine does not know `picture` and goes on sending: the next frames need a key.
      if (on) ws.send(JSON.stringify({ keyframe: true }));
    }
  }

  private applyPicture(on: boolean): void {
    if (on === this.pictureOn) return;
    this.pictureOn = on;
    if (on) {
      this.reopenPicture();
    } else {
      this.dropPicture();
    }
    this.o.onPictureHere?.(on);
  }

  /** Everything that works for this device's picture and sound goes. The codec headers stay. */
  private dropPicture(): void {
    this.ticking = false;
    try {
      this.rafWin.cancelAnimationFrame(this.raf);
    } catch {
      // That window is gone.
    }
    window.clearTimeout(this.glLostTimer);
    const header = this.header;
    const rect = this.spsRect;
    this.resetMedia();
    this.header = header;
    this.spsRect = rect;
    this.closeAudio();
    this.audioHealth = { path: "scheduled", avgMs: 0, minMs: 0, corrMs: 0, concealed: 0, trimmedMs: 0 };
    this.audioMs = 0;
    this.releasePicture();
    // A canvas keeps the first context it gave, and this one's was just given back.
    this.replaceCanvas();
    this.laidOut = "";
    this.lastPad = "";
    for (const w of [...this.frameWaiters]) w();
  }

  /** The picture and sound start over; the next keyframe brings the picture. */
  private reopenPicture(): void {
    this.planes = isTauriEnv() && isLinuxDesktop() ? new PlaneReader() : null;
    this.openSound();
    this.ticking = true;
    this.schedule();
    this.openHdr();
    if (this.header && !this.video) this.openVideo();
    if (this.audioHeader && !this.audioWorker) this.pipe().header(this.audioHeader);
  }

  /** The picture came back on in an HDR session: its presenter again, on the new canvas. */
  private openHdr(): void {
    if (this.codecChoice !== "h265_hdr" || this.hdr) return;
    const cv = this.cv;
    // A frame drawn meanwhile would take the canvas with another kind of context.
    this.hdrOpening = true;
    void HdrPresenter.create(cv).then(
      (hdr) => {
        this.hdrOpening = false;
        if (this.ended || !this.pictureOn || this.cv !== cv) return hdr.dispose();
        this.hdr = hdr;
      },
      (e) => {
        this.hdrOpening = false;
        this.hdrFailed(String(e));
      },
    );
  }

  /** Start the session (call from the click that asks for it, so sound may play). */
  async start(): Promise<void> {
    const { width, height } = this.o;
    // The Linux app lays the picture out again when its window comes back (lib/repaintOnRestore.ts).
    if (isTauriEnv() && isLinuxDesktop()) {
      this.restoreOff = watchRestore(window, () => {
        if (this.ended) return;
        relayoutElement(this.cv);
        // The next frame logs where the picture is now.
        this.laidOut = "";
      });
    }
    if (this.pictureOn) this.openSound();
    const pref = this.o.codec ?? "auto";
    const [hevc, hdr] = await Promise.all([
      pref === "h264" ? Promise.resolve(false) : chooseCodec(width, height).then((c) => c === "h265"),
      pref === "hdr" ? hdrSupport(width, height) : Promise.resolve(null),
    ]);
    // Stopped while waiting: no socket, no console woken, nothing behind a page that has gone on.
    if (this.ended) return;
    let { codec, hdrOff } = pickCodec(pref, hevc, hdr);
    if (codec === "h265_hdr" && this.pictureOn) {
      // The HDR canvas is set up before the console is asked: if it cannot be, the session is
      // plain HEVC from the start.
      try {
        this.hdr = await HdrPresenter.create(this.cv);
      } catch (e) {
        codec = hevc ? "h265" : "h264";
        hdrOff = String(e);
      }
      if (this.ended) return;
    }
    if (hdrOff) {
      log.warn("remote-play", `HDR was asked for but is not used (${hdrOff}): streaming ${codec}`);
      this.o.onHdrOff?.(hdrOff, false);
    }
    if (codec === "h264" && !(await canDecodeAvc(width, height))) {
      // Found before the console is woken or asked to stream.
      if (this.ended) return;
      this.o.onError({ code: "no_decoder", detail: "" });
      this.finish();
      return;
    }
    if (this.ended) return;
    this.codecChoice = codec;
    this.hevc = codec !== "h264";
    this.connect(!!this.o.attach);
    this.ticking = this.pictureOn;
    this.schedule();
    this.watchHidden();
    let lastDecoded = 0;
    let lastBytes = 0;
    this.statsTimer = window.setInterval(() => {
      this.o.onStats({
        soundBlocked: this.soundBlocked(),
        soundUnavailable: this.soundUnavailable,
        fps: this.decoded - lastDecoded,
        mbps: ((this.bytes - lastBytes) * 8) / 1e6,
        decodeMs: this.decodeMs,
        codec: this.codec,
        hdr: this.codecChoice === "h265_hdr",
        audioMs: this.audioMs,
        underruns: this.underruns,
        audio: this.currentAudioHealth(),
        rttMs: this.rttMs,
        consoleRttMs: this.consoleRttMs,
        stages: this.stages.summary(performance.now()),
        decoderRestarts: this.decoderRestarts,
        ...this.lastLoss,
      });
      lastDecoded = this.decoded;
      lastBytes = this.bytes;
      this.ping();
      this.watchDecoder();
      if (this.decoded && Date.now() - this.lastLatencyLog > LATENCY_LOG_MS) {
        this.lastLatencyLog = Date.now();
        log.info("remote-play", `latency: ${this.linkLogText()}; ${this.audioLogLine()}`);
      }
    }, 1000);
  }

  /** A page in the background has no animation frames, so nothing would read the pad again: what
   *  was held when it went away would stay held on the console until it comes back. */
  private watchHidden(): void {
    const away = () => {
      if (document.visibilityState === "hidden") this.o.onHidden?.();
      this.sendPad();
    };
    document.addEventListener("visibilitychange", away);
    window.addEventListener("pagehide", away);
    this.hiddenOff = () => {
      document.removeEventListener("visibilitychange", away);
      window.removeEventListener("pagehide", away);
    };
  }

  /** Whether the browser holds the sound back (the context is not running). */
  soundBlocked(): boolean {
    const st = this.audioCtx?.state as string | undefined;
    return !!st && st !== "running" && st !== "closed";
  }

  /** Ask for the sound again: from a click or key press (which autoplay rules require). */
  resumeSound(): void {
    const actx = this.audioCtx;
    if (actx && this.soundBlocked()) void actx.resume().catch(() => {});
  }

  /** The browser may refuse to start the sound when the session did not start right after a
   *  click (a launch that first woke the console or started the game, a session rejoined after
   *  a reload), or stop it later (another app took the sound device, an incoming call on a
   *  phone). Then the next click or key press anywhere in the app starts it again, and the
   *  stream shows a "Click for sound" button meanwhile (StreamStats.soundBlocked). */
  private watchSoundBlocked(actx: AudioContext): void {
    const wake = () => {
      if (this.audioCtx !== actx || this.ended) return off();
      if (actx.state === "running") return off();
      void actx.resume().catch(() => {});
    };
    const events = ["pointerdown", "keydown", "touchend"] as const;
    let armed = false;
    const on = () => {
      if (armed) return;
      armed = true;
      for (const ev of events) window.addEventListener(ev, wake, { capture: true });
    };
    const off = () => {
      if (!armed) return;
      armed = false;
      for (const ev of events) window.removeEventListener(ev, wake, { capture: true });
    };
    actx.addEventListener("statechange", () => {
      if (this.audioCtx !== actx || actx.state === "closed") return off();
      if (actx.state === "running") off();
      else {
        log.info("remote-play", `sound: the browser holds it back (${actx.state}); the next click or key starts it`);
        on();
      }
    });
    // A context made outside a click starts suspended without a statechange.
    window.setTimeout(() => {
      if (this.audioCtx === actx && !this.ended && actx.state !== "running" && actx.state !== "closed") {
        log.info("remote-play", `sound: the browser holds it back (${actx.state}); the next click or key starts it`);
        on();
      }
    }, 0);
    this.soundUnlockOff = off;
  }

  /** The sound's player: the worklet's ring buffer (rpAudioWorklet.js), fed by a decoding
   *  worker where one can decode Opus, else by the page. No worklet: frames are scheduled one by
   *  one from the page. */
  private async openWorklet(): Promise<void> {
    const actx = this.audioCtx;
    if (!actx?.audioWorklet || !this.gainNode) {
      log.warn("remote-play", "sound: no audio worklet here: each frame is scheduled on the page");
      return;
    }
    let node: AudioWorkletNode;
    try {
      await actx.audioWorklet.addModule(audioWorkletUrl);
      if (this.ended || this.audioCtx !== actx || !this.gainNode) return;
      node = new AudioWorkletNode(actx, "rp-audio", { outputChannelCount: [2] });
      node.port.onmessage = (e: MessageEvent) => this.onWorkletStats(e.data);
      node.connect(this.gainNode);
    } catch (e) {
      log.warn("remote-play", `sound: the audio worklet did not load (${String(e)}): each frame is scheduled on the page`);
      return;
    }
    this.worklet = node;
    this.audioHealth.path = "page";
    this.setAudioBuffer(this.o.audioBufferMs ?? 60);
    // Not waited for: the session starts now, the page decodes until the worker is ready.
    void this.openAudioWorker(node);
  }

  /** Decode the sound in a worker that hands it straight to the worklet, where workers have an
   *  Opus AudioDecoder (Chromium, Safari 26); otherwise the page decodes it. */
  private async openAudioWorker(node: AudioWorkletNode): Promise<void> {
    if (typeof Worker === "undefined" || typeof MessageChannel === "undefined") return;
    let worker: Worker;
    try {
      worker = new Worker(new URL("./rpAudioWorker.ts", import.meta.url), { type: "module" });
    } catch (e) {
      log.info("remote-play", `sound: no decoding worker (${String(e)}): decoded on the page`);
      return;
    }
    const ok = await new Promise<boolean>((resolve) => {
      const t = window.setTimeout(() => resolve(false), 2000);
      worker.onmessage = (e: MessageEvent) => {
        if (e.data && typeof e.data === "object" && "ready" in e.data) {
          window.clearTimeout(t);
          resolve(!!e.data.ready);
        }
      };
      worker.onerror = () => {
        window.clearTimeout(t);
        resolve(false);
      };
      worker.postMessage({ probe: true });
    });
    if (!ok || this.ended || this.worklet !== node) {
      worker.terminate();
      if (!ok) log.info("remote-play", "sound: a worker here cannot decode Opus: decoded on the page");
      return;
    }
    const ch = new MessageChannel();
    worker.postMessage({ port: ch.port1 }, [ch.port1]);
    node.port.postMessage({ feed: ch.port2 }, [ch.port2]);
    // The stream may have started while the worker got ready: it takes over from the page's
    // decoder with the header the page already has.
    if (this.audioHeader) {
      const rec = new Uint8Array(this.audioHeader.length + 1);
      rec[0] = 3;
      rec.set(this.audioHeader, 1);
      worker.postMessage(rec.buffer, [rec.buffer]);
    }
    this.audioPipe?.reset();
    worker.onmessage = (e: MessageEvent) => {
      const said = e.data as { concealed?: unknown; failed?: unknown } | null;
      if (said?.failed) return this.audioWorkerFailed("its decoder failed");
      const c = said?.concealed;
      if (typeof c === "number") this.workerConcealed = c;
    };
    worker.onerror = (e) => this.audioWorkerFailed(e.message || "error");
    this.audioWorker = worker;
    this.audioHealth.path = "worker";
  }

  /** The decoding worker failed mid-session: the page decodes from here on. */
  private audioWorkerFailed(why: string): void {
    const w = this.audioWorker;
    if (!w) return;
    log.warn("remote-play", `sound: the decoding worker failed (${why}): decoded on the page from now on`);
    this.audioWorker = null;
    w.onmessage = null;
    w.onerror = null;
    w.terminate();
    this.worklet?.port.postMessage({ close: true });
    if (this.worklet) this.audioHealth.path = "page";
    if (this.audioHeader) this.pipe().header(this.audioHeader);
  }

  /** What the worklet says every ~64 ms (RpRing.stats). */
  private onWorkletStats(d: unknown): void {
    if (!d || typeof d !== "object") return;
    const s = d as Record<string, unknown>;
    const n = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
    this.audioMs = n(s.ms);
    this.underruns = n(s.under);
    const h = this.audioHealth;
    h.avgMs = n(s.avgMs);
    h.minMs = n(s.minMs);
    h.corrMs = n(s.corrMs);
    h.trimmedMs = n(s.trimmedMs);
  }

  private currentAudioHealth(): AudioHealth {
    return { ...this.audioHealth, concealed: this.workerConcealed + (this.audioPipe?.concealed ?? 0) };
  }

  private audioLogLine(): string {
    return audioLogText(this.currentAudioHealth(), this.underruns, this.audioTargetMs);
  }

  /** The page's own decoder for the sound. */
  private pipe(): OpusPipe {
    if (!this.audioPipe) {
      this.audioPipe = new OpusPipe(
        (planes, scale) => this.play(planes, scale),
        undefined,
        () => {
          this.soundUnavailable = true;
          log.warn("remote-play", "sound: this device can't decode the stream's Opus sound, so there is none");
        },
      );
    }
    return this.audioPipe;
  }

  /** The sound's parts go: worker, worklet, context (the worklet's port would otherwise keep
   *  this whole stream, its canvas and WebGL context alive: an AudioWorkletNode outlives its
   *  closed context while its port listens). */
  private closeAudio(): void {
    this.soundUnlockOff?.();
    this.soundUnlockOff = null;
    const w = this.audioWorker;
    this.audioWorker = null;
    if (w) {
      w.onmessage = null;
      w.onerror = null;
      w.terminate();
    }
    this.audioPipe?.reset();
    const node = this.worklet;
    this.worklet = null;
    if (node) {
      node.port.onmessage = null;
      try {
        node.port.postMessage({ close: true });
        node.port.close();
        node.disconnect();
      } catch {
        // Already gone.
      }
    }
    try {
      this.gainNode?.disconnect();
    } catch {
      // Already gone.
    }
    this.gainNode = null;
    const actx = this.audioCtx;
    this.audioCtx = null;
    if (actx && actx.state !== "closed") void actx.close().catch(() => {});
  }

  /** The picture's GPU parts go now, the WebGL context given back (not left to whenever the
   *  canvas is collected: a page that played many sessions kept every one's). */
  private releasePicture(): void {
    if (this.drawer?.kind === "gl") this.drawer.pass.release();
    this.drawer = null;
    this.hdr?.dispose();
    this.hdr = null;
    this.planes = null;
    // Its pixels too (a 2D canvas keeps them until it is collected; something on the page may
    // hold the ended stream a while: React keeps old callbacks, and with them old renders).
    try {
      this.cv.width = 0;
      this.cv.height = 0;
    } catch {
      // Not a canvas we can size.
    }
  }

  private setPhase(p: StreamPhase): void {
    this.phase = p;
    this.o.onPhase(p);
  }

  private connect(attachOnly: boolean): void {
    const { width, height } = this.o;
    const url = getEngineUrl().replace(/^http/, "ws") + "/api/ps5/rp/ws";
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    // The engine let go of this page's pad when its last socket left: what is held goes again.
    this.lastPad = "";
    if (!attachOnly) this.setPhase("connecting");
    ws.onopen = () => {
      this.socketOpened = true;
      ws.send(
        JSON.stringify({
          host: this.o.host,
          regist_key: this.o.registKey,
          rp_key: this.o.rpKey,
          width,
          height,
          fps: this.o.fps,
          codec: this.codecChoice,
          ...(this.o.bitrateKbps ? { bitrate_kbps: this.o.bitrateKbps } : {}),
          keep_alive_s: this.o.keepAliveS,
          attach_only: attachOnly,
          low_latency: !!this.o.lowLatency,
          ...(this.o.mic ? { mic: true } : {}),
          ...(this.o.dualSense ? { dualsense: true } : {}),
          ...(this.replayS ? { replay_s: this.replayS } : {}),
          // Video as kind 7, each frame with the engine's time (an older engine ignores this).
          timing: true,
          // Typing into the console's keyboard (lib/rpKeyboard.ts; an older engine ignores this).
          keyboard: true,
          // Off, or asked back on while the socket was away. An older engine ignores this and
          // goes on sending; the page drops it.
          ...(this.pictureAsked !== null ? { picture: this.pictureAsked } : {}),
        }),
      );
      // Attaching turns the console's mic off in the engine: a page that has it on says so.
      if (this.micOn) ws.send(JSON.stringify({ mic: true }));
    };
    ws.onmessage = (m) => {
      if (ws !== this.ws) return;
      if (typeof m.data === "string") return this.onStatus(m.data);
      const recv = performance.now();
      const b = new Uint8Array(m.data as ArrayBuffer);
      const kind = b[0];
      const payload = b.subarray(1);
      if (!this.pictureOn) {
        // Only what an older engine goes on sending, or what was on its way: the headers are
        // kept for when the picture comes back.
        if (kind === 0) {
          this.header = payload.slice();
          this.spsRect = spsVisibleRect(this.header, this.hevc);
        } else if (kind === 3) this.audioHeader = payload.slice();
        return;
      }
      if (kind <= 1) this.onVideo(kind, payload, recv, NaN);
      else if (kind === 7) {
        if (b.length < 9) return;
        const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
        // u64 µs as two halves (no BigInt in this bundle); exact up to 2^53.
        const us = dv.getUint32(1) * 4294967296 + dv.getUint32(5);
        const off = this.clock.offsetMs;
        this.onVideo(1, b.subarray(9), recv, off === null ? NaN : us / 1000 - off);
      }
      else if (kind === 6) this.o.dualSense?.haptics(payload);
      else if (kind === 3 || kind === 4) this.onAudio(kind, payload, m.data as ArrayBuffer);
    };
    // What a close means is decided in onclose; an error alone says nothing more.
    ws.onerror = () => {};
    ws.onclose = () => {
      if (ws !== this.ws) return;
      this.ws = null;
      if (this.stopping || this.engineEnded) return this.finish();
      if (this.moved) return this.setPhase("moved");
      // Never reached the engine: nothing was started, so there is nothing to rejoin.
      if (!this.socketOpened) return this.firstSocketFailed();
      this.reconnect();
    };
  }

  /** The first socket never opened: try the same start again, then say so. */
  private firstSocketFailed(): void {
    if (this.firstSocketTries >= FIRST_SOCKET_TRIES) {
      this.o.onError({ code: "socket", detail: "" });
      return this.finish();
    }
    this.firstSocketTries++;
    this.reconnectTimer = window.setTimeout(() => this.connect(!!this.o.attach), FIRST_SOCKET_WAIT_MS);
  }

  /** The socket dropped while the session runs: rejoin it, a little later each time. */
  private reconnect(): void {
    const now = Date.now();
    if (this.phase !== "reconnecting") {
      this.reconnectSince = now;
      this.reconnectTry = 0;
      this.setPhase("reconnecting");
    }
    if (now - this.reconnectSince > RECONNECT_FOR_MS) {
      this.o.onError({ code: "socket", detail: "" });
      return this.finish();
    }
    const wait = RECONNECT_WAITS_MS[Math.min(this.reconnectTry++, RECONNECT_WAITS_MS.length - 1)];
    this.reconnectTimer = window.setTimeout(() => {
      this.resetMedia();
      this.connect(true);
    }, wait);
  }

  private readonly tick = (): void => {
    if (this.outputPending) {
      // The browser shows what was drawn at this frame. A page that was hidden has no frames: a
      // wait that long is not drawing time.
      const ms = performance.now() - this.outputPending;
      if (ms < 500) this.stages.add("draw", ms, performance.now());
      this.outputPending = 0;
    }
    this.sendPad();
    this.schedule();
  };

  private schedule(): void {
    if (!this.ticking) return;
    const w = this.frameWin && !this.frameWin.closed ? this.frameWin : window;
    this.rafWin = w;
    this.raf = w.requestAnimationFrame(this.tick);
  }

  /** Poll the pad on `w`'s animation frames (a picture-in-picture window), or this page's (null). */
  setFrameWindow(w: Window | null): void {
    this.frameWin = w;
    if (!this.ticking) return;
    try {
      this.rafWin.cancelAnimationFrame(this.raf);
    } catch {
      // That window is gone.
    }
    this.schedule();
  }

  /** Show the stream here again after another window took it. */
  takeOver(): void {
    if (this.ended || this.ws) return;
    this.moved = false;
    this.resetMedia();
    this.setPhase("starting");
    this.connect(true);
  }

  /** A fresh start for the picture and sound: the engine sends the headers again on attach. */
  private resetMedia(): void {
    this.endDecoderStart();
    try {
      this.video?.close();
    } catch {
      // Already closed.
    }
    this.video = null;
    this.header = null;
    this.spsRect = null;
    this.sawKey = false;
    this.queued = [];
    this.slotTs.fill(-1);
    this.stages.clear();
    this.outputPending = 0;
    this.lastFedAt = 0;
    this.waitingSince = 0;
    this.rebuilds = 0;
    this.gaveUp = false;
    this.planesNext?.close();
    this.planesNext = null;
    this.audioPipe?.reset();
    this.audioWorker?.postMessage({ reset: true });
    this.worklet?.port.postMessage({ reset: true });
    this.audioNext = 0;
  }

  /** The latency log line: the stages, the round trips, the input estimate, and the console's
   *  own connection report (raw: its units are not documented). */
  private linkLogText(): string {
    const stages = this.stages.summary(performance.now());
    const ms = (v: number | null) => (v === null ? "?" : `${Math.round(v)} ms`);
    const input = inputEstimate({ pageRttMs: this.rttMs, consoleRttMs: this.consoleRttMs, stages });
    return (
      `${stagesLogText(stages)}; round trip to the engine ${ms(this.rttMs)}, engine to PS5 ${ms(this.consoleRttMs)}; ` +
      `input ≈ ${ms(input)} + the game; PS5 report ${this.quality ? JSON.stringify(this.quality) : "none yet"}`
    );
  }

  /** Time a round trip to the engine (answered with `{"pong": t}`). */
  private ping(): void {
    const ws = this.ws;
    if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ ping: performance.now() }));
  }

  /** The picture as it is now, a PNG at the stream's own size (null before the first frame). */
  snapshot(): Promise<Blob | null> {
    const c = this.cv;
    if (!this.decoded || !this.pictureOn) return Promise.resolve(null);
    return new Promise((resolve) => c.toBlob((b) => resolve(b), "image/png"));
  }

  /** The picture and the game's sound as one MediaStream, for a recording. The sound is tapped
   *  off before the volume (a muted player still records it) into a second output, so what
   *  plays is untouched. `release()` takes the tap off again. */
  captureMedia(fps: number): { stream: MediaStream; release: () => void } | null {
    if (!this.pictureOn) return null;
    const c = this.cv as HTMLCanvasElement & { captureStream?: (fps?: number) => MediaStream };
    if (typeof c.captureStream !== "function") return null;
    const stream = c.captureStream(fps);
    const letGo = this.holdPicture();
    const actx = this.audioCtx;
    const from: AudioNode | null = this.worklet ?? this.gainNode;
    let dest: MediaStreamAudioDestinationNode | null = null;
    try {
      if (actx && from && typeof actx.createMediaStreamDestination === "function") {
        dest = actx.createMediaStreamDestination();
        from.connect(dest);
        for (const t of dest.stream.getAudioTracks()) stream.addTrack(t);
      }
    } catch {
      dest = null; // a picture without sound rather than no recording
    }
    return {
      stream,
      release: () => {
        letGo();
        for (const t of stream.getTracks()) t.stop();
        try {
          if (dest) from?.disconnect(dest);
        } catch {
          // Already gone with the session.
        }
      },
    };
  }

  /** Send the pad now if it changed (call on key events so short taps are not missed). */
  sendPad(): void {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN || !this.pictureOn) return;
    const p = this.o.readPad();
    const s = JSON.stringify(p);
    if (s === this.lastPad) return;
    this.lastPad = s;
    ws.send(JSON.stringify({ pad: p }));
  }

  /** Native keyboard & mouse (lib/kbmInput.ts): `{"kbm": …}` or a binary `[8][report]`. False
   *  while the socket is down (the heartbeat sends it again). */
  sendKbm(msg: { kbm: unknown } | Uint8Array<ArrayBuffer>): boolean {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN || !this.pictureOn) return false;
    ws.send(msg instanceof Uint8Array ? msg : JSON.stringify(msg));
    return true;
  }

  /** Text for the console's on-screen keyboard, or done / cancel (lib/rpKeyboard.ts). */
  sendKeyboard(m: RpKeyboardSend): void {
    const ws = this.ws;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m));
  }

  /** The user's sign-in passcode, for the console that asked for it. Sent, never kept. */
  sendPasscode(digits: string): boolean {
    const ws = this.ws;
    if (!isPasscode(digits) || !ws || ws.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify({ passcode: digits }));
    return true;
  }

  /** The user would rather not enter the passcode: the start ends. */
  cancelPasscode(): void {
    const ws = this.ws;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ passcode_cancel: true }));
  }

  /** This session was started with the mic set up (Settings › Microphone). */
  get micEnabled(): boolean {
    return !!this.o.mic;
  }

  /** Turn the console's microphone on or off (experimental; see remotePlayMic.ts). */
  setMic(on: boolean): void {
    if (!this.o.mic) return;
    this.micOn = on;
    const ws = this.ws;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ mic: on }));
  }

  /** Rumble on the controllers like the console's own (not when vibration is off in Settings):
   *  a DualSense's haptics where they cannot be played (lib/dualSense.ts). */
  vibrate(strong: number, weak: number): void {
    if (!this.pictureOn || (this.o.rumble && !this.o.rumble())) return;
    playRumble(this.o.gamepads?.() ?? navigator.getGamepads?.() ?? [], strong, weak);
  }

  /** One `[5][Opus packet]` mic frame; dropped while the socket is down. */
  sendMicFrame(msg: Uint8Array<ArrayBuffer>): void {
    const ws = this.ws;
    if (this.micOn && ws && ws.readyState === WebSocket.OPEN) ws.send(msg);
  }

  /** 0-1; 0 is muted. */
  setVolume(v: number): void {
    this.volume = Math.max(0, Math.min(1, v));
    if (this.gainNode) this.gainNode.gain.value = this.volume;
  }

  setAudioBuffer(ms: number): void {
    this.audioTargetMs = Math.max(20, ms);
    this.worklet?.port.postMessage({ target: Math.round((this.audioTargetMs / 1000) * 48000) });
  }

  /** End the session; the engine reports it and closes the socket. */
  stop(): void {
    this.stopping = true;
    window.clearTimeout(this.reconnectTimer);
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      if (ws) ws.close();
      else this.finish();
      // A session nobody is attached to still has to be told.
      if (!ws || ws.readyState !== WebSocket.OPEN) stopEngineSession(this.o.host);
      return;
    }
    ws.send(JSON.stringify({ stop: true }));
    window.setTimeout(() => ws.close(), 3000);
  }

  private finish(): void {
    if (this.ended) return;
    this.ended = true;
    this.endDecoderStart();
    this.ticking = false;
    try {
      this.rafWin.cancelAnimationFrame(this.raf);
    } catch {
      // That window is gone.
    }
    window.clearInterval(this.statsTimer);
    window.clearTimeout(this.reconnectTimer);
    window.clearTimeout(this.glLostTimer);
    if (this.decoded) {
      log.info(
        "remote-play",
        `latency at the end: ${this.linkLogText()}; decoder restarts ${this.decoderRestarts}; ${this.audioLogLine()}`,
      );
    }
    this.watchBox(false);
    this.restoreOff?.();
    this.restoreOff = null;
    this.hiddenOff?.();
    this.hiddenOff = null;
    this.resetMedia();
    this.releasePicture();
    this.closeAudio();
    this.ws = null;
    this.setPhase("ended");
  }

  private onStatus(text: string): void {
    let s: {
      stage?: string;
      code?: string;
      retry_s?: number;
      error?: string;
      stats?: Record<string, unknown>;
      host?: string;
      rumble?: [number, number];
      light?: [number, number, number];
      pong?: unknown;
      engine_us?: unknown;
      triggers?: { left: number[]; right: number[] };
      ds_intensity?: [number, number];
      kbm?: unknown;
      pad_kbm?: unknown;
      keyboard?: RpKeyboardMsg;
      retry?: boolean;
      wait_s?: number;
      snapshot?: unknown;
      reason?: unknown;
      picture?: unknown;
    };
    try {
      s = JSON.parse(text);
    } catch {
      return;
    }
    if (s && typeof s === "object" && s.snapshot !== undefined) {
      void this.answerSnapshot(s.snapshot);
      return;
    }
    if (s && typeof s === "object" && "pong" in s) {
      const now = performance.now();
      this.rttMs = nextRtt(this.rttMs, s.pong, now);
      this.clock.push(s.pong, s.engine_us, now);
      return;
    }
    if (typeof s.picture === "boolean") {
      this.pictureAsked = s.picture;
      this.applyPicture(s.picture);
      return;
    }
    if (s.kbm !== undefined) {
      this.o.onKbm?.(s.kbm);
      return;
    }
    if (s.pad_kbm !== undefined) {
      this.o.onPadKbm?.(s.pad_kbm);
      return;
    }
    if (s.rumble) {
      if (!this.pictureOn || (this.o.rumble && !this.o.rumble())) return;
      playRumble(this.o.gamepads?.() ?? navigator.getGamepads?.() ?? [], s.rumble[0], s.rumble[1]);
      return;
    }
    if (s.light) {
      this.o.onLight?.(s.light);
      return;
    }
    if (s.triggers) {
      this.o.dualSense?.triggers(s.triggers.left, s.triggers.right);
      return;
    }
    if (s.ds_intensity) {
      this.o.dualSense?.intensity(s.ds_intensity[0], s.ds_intensity[1]);
      return;
    }
    if (s.keyboard) {
      this.o.onKeyboard?.(s.keyboard);
      return;
    }
    // Any later step means the console took the passcode (or the start ended): the prompt goes.
    if (this.passcodeAsked && s.stage && s.stage !== "passcode" && s.stage !== "stats") {
      this.passcodeAsked = false;
      this.o.onPasscode?.(null);
    }
    switch (s.stage) {
      case "waking":
        this.setPhase("waking");
        break;
      case "passcode":
        this.passcodeAsked = true;
        this.o.onPasscode?.({
          retry: s.retry === true,
          waitS: typeof s.wait_s === "number" && s.wait_s > 0 ? s.wait_s : 180,
        });
        break;
      case "nonce":
      case "session":
      case "takion":
      case "bang":
      case "streaminfo":
        this.setPhase("starting");
        break;
      case "first_frame":
        this.setPhase("playing");
        break;
      case "stats": {
        const st = s.stats ?? {};
        const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
        const units = num(st.units);
        this.lastLoss = {
          lossPct: units ? (num(st.units_lost) * 100) / units : 0,
          rebuilt: num(st.frames_rebuilt),
          lost: num(st.frames_lost),
        };
        this.consoleRttMs = typeof st.console_rtt_ms === "number" && Number.isFinite(st.console_rtt_ms) ? st.console_rtt_ms : null;
        if (st.quality && typeof st.quality === "object") this.quality = st.quality;
        break;
      }
      case "moved":
        this.moved = true;
        break;
      case "ended": {
        this.engineEnded = true;
        // The player's own Stop needs no word; anything else does, or the picture just goes.
        if (this.stopping) break;
        const why = typeof s.reason === "string" ? s.reason : "";
        if (why === "no session") this.o.onError({ code: "no_session", detail: "" });
        else if (why) this.o.onError({ code: "console_ended", detail: why });
        break;
      }
      case "error":
        this.engineEnded = true;
        this.o.onError({
          code: s.code ?? "other",
          retryS: s.retry_s ?? undefined,
          host: s.host,
          detail: s.error ?? "",
        });
        break;
    }
  }

  /** One video record: `recv` when it arrived, `engineAt` when the engine had it whole (this
   *  page's clock; NaN when unknown). */
  private onVideo(kind: number, payload: Uint8Array, recv: number, engineAt: number): void {
    if (kind === 0) {
      const prev = this.header;
      this.header = payload.slice();
      this.spsRect = spsVisibleRect(this.header, this.hevc);
      if (!this.video) this.openVideo();
      else if (!this.hevc && !sameBytes(prev, this.header)) {
        // H.264 decodes in AVC form, its parameter sets in the description: new ones need a
        // decoder configured with them.
        const old = this.video;
        this.video = null;
        try {
          old.close();
        } catch {
          // Already closed.
        }
        this.sawKey = false;
        this.openVideo();
        this.ws?.send(JSON.stringify({ keyframe: true }));
      }
      return;
    }
    this.bytes += payload.length;
    const key = isKeyframe(payload, this.hevc);
    // Behind: old pictures are no use to someone playing. Skip to a keyframe.
    const backlog = this.o.lowLatency ? VIDEO_BACKLOG_LOW_LATENCY : VIDEO_BACKLOG;
    if (!key && this.video && this.video.decodeQueueSize > backlog) {
      this.sawKey = false;
      this.ws?.send(JSON.stringify({ keyframe: true }));
    }
    if (!this.sawKey && !key) return;
    this.sawKey = true;
    let data = payload;
    if (!this.hevc) {
      // AVC form (length-prefixed, parameter sets in the description): the Linux app's
      // WebKitGTK decodes no H.264 given as Annex B ("Decode error", a black picture).
      data = lengthPrefixed(payload, false, true);
    } else if (key && this.header) {
      data = new Uint8Array(this.header.length + payload.length);
      data.set(this.header);
      data.set(payload, this.header.length);
    }
    this.ts += 16667;
    const slot = (this.ts / 16667) % FRAME_SLOTS;
    this.slotTs[slot] = this.ts;
    this.slotRecv[slot] = recv;
    this.slotEngine[slot] = engineAt;
    const chunk = new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: this.ts, data });
    const now = performance.now();
    // The wait for a picture starts at the first frame in, or again after a pause in frames (a
    // keyframe waited for, a hidden page): a gap in the input is not the decoder's.
    if (!this.waitingSince || now - this.lastFedAt > FED_RECENT_MS) this.waitingSince = now;
    this.lastFedAt = now;
    if (!this.video || this.video.state !== "configured") {
      // The HEVC codec string is still being probed: "queue" runs until it is handed in.
      this.slotSubmit[slot] = NaN;
      this.queued.push(chunk);
      // Never that long for a real probe. Start over from a later keyframe, not memory for ever.
      if (this.queued.length > MAX_QUEUED_FRAMES) {
        this.queued = [];
        this.sawKey = false;
      }
      return;
    }
    this.slotSubmit[slot] = now;
    this.video.decode(chunk);
  }

  /** The decoder stopped giving pictures while frames go in, or closed under the stream: a new
   *  one from the codec header, and a keyframe asked for (rpDecoderWatch.ts). A few tries, a
   *  little further apart each time; then the player is told, once, and it waits for a picture. */
  private watchDecoder(): void {
    // Nothing goes in while the picture is off here: that is the point, not a stall.
    if (this.gaveUp || !this.pictureOn) return;
    const verdict = decoderVerdict({
      now: performance.now(),
      haveHeader: !!this.header,
      state: this.video ? this.video.state : "none",
      lastFedAt: this.lastFedAt,
      waitingSince: this.waitingSince,
      rebuilds: this.rebuilds,
    });
    if (verdict === "ok") return;
    if (verdict === "give_up") {
      this.gaveUp = true;
      log.warn("remote-play", `the video decoder gave no picture after ${MAX_REBUILDS} restarts: waiting for the next keyframe`);
      this.o.onError({
        code: "decode",
        detail: `The video decoder stopped giving pictures and ${MAX_REBUILDS} restarts did not bring it back. Stop and start the stream, or set Settings › Video decoding to software.`,
      });
      return;
    }
    const old = this.video;
    const waited = Math.round(performance.now() - this.waitingSince);
    log.warn(
      "remote-play",
      `the video decoder ${old?.state === "closed" ? "closed" : `gave no picture for ${waited} ms`} while frames kept arriving ` +
        `(queue ${old?.decodeQueueSize ?? "?"}): restarting it (${this.rebuilds + 1} of ${MAX_REBUILDS})`,
    );
    this.rebuilds++;
    this.decoderRestarts++;
    this.video = null;
    try {
      old?.close();
    } catch {
      // Already closed.
    }
    this.sawKey = false;
    this.queued = [];
    this.waitingSince = 0;
    this.openVideo();
    this.ws?.send(JSON.stringify({ keyframe: true }));
  }

  /** Draw one decoded frame onto the canvas (it closes the frame). The canvas is the picture's
   *  own size (its visible part, at the display aspect), taken from each frame, never from the
   *  size asked for: the console may send another; upscaled, it is the size upscaled to. */
  private pictureUnseen(): boolean {
    return pictureUnseen({
      hidden: this.cv.ownerDocument?.visibilityState === "hidden",
      detached: this.cv.isConnected === false,
      waiting: this.frameWaiters.size > 0,
      holds: this.pictureHolds,
    });
  }

  /** Something reads this canvas with no one looking at it (a clip, a video picture in picture):
   *  frames keep being drawn until the returned function is called. */
  holdPicture(): () => void {
    this.pictureHolds++;
    let held = true;
    return () => {
      if (held) this.pictureHolds--;
      held = false;
    };
  }

  private drawFrame(frame: VideoFrame): void {
    if (this.hdrOpening || !this.pictureOn) {
      frame.close();
      return;
    }
    const fw = frame.displayWidth;
    const fh = frame.displayHeight;
    let out = { w: fw, h: fh };
    // The picture's own shape (before any upscale), which its layout follows.
    let shape = fw / fh;
    const d = this.drawer ?? this.pickDrawer();
    {
      if (d.kind === "hdr") {
        if (!this.loggedHdrFrame) {
          this.loggedHdrFrame = true;
          const cs = frame.colorSpace;
          const desc = `${cs?.primaries ?? "?"}/${cs?.transfer ?? "?"}/${cs?.matrix ?? "?"}, format ${frame.format ?? "GPU"}`;
          if (isHdrFrame(cs)) log.info("remote-play", `HDR picture: ${desc}`);
          else log.warn("remote-play", `HDR session, but the decoder says the picture is ${desc}: drawn as it is`);
        }
        try {
          out = d.hdr.render(frame, fw, fh, this.tools.crop219);
          shape = fw / out.h;
        } catch (e) {
          this.hdrFailed(String(e));
          frame.close();
          return;
        }
      } else if (d.kind === "gl") {
        // Lost: frames are dropped until it is back (watchGl).
        if (d.pass.lost) {
          frame.close();
          return;
        }
        if (this.planes && isPlaneFormat(frame.format)) return this.drawPlanes(frame);
        const tools = this.passOff ? DEFAULT_PICTURE : this.tools;
        const t0 = performance.now();
        try {
          const band = cropRect(fw, fh, tools.crop219).h;
          shape = fw / band;
          const to = upscales(tools) ? upscaleSize({ w: fw, h: band }, this.boxPx()) : null;
          out = d.pass.render(frame, fw, fh, tools, to);
          this.input = "frame";
        } catch (e) {
          if (d.pass.lost) {
            frame.close();
            return;
          }
          this.glFailed(String(e));
          return this.drawFrame(frame);
        }
        this.checkSlow(t0, tools);
      } else {
        // No WebGL: the picture as it is.
        if (needsPass(this.tools)) this.picturePassOff("failed");
        const c = this.cv;
        if (c.width !== fw) c.width = fw;
        if (c.height !== fh) c.height = fh;
        d.ctx?.drawImage(frame, 0, 0, fw, fh);
      }
    }
    this.drawn(fw, fh, out, shape);
    frame.close();
  }

  /** A device that cannot keep up gets the plain picture back, then the 2D canvas. */
  private checkSlow(t0: number, tools: PictureTools): void {
    this.slowFrames = performance.now() - t0 > 12 ? this.slowFrames + 1 : 0;
    if (this.slowFrames > 30) {
      this.slowFrames = 0;
      if (needsPass(tools)) this.picturePassOff("slow");
      else this.glFailed("too slow on this device");
    }
  }

  /** Draw a frame through WebGL as its YUV planes (closes it). The copy out is asynchronous:
   *  frames that come meanwhile wait, only the newest (a backlog only adds latency). */
  private drawPlanes(frame: VideoFrame): void {
    const reader = this.planes;
    if (!reader) return this.drawFrame(frame);
    if (this.planesBusy) {
      this.planesNext?.close();
      this.planesNext = frame;
      return;
    }
    this.planesBusy = true;
    const t0 = performance.now();
    const fw = frame.displayWidth;
    const fh = frame.displayHeight;
    const next = () => {
      this.planesBusy = false;
      const n = this.planesNext;
      this.planesNext = null;
      if (n) this.drawFrame(n);
    };
    reader
      .read(frame)
      .then((planes) => {
        const d = this.drawer;
        // The drawer changed meanwhile (WebGL lost or failed, the stream reset): dropped.
        if (!planes || d?.kind !== "gl" || d.pass.lost || this.planes !== reader) {
          frame.close();
          return;
        }
        if (chromaBlank(planes)) {
          if (++this.planesBlank >= 3) {
            // The copy gives nothing but zeros (a green picture): frames as frames from now on.
            log.warn("remote-play", `picture: the frame's ${planes.format} planes came back empty: drawing frames as they are`);
            this.planes = null;
            this.drawFrame(frame);
          } else frame.close();
          return;
        }
        this.planesBlank = 0;
        const tools = this.passOff ? DEFAULT_PICTURE : this.tools;
        const band = cropRect(fw, fh, tools.crop219).h;
        const to = upscales(tools) ? upscaleSize({ w: fw, h: band }, this.boxPx()) : null;
        let out: { w: number; h: number };
        try {
          out = d.pass.render(planes, fw, fh, tools, to);
        } catch (e) {
          frame.close();
          if (d.pass.lost) return;
          // WebGL itself failed: as with frames, the 2D canvas.
          this.glFailed(String(e));
          return;
        }
        this.input = `${planes.format.toLowerCase()}-planes`;
        this.checkSlow(t0, tools);
        this.drawn(fw, fh, out, fw / band);
        frame.close();
      })
      .catch((e) => {
        // The copy failed: frames go to WebGL as frames from now on.
        log.warn("remote-play", `picture: copying the frame's ${frame.format} planes failed (${String(e)}): drawing frames as they are`);
        this.planes = null;
        this.drawFrame(frame);
      })
      .finally(next);
  }

  /** A local automation client asked the engine for the picture (rpSnapshot.ts): answer on
   *  the socket it came on, with what the canvas shows now. Nothing on screen changes. */
  private async answerSnapshot(v: unknown): Promise<void> {
    const req = parseSnapshotRequest(v);
    const ws = this.ws;
    if (!req || !ws) return;
    const reply = await takeSnapshot(req, {
      canvas: this.cv,
      frameSeq: () => this.frameSeq,
      frameAt: () => this.frameAt,
      waitForFrame: (seq, ms) => this.waitForFrame(seq, ms),
    });
    // A socket replaced meanwhile belongs to another attach: the engine no longer waits on it.
    if (ws === this.ws && ws.readyState === WebSocket.OPEN) ws.send(reply);
  }

  /** True once a picture after `seq` is drawn; false after `ms`. */
  private waitForFrame(seq: number, ms: number): Promise<boolean> {
    if (this.frameSeq > seq) return Promise.resolve(true);
    return new Promise((resolve) => {
      const done = () => {
        if (this.frameSeq <= seq) return;
        window.clearTimeout(t);
        this.frameWaiters.delete(done);
        resolve(true);
      };
      // The wait comes from a request (rpSnapshot): never longer than 10 s.
      const t = window.setTimeout(
        () => {
          this.frameWaiters.delete(done);
          resolve(false);
        },
        Math.min(Math.max(0, ms), 10_000),
      );
      this.frameWaiters.add(done);
    });
  }

  /** After a frame is drawn: the canvas's shape and the layout log line follow the picture. */
  private drawn(fw: number, fh: number, out: { w: number; h: number }, shape: number): void {
    this.frameSeq++;
    this.frameAt = Date.now();
    if (this.frameWaiters.size) for (const w of [...this.frameWaiters]) w();
    // The shape on its own too: upscaled, the canvas keeps the box's size while Crop to 21:9
    // changes the shape.
    const laid = `${out.w}x${out.h}:${shape}`;
    if (laid !== this.laidOut) {
      this.laidOut = laid;
      if (shape !== this.shape) {
        this.cv.style.setProperty("--rp-shape", String(shape));
        this.shape = shape;
      }
      this.logLayout(fw, fh);
    }
  }

  private endDecoderStart(): void {
    this.decoderStart?.end();
    this.decoderStart = null;
  }

  private openVideo(): void {
    // A new decoder: its wait for a picture starts with its first frame.
    this.waitingSince = 0;
    // Linux: the GL display WebKitGTK needs first, and the crash marker until the first picture.
    this.endDecoderStart();
    const start = decoderStarting();
    this.decoderStart = start;
    const video = new VideoDecoder({
      output: (decoded) => {
        const now = performance.now();
        const slot = (decoded.timestamp / 16667) % FRAME_SLOTS;
        if (this.slotTs[slot] === decoded.timestamp) {
          this.slotTs[slot] = -1;
          const recv = this.slotRecv[slot];
          const submit = this.slotSubmit[slot];
          const engineAt = this.slotEngine[slot];
          if (Number.isFinite(submit)) {
            this.decodeMs = now - submit;
            this.stages.add("decode", now - submit, now);
            this.stages.add("queue", submit - recv, now);
          }
          if (Number.isFinite(engineAt)) this.stages.add("net", recv - engineAt, now);
        }
        // A picture: the watchdog starts over, and the decoder is made.
        if (this.decoderStart === start) this.endDecoderStart();
        this.waitingSince = 0;
        this.rebuilds = 0;
        this.gaveUp = false;
        const frame = this.withRealVisibleRect(decoded);
        // Decoding has to go on (the next frames build on this one); showing it does not.
        if (this.pictureUnseen()) frame.close();
        else this.drawFrame(frame);
        this.outputPending = now;
        this.decoded++;
      },
      error: (e) => {
        if (video !== this.video) return;
        if (this.decoderStart === start) this.endDecoderStart();
        if (isNoDecoderError(e)) {
          // Starting over would fail the same way, forever: end the session instead.
          this.video = null;
          this.o.onError({ code: "no_decoder", detail: String(e) });
          this.stop();
          return;
        }
        // A broken picture: start over from the next keyframe rather than give up.
        this.video = null;
        this.sawKey = false;
        if (this.decoded === 0 && !this.nextAfterNoPicture(String(e))) return;
        if (this.header) this.openVideo();
        this.ws?.send(JSON.stringify({ keyframe: true }));
      },
    });
    this.video = video;
    const configure = async (codec: string, description?: Uint8Array) => {
      // The hardware decoder when there is one. "prefer-hardware" is a demand, not a wish: where
      // the device has none for this codec, configure() fails and no picture ever comes.
      // Software when asked (Settings › Video decoding) or after the hardware one gave a picture
      // that could not be put right.
      // The Linux app decoding in software this launch (Settings › Window › Graphics) too.
      const software = this.o.decoding === "software" || this.softwareAfterBadFrame || linuxSoftwareVideo();
      const base: VideoDecoderConfig = description ? { codec, description, optimizeForLatency: true } : { codec, optimizeForLatency: true };
      let cfg: VideoDecoderConfig = {
        ...base,
        hardwareAcceleration: software ? "prefer-software" : "prefer-hardware",
      };
      await start.ready;
      if (video !== this.video) return;
      try {
        if (!(await VideoDecoder.isConfigSupported(cfg)).supported) cfg = base;
      } catch {
        cfg = base;
      }
      if (video !== this.video) return;
      this.codec = codec;
      try {
        video.configure(cfg);
      } catch (e) {
        this.video = null;
        this.o.onError({ code: isNoDecoderError(e) ? "no_decoder" : "decode", detail: String(e) });
        this.stop();
        return;
      }
      const now = performance.now();
      for (const q of this.queued) {
        const slot = (q.timestamp / 16667) % FRAME_SLOTS;
        if (this.slotTs[slot] === q.timestamp) this.slotSubmit[slot] = now;
        video.decode(q);
      }
      this.queued = [];
    };
    if (this.codecChoice === "h265_hdr") {
      // Main10: the SPS's own codec string where the device takes it, else the usual ones.
      const own = this.header ? hevcCodecString(this.header) : null;
      void (async () => {
        await start.ready;
        let c: string | null = null;
        try {
          if (own && (await VideoDecoder.isConfigSupported({ codec: own })).supported) c = own;
        } catch {
          // Not this one.
        }
        c ??= await supportedHevcMain10(this.o.width, this.o.height);
        if (c) void configure(c);
        else {
          this.o.onHdrOff?.("no HEVC Main10 decoder", true);
          this.cannotDecode("This device cannot decode the HDR stream (HEVC Main10).");
        }
      })();
    } else if (this.hevc) {
      void supportedHevc(this.o.width, this.o.height).then((c) => {
        if (c) void configure(c);
        else this.cannotDecode("This device cannot decode HEVC.");
      });
    } else if (this.header) {
      const description = avcDescription(this.header);
      if (description) void configure(avcCodecString(this.header), description);
      else this.cannotDecode("The stream's H.264 header has no SPS/PPS.");
    }
  }

  /** A decoder failed with no picture yet. Starting the same one over forever only repeats the
   *  error (#683), so a few tries lead to the next setup. False when the session ended. */
  private nextAfterNoPicture(detail: string): boolean {
    this.noPictureFails++;
    const software = this.o.decoding === "software" || this.softwareAfterBadFrame || linuxSoftwareVideo();
    const step = noPictureStep(this.noPictureFails, { hevc: this.hevc, software });
    if (step === "retry") return true;
    if (step === "software") {
      log.warn("remote-play", `no picture from the hardware decoder (${detail}): decoding in software`);
      this.softwareAfterBadFrame = true;
      this.noPictureFails = 0;
      return true;
    }
    if (step === "h264") {
      log.warn("remote-play", `no picture from HEVC (${detail}): ending the stream, the next start asks for H.264`);
      this.o.onHevcFailed?.();
      this.stop();
      return false;
    }
    log.warn("remote-play", `no picture with any decoder (${detail})`);
    this.cannotDecode(detail);
    return false;
  }

  /** No decoder will ever take this stream: said, and the session ends rather than go on streaming
   *  to nothing (with its frames piling up). */
  private cannotDecode(detail: string): void {
    this.o.onError({ code: "decode", detail });
    this.stop();
  }

  /** An audio record (3 header, 4 frame): to the decoding worker as it is (the socket's buffer
   *  handed over, not copied), or decoded here. */
  private onAudio(kind: number, payload: Uint8Array, whole: ArrayBuffer): void {
    if (!this.audioCtx) return;
    if (kind === 3) this.audioHeader = payload.slice();
    const w = this.audioWorker;
    if (w) {
      w.postMessage(whole, [whole]);
      return;
    }
    if (kind === 3) this.pipe().header(payload);
    else this.pipe().packet(payload);
  }

  /** Decoded sound (the arrays are this call's to keep or hand over). */
  private play(planes: Float32Array[], scale: number): void {
    if (this.workletLoading) return;
    if (this.worklet) {
      this.worklet.port.postMessage({ planes, scale }, planes.map((p) => p.buffer));
      return;
    }
    // No worklet: each frame scheduled back to back ~60 ms ahead.
    const actx = this.audioCtx;
    if (!actx || !this.gainNode) return;
    const rate = this.audioPipe?.sampleRate ?? 48000;
    const n = planes[0].length;
    const buf = actx.createBuffer(planes.length, n, rate);
    planes.forEach((p, c) => buf.copyToChannel(scale === 1 ? (p as Float32Array<ArrayBuffer>) : p.map((v) => v * scale), c));
    const now = actx.currentTime;
    if (this.audioNext < now + 0.01) {
      // Ran dry (the page was busy, or the start): a gap, then 60 ms ahead again.
      if (this.audioNext > 0) this.underruns++;
      this.audioNext = now + this.audioTargetMs / 1000;
    }
    if (this.audioNext > now + 0.25) return; // already far enough ahead: drop rather than pile up
    const src = actx.createBufferSource();
    src.buffer = buf;
    src.connect(this.gainNode);
    src.start(this.audioNext);
    this.audioNext += n / rate;
    this.audioMs = (this.audioNext - now) * 1000;
  }
}

/** The desktop app on Linux with WebKitGTK's compositing off, where the picture is drawn with the
 *  2D canvas. */
function linuxApp(): boolean {
  return isTauriEnv() && isLinuxDesktop() && linuxCompositingOff();
}
