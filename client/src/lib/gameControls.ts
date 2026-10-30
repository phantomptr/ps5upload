// Per-game controls for Remote Play: turbo buttons, mouse aim and macros, kept per title on this
// device and applied to the pad as it is read for every send (state/remotePlay.ts readPad). The
// console only ever sees an ordinary controller.

import { PadButton, type PadState } from "./remotePlayStream";

/** One step of a macro: these buttons held this long (0 buttons = a pause). */
export interface MacroStep {
  buttons: number;
  ms: number;
}

export interface Macro {
  /** The keyboard key (KeyboardEvent.code) that plays it. */
  key: string;
  steps: MacroStep[];
}

export interface GameControls {
  /** Buttons that repeat while held (a PadButton mask). */
  turbo: number;
  /** Presses a second while a turbo button is held. */
  turboHz: number;
  /** The mouse moves the right stick while the pointer is captured; left click R2, right L2. */
  mouse: boolean;
  /** How far the stick goes for the mouse's movement (1 = default). */
  mouseSens: number;
  mouseInvertY: boolean;
  macros: Macro[];
}

export const DEFAULT_CONTROLS: GameControls = {
  turbo: 0,
  turboHz: 10,
  mouse: false,
  mouseSens: 1,
  mouseInvertY: false,
  macros: [],
};

/** The buttons turbo and macros offer (sticks and the PS button are left out). */
export const TURBO_BUTTONS: Array<{ bit: number; name: string }> = [
  { bit: PadButton.CROSS, name: "✕" },
  { bit: PadButton.CIRCLE, name: "○" },
  { bit: PadButton.SQUARE, name: "□" },
  { bit: PadButton.TRIANGLE, name: "△" },
  { bit: PadButton.L1, name: "L1" },
  { bit: PadButton.R1, name: "R1" },
  { bit: PadButton.UP, name: "↑" },
  { bit: PadButton.DOWN, name: "↓" },
  { bit: PadButton.LEFT, name: "←" },
  { bit: PadButton.RIGHT, name: "→" },
];

/** L2/R2 are analog triggers, not bits: macros and turbo name them with these pseudo-bits. */
export const L2_BIT = 1 << 20;
export const R2_BIT = 1 << 21;

const STICK = 32767;
/** Mouse speed (counts per frame at sensitivity 1) that moves the stick all the way. */
const MOUSE_FULL = 40;
/** How fast the mouse's push on the stick fades once it stops (per second, as a time constant). */
const MOUSE_DECAY_S = 0.06;

/** What the mouse is doing (moved by the stream screen while the pointer is captured). */
export interface MouseAim {
  vx: number;
  vy: number;
  /** When vx/vy were last decayed. */
  t: number;
  left: boolean;
  right: boolean;
}

export function newMouseAim(): MouseAim {
  return { vx: 0, vy: 0, t: 0, left: false, right: false };
}

/** The mouse moved (movementX/Y while the pointer is locked). */
export function mouseMoved(m: MouseAim, dx: number, dy: number): void {
  m.vx += dx;
  m.vy += dy;
}

/** A macro being played: which, and since when. */
export interface MacroRun {
  index: number;
  since: number;
}

/** What the controls keep between reads. */
export interface ControlsState {
  mouse: MouseAim;
  macro: MacroRun | null;
  /** The keys held at the last read (a macro starts on its key's press, not while held). */
  lastKeys: Set<string>;
}

export function newControlsState(): ControlsState {
  return { mouse: newMouseAim(), macro: null, lastKeys: new Set() };
}

const clampStick = (v: number) => Math.max(-STICK, Math.min(STICK, Math.round(v)));

/** The buttons (and L2/R2 pseudo-bits) a macro holds at `ms` into it, or null when it is over. */
export function macroAt(m: Macro, ms: number): number | null {
  let t = 0;
  for (const s of m.steps) {
    if (ms < t + s.ms) return s.buttons;
    t += s.ms;
  }
  return null;
}

/**
 * The pad as the console gets it, from the pad as the player holds it (`pad`, merged from the
 * controller, keys and touch), the keys held, and this game's controls. `st` carries the
 * mouse and a playing macro between reads.
 */
export function applyControls(
  pad: PadState,
  c: GameControls | null,
  keys: ReadonlySet<string>,
  st: ControlsState,
  now: number,
): PadState {
  if (!c) {
    st.lastKeys = new Set(keys);
    return pad;
  }
  const out: PadState = { ...pad };

  // Turbo: a held turbo button is let go every other half-period.
  if (c.turbo && c.turboHz > 0) {
    const off = Math.floor((now / 1000) * c.turboHz * 2) % 2 === 1;
    if (off) {
      out.buttons &= ~(c.turbo & ~(L2_BIT | R2_BIT));
      if (c.turbo & L2_BIT) out.l2 = 0;
      if (c.turbo & R2_BIT) out.r2 = 0;
    }
  }

  // Mouse aim: its speed pushes the right stick and fades when the mouse stops.
  if (c.mouse) {
    const m = st.mouse;
    const dt = m.t ? Math.max(0, now - m.t) / 1000 : 0;
    m.t = now;
    // Fade first: a mouse that stopped since the last read pushes the stick less now.
    const decay = Math.exp(-dt / MOUSE_DECAY_S);
    m.vx *= decay;
    m.vy *= decay;
    const k = c.mouseSens / MOUSE_FULL;
    const rx = clampStick(m.vx * k * STICK);
    const ry = clampStick((c.mouseInvertY ? -m.vy : m.vy) * k * STICK);
    if (Math.abs(rx) > Math.abs(out.rx)) out.rx = rx;
    if (Math.abs(ry) > Math.abs(out.ry)) out.ry = ry;
    if (m.left) out.r2 = 255;
    if (m.right) out.l2 = 255;
  }

  // Macros: a macro key's press plays it; the player pressing any button it uses stops it.
  const pressed = [...keys].filter((k) => !st.lastKeys.has(k));
  st.lastKeys = new Set(keys);
  const start = c.macros.findIndex((m) => pressed.includes(m.key));
  if (start >= 0) st.macro = { index: start, since: now };
  if (st.macro) {
    const m = c.macros[st.macro.index];
    const held = m ? macroAt(m, now - st.macro.since) : null;
    if (held === null || !m) {
      st.macro = null;
    } else {
      const used = m.steps.reduce((a, s) => a | s.buttons, 0) & ~(L2_BIT | R2_BIT);
      if (pad.buttons & used) {
        st.macro = null; // the player took over
      } else {
        out.buttons |= held & ~(L2_BIT | R2_BIT);
        if (held & L2_BIT) out.l2 = 255;
        if (held & R2_BIT) out.r2 = 255;
      }
    }
  }
  return out;
}

/** A stored profile, checked field by field (anything odd falls back to the default). */
export function readControls(v: unknown): GameControls {
  const o = (v ?? {}) as Record<string, unknown>;
  const num = (x: unknown, d: number, lo: number, hi: number) =>
    typeof x === "number" && Number.isFinite(x) ? Math.min(hi, Math.max(lo, x)) : d;
  const macros = Array.isArray(o.macros)
    ? (o.macros as unknown[])
        .map((m) => (m ?? {}) as Record<string, unknown>)
        .filter((m) => typeof m.key === "string" && m.key && Array.isArray(m.steps))
        .map((m) => ({
          key: m.key as string,
          steps: (m.steps as unknown[])
            .map((s) => (s ?? {}) as Record<string, unknown>)
            .map((s) => ({ buttons: num(s.buttons, 0, 0, 0x7fffffff) | 0, ms: num(s.ms, 100, 16, 10_000) })),
        }))
        .slice(0, 12)
    : [];
  return {
    turbo: num(o.turbo, 0, 0, 0x7fffffff) | 0,
    turboHz: num(o.turboHz, DEFAULT_CONTROLS.turboHz, 2, 30),
    mouse: o.mouse === true,
    mouseSens: num(o.mouseSens, 1, 0.1, 5),
    mouseInvertY: o.mouseInvertY === true,
    macros,
  };
}
