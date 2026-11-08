// Controller profiles: how each controller's buttons and sticks become the PS5's pad.
//
// Phone controllers (Backbone, Razer Kishi, GameSir, Scuf Nomad, ELO Vagabond...) and desktop
// pads all reach the page through the Gamepad API. Most report the browser's "standard" layout;
// one that does not still plays with the standard order as a guess, and any controller can be
// mapped by pressing its buttons in turn ("press ✕"), saved per controller. Extra buttons (back
// paddles) can be given any PS5 button the same way.

import { knownProfile, type PadKind } from "./padMappings";
import { safeGetItem, safeSetItem } from "./safeStorage";
import {
  guessProfile,
  hatPosition,
  PadButton,
  STANDARD_PROFILE,
  type PadAxis,
  type PadProfile,
  type PadSource,
} from "./remotePlayStream";

const KEY = "ps5upload.rp_pad_profiles.v1";

function isPadSource(v: unknown): boolean {
  return v === null || (typeof v === "object" && !Array.isArray(v));
}

function isPadProfile(v: unknown): v is PadProfile {
  if (!v || typeof v !== "object") return false;
  const p = v as Partial<Record<keyof PadProfile, unknown>>;
  return (
    Array.isArray(p.buttons) &&
    p.buttons.every((b) => Array.isArray(b) && b.length === 2 && typeof b[0] === "number" && isPadSource(b[1])) &&
    [p.l2, p.r2, p.lx, p.ly, p.rx, p.ry].every(isPadSource)
  );
}

/** Saved profiles by the controller's id (what the browser calls it). */
export function loadProfiles(): Record<string, PadProfile> {
  try {
    const raw = safeGetItem(KEY);
    const v = raw ? (JSON.parse(raw) as unknown) : null;
    if (!v || typeof v !== "object" || Array.isArray(v)) return {};
    // A damaged entry is dropped: its controller plays with the default until mapped again.
    return Object.fromEntries(
      Object.entries(v).filter((entry): entry is [string, PadProfile] => isPadProfile(entry[1])),
    );
  } catch {
    return {};
  }
}

export function saveProfile(id: string, prof: PadProfile | null): Record<string, PadProfile> {
  const all = loadProfiles();
  if (prof) all[id] = prof;
  else delete all[id];
  safeSetItem(KEY, JSON.stringify(all));
  cache = all;
  return all;
}

let cache: Record<string, PadProfile> | null = null;

/** The profile `g` plays with: its saved one, else the browser's standard layout, the known
 *  controllers' table (padMappings.ts) or the guess. */
export function profileFor(g: Gamepad): PadProfile {
  cache ??= loadProfiles();
  return cache[g.id] ?? migratedProfile(cache, g.id) ?? knownProfile(g) ?? guessProfile(g);
}

/** How `g` plays now (see PadKind). */
export function padKind(g: Gamepad): PadKind {
  cache ??= loadProfiles();
  if (cache[g.id]) return "saved";
  if (g.mapping === "standard") return "standard";
  return knownProfile(g) ? "known" : "guess";
}

/** What `g` would play with without a saved mapping: the base a check fills its skipped steps
 *  from. */
export function defaultProfileFor(g: Gamepad): PadProfile {
  return knownProfile(g) ?? guessProfile(g);
}

/** A forwarded controller is named `<name> (app)` since #668. A mapping saved under the name the
 *  browser gave it applies still, and is copied to the new name on first use; the old entry stays,
 *  for a browser or an older version that still reads it. */
function migratedProfile(all: Record<string, PadProfile>, id: string): PadProfile | null {
  if (!id.endsWith(" (app)")) return null;
  const name = id.slice(0, -" (app)".length).trim().toLowerCase();
  if (!name) return null;
  const same = Object.keys(all).filter((k) => {
    const key = k.trim().toLowerCase();
    return key === name || key.includes(name);
  });
  const exact = same.find((k) => k.trim().toLowerCase() === name);
  const from = exact ?? (same.length === 1 ? same[0] : null);
  if (!from) return null;
  cache = saveProfile(id, all[from]);
  return all[from];
}

/** A controller as one moment of its buttons and axes. */
export interface PadSnapshot {
  buttons: number[];
  axes: number[];
}

export function snapshot(g: Gamepad): PadSnapshot {
  return { buttons: g.buttons.map((b) => Math.max(b.pressed ? 1 : 0, b.value)), axes: [...g.axes] };
}

/** The button or half-axis that moved from `rest` to `now` the most, past a clear press, or null.
 *  Axes take a bigger move to count, so a stick nudged while pressing a button is not picked. */
export function detectPress(rest: PadSnapshot, now: PadSnapshot): PadSource | null {
  let best: { src: PadSource; amount: number } | null = null;
  now.buttons.forEach((v, i) => {
    const moved = v - (rest.buttons[i] ?? 0);
    if (v > 0.5 && moved > 0.5 && (!best || moved > best.amount)) best = { src: { b: i }, amount: moved };
  });
  if (best) return (best as { src: PadSource }).src;
  // A d-pad on one axis: resting outside -1..1, each direction a value of its own. Taken as half an
  // axis, up, right and down would all be one source, and left too small a move to count.
  for (let i = 0; i < now.axes.length; i++) {
    const at = Math.abs(rest.axes[i] ?? 0) > 1.05 ? hatPosition(now.axes[i]) : null;
    if (at !== null) return { h: i, at };
  }
  now.axes.forEach((v, i) => {
    const r = rest.axes[i] ?? 0;
    const moved = v - r;
    if (Math.abs(moved) < 0.6) return;
    if (!best || Math.abs(moved) > best.amount) {
      const d: 1 | -1 = moved > 0 ? 1 : -1;
      // A trigger on an axis rests at one end (-1 or 1): keep where, so half way reads half.
      const src: PadSource = Math.abs(r) > 0.5 ? { a: i, d, rest: r } : { a: i, d };
      best = { src, amount: Math.abs(moved) };
    }
  });
  return best ? (best as { src: PadSource }).src : null;
}

/** The axis a stick was pushed along ("push it right" / "push it down"), or null. */
export function detectAxis(rest: PadSnapshot, now: PadSnapshot): PadAxis | null {
  let best: { a: number; moved: number } | null = null;
  now.axes.forEach((v, i) => {
    const moved = v - (rest.axes[i] ?? 0);
    if (Math.abs(moved) >= 0.6 && (!best || Math.abs(moved) > Math.abs(best.moved))) best = { a: i, moved };
  });
  if (!best) return null;
  const b = best as { a: number; moved: number };
  return b.moved > 0 ? { a: b.a } : { a: b.a, inv: true };
}

/** One step of mapping a controller: a button, a trigger, or a stick direction. */
export type MapStep =
  | { kind: "button"; bit: number; name: string }
  | { kind: "trigger"; which: "l2" | "r2"; name: string }
  | { kind: "axis"; which: "lx" | "ly" | "rx" | "ry"; name: string };

/** The steps in order, named the way the console's controller labels them. */
export const MAP_STEPS: MapStep[] = [
  { kind: "button", bit: PadButton.CROSS, name: "✕" },
  { kind: "button", bit: PadButton.CIRCLE, name: "○" },
  { kind: "button", bit: PadButton.SQUARE, name: "□" },
  { kind: "button", bit: PadButton.TRIANGLE, name: "△" },
  { kind: "button", bit: PadButton.UP, name: "↑" },
  { kind: "button", bit: PadButton.DOWN, name: "↓" },
  { kind: "button", bit: PadButton.LEFT, name: "←" },
  { kind: "button", bit: PadButton.RIGHT, name: "→" },
  { kind: "button", bit: PadButton.L1, name: "L1" },
  { kind: "button", bit: PadButton.R1, name: "R1" },
  { kind: "trigger", which: "l2", name: "L2" },
  { kind: "trigger", which: "r2", name: "R2" },
  { kind: "button", bit: PadButton.L3, name: "L3" },
  { kind: "button", bit: PadButton.R3, name: "R3" },
  { kind: "button", bit: PadButton.OPTIONS, name: "Options" },
  { kind: "button", bit: PadButton.SHARE, name: "Create" },
  { kind: "button", bit: PadButton.PS, name: "PS" },
  { kind: "button", bit: PadButton.TOUCHPAD, name: "Touchpad" },
  { kind: "axis", which: "lx", name: "L →" },
  { kind: "axis", which: "ly", name: "L ↓" },
  { kind: "axis", which: "rx", name: "R →" },
  { kind: "axis", which: "ry", name: "R ↓" },
];

/** An empty profile to fill step by step. */
export function blankProfile(): PadProfile {
  return { buttons: [], l2: null, r2: null, lx: null, ly: null, rx: null, ry: null };
}

/** `prof` with `step` set from what was pressed (a button step replaces that button's sources). */
export function applyStep(prof: PadProfile, step: MapStep, got: PadSource | PadAxis): PadProfile {
  if (step.kind === "button") {
    return { ...prof, buttons: [...prof.buttons.filter(([bit]) => bit !== step.bit), [step.bit, got as PadSource]] };
  }
  if (step.kind === "trigger") return { ...prof, [step.which]: got as PadSource };
  return { ...prof, [step.which]: got as PadAxis };
}

/** An extra button (a back paddle, say) set to also press `bit`. */
export function addExtra(prof: PadProfile, bit: number, src: PadSource): PadProfile {
  const same = (a: PadSource, b: PadSource) => JSON.stringify(a) === JSON.stringify(b);
  return { ...prof, buttons: [...prof.buttons.filter(([, s]) => !same(s, src)), [bit, src]] };
}

/** Steps skipped in a mapping keep what the standard layout had, unless that button or axis
 *  was given to another control (✕ mapped to button 5 must not also press R1, button 5's
 *  standard meaning). */
export function withStandardGaps(prof: PadProfile, base: PadProfile = STANDARD_PROFILE): PadProfile {
  const have = new Set(prof.buttons.map(([bit]) => bit));
  const key = (s: PadSource) => ("b" in s ? `b${s.b}` : "h" in s ? `h${s.h}.${s.at}` : `a${s.a}${s.d}`);
  const used = new Set([...prof.buttons.map(([, s]) => key(s)), ...[prof.l2, prof.r2].filter((s): s is PadSource => !!s).map(key)]);
  return {
    buttons: [
      ...prof.buttons,
      ...base.buttons.filter(([bit, src]) => !have.has(bit) && !used.has(key(src))),
    ],
    l2: prof.l2 ?? base.l2,
    r2: prof.r2 ?? base.r2,
    lx: prof.lx ?? base.lx,
    ly: prof.ly ?? base.ly,
    rx: prof.rx ?? base.rx,
    ry: prof.ry ?? base.ry,
  };
}

/** The quick check a friend runs on the Play Together page: the face buttons by position, the
 *  d-pad, the shoulders and triggers, then the sticks. The rest keep what the controller had. */
export const CHECK_STEPS: MapStep[] = MAP_STEPS.filter(
  (s) =>
    s.kind !== "button" ||
    !(
      [PadButton.L3, PadButton.R3, PadButton.OPTIONS, PadButton.SHARE, PadButton.PS, PadButton.TOUCHPAD] as number[]
    ).includes(s.bit),
);

/** Everything back where it rests (within a little), after a press. */
export function atRest(rest: PadSnapshot, now: PadSnapshot): boolean {
  return (
    now.buttons.every((v, i) => Math.abs(v - (rest.buttons[i] ?? 0)) < 0.3) &&
    now.axes.every((v, i) => Math.abs(v - (rest.axes[i] ?? 0)) < 0.3)
  );
}

/**
 * Mapping a controller one step at a time, fed the controller every frame: the first reading is
 * where everything rests; then each step waits for everything to be let go, and takes the next
 * press (or stick push). Shared by Play's controller setup and the Play Together check.
 */
export class PadLearner {
  step = 0;
  private prof = blankProfile();
  private rest: PadSnapshot | null = null;
  private waiting = true;

  constructor(readonly steps: readonly MapStep[] = MAP_STEPS) {}

  get done(): boolean {
    return this.step >= this.steps.length;
  }

  get current(): MapStep | null {
    return this.steps[this.step] ?? null;
  }

  /** One reading of the controller. True when it answered a step. */
  feed(now: PadSnapshot): boolean {
    if (this.done) return false;
    if (!this.rest) {
      // Where everything rests, taken as the check opens (triggers on axes rest at -1).
      this.rest = now;
      this.waiting = false;
      return false;
    }
    if (this.waiting) {
      if (atRest(this.rest, now)) this.waiting = false;
      return false;
    }
    const s = this.steps[this.step];
    const got = s.kind === "axis" ? detectAxis(this.rest, now) : detectPress(this.rest, now);
    if (!got) return false;
    this.prof = applyStep(this.prof, s, got);
    this.go(this.step + 1);
    return true;
  }

  /** Skip this step (it keeps what the controller had). */
  skip(): void {
    this.go(this.step + 1);
  }

  back(): void {
    if (this.step > 0) this.go(this.step - 1);
  }

  private go(to: number): void {
    this.waiting = true;
    this.step = Math.min(to, this.steps.length);
  }

  /** What was learned, the skipped steps filled from `base`. */
  profile(base: PadProfile = STANDARD_PROFILE): PadProfile {
    return withStandardGaps(this.prof, base);
  }
}
