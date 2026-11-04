/**
 * Controller, pointer and keyboard navigation for the console page.
 *
 * Focus is always real DOM focus on real controls (<button>, <a>, inputs), so
 * the browser's own focus handling keeps working; this layer only adds:
 *   - pointer = focus: the PS5 browser drives an on-screen pointer with the
 *     left stick and ✕ clicks at it, so hovering a control focuses it; the
 *     focus ring and what ✕ activates are then the same element;
 *   - spatial movement: a direction moves focus to the nearest focusable
 *     element that way (nextFocus), from arrow keys or the D-pad;
 *   - confirm: clicks the focused control (Enter, or ✕ when it arrives as a key);
 *   - page moves (L2/R2), top of the list (L3), the rail (R3);
 *   - the other actions (back, actions, select, prevSection, nextSection,
 *     help, refresh, fullscreen) go to the handlers registered with
 *     onNavAction, newest first.
 *
 * Which keys the PS5 browser really delivers for each button has not been
 * measured yet, so the bindings are one table, DEFAULT_INPUT_MAP, covering the
 * whole DualSense with a `verified` flag per entry (the input log, inputLog.ts,
 * is how they get measured). The HintBar shows ✕ and ○ always and the others
 * only once verified, and every action has an on-screen control as well, so
 * nothing depends on a shortcut.
 */
import { requestBack } from "./backHistory";

export type Direction = "up" | "down" | "left" | "right";
export type NavAction =
  | Direction
  | "confirm"
  | "back"
  | "actions"
  | "select"
  | "prevSection"
  | "nextSection"
  | "pageUp"
  | "pageDown"
  | "top"
  | "rail"
  | "help"
  | "refresh"
  | "fullscreen";

/** What a table entry does: a NavAction, or a button the page never acts on itself. */
export type ButtonAction =
  | NavAction
  /** The browser's own pointer (left stick): hover = focus, handled by installNavigation. */
  | "pointer"
  /** The browser's own scrolling (right stick): left native. */
  | "scroll"
  /** Used by the PS5 itself, never delivered to the page (PS, Mute). */
  | "system";

export type ButtonId =
  | "cross"
  | "circle"
  | "triangle"
  | "square"
  | "up"
  | "down"
  | "left"
  | "right"
  | "lstick"
  | "rstick"
  | "l1"
  | "r1"
  | "l2"
  | "r2"
  | "l3"
  | "r3"
  | "options"
  | "create"
  | "touchpad"
  | "ps"
  | "mute";

export interface InputBinding {
  button: ButtonId;
  /** The controller glyph the HintBar and the Controls screen show. */
  glyph: string;
  action: ButtonAction;
  /** KeyboardEvent.key values the button may arrive as (seeded guesses until verified). */
  keys: string[];
  /** KeyboardEvent.keyCode values, for a browser that names the key "Unidentified". */
  keyCodes: number[];
  /** Gamepad API "standard" button index (only where the Gamepad API exists; the PS5 browser has none). */
  pad?: number;
  /** Measured on a real console. Only verified entries (and ✕/○) are advertised. */
  verified: boolean;
}

/** The whole controller, one entry per button. */
export type InputMap = InputBinding[];

const bind = (
  button: ButtonId,
  glyph: string,
  action: ButtonAction,
  keys: string[],
  keyCodes: number[],
  pad?: number,
): InputBinding => ({ button, glyph, action, keys, keyCodes, pad, verified: false });

/**
 * Every DualSense button. Keys are the standard keyboard equivalents where one
 * exists (the measurement pass replaces them with what the PS5 browser really
 * sends and sets `verified`).
 */
export const DEFAULT_INPUT_MAP: InputMap = [
  bind("cross", "✕", "confirm", ["Enter"], [13], 0),
  bind("circle", "○", "back", ["Escape", "Backspace", "GoBack", "BrowserBack"], [27, 8], 1),
  bind("square", "□", "select", [], [], 2),
  bind("triangle", "△", "actions", ["ContextMenu"], [93], 3),
  bind("up", "↑", "up", ["ArrowUp"], [38], 12),
  bind("down", "↓", "down", ["ArrowDown"], [40], 13),
  bind("left", "←", "left", ["ArrowLeft"], [37], 14),
  bind("right", "→", "right", ["ArrowRight"], [39], 15),
  bind("lstick", "LS", "pointer", [], []),
  bind("rstick", "RS", "scroll", [], []),
  bind("l1", "L1", "prevSection", [], [], 4),
  bind("r1", "R1", "nextSection", [], [], 5),
  bind("l2", "L2", "pageUp", ["PageUp"], [33], 6),
  bind("r2", "R2", "pageDown", ["PageDown"], [34], 7),
  bind("l3", "L3", "top", ["Home"], [36], 10),
  bind("r3", "R3", "rail", [], [], 11),
  bind("options", "OPTIONS", "help", ["F1", "Help"], [112], 9),
  bind("create", "CREATE", "refresh", ["F5", "BrowserRefresh"], [116], 8),
  bind("touchpad", "TOUCHPAD", "fullscreen", ["F11"], [122]),
  bind("ps", "PS", "system", [], []),
  bind("mute", "MUTE", "system", [], []),
];

const ACTIONS_THE_PAGE_RUNS: ReadonlySet<ButtonAction> = new Set<ButtonAction>([
  "up",
  "down",
  "left",
  "right",
  "confirm",
  "back",
  "actions",
  "select",
  "prevSection",
  "nextSection",
  "pageUp",
  "pageDown",
  "top",
  "rail",
  "help",
  "refresh",
  "fullscreen",
]);

function isNavAction(a: ButtonAction): a is NavAction {
  return ACTIONS_THE_PAGE_RUNS.has(a);
}

/*
 * Ruling R18: a binding acts only once measured (verified). Until then only the
 * keys the page always handled stay live: the arrows, Enter, Escape and
 * Backspace (and their keyCodes). Every other seeded key (F5, Home, PageUp, ...)
 * is inert: not matched, its default never prevented, so it keeps doing what
 * the browser does with it. The entries stay in the table for the Controls
 * screen and the measurement pass.
 */
const ALWAYS_LIVE_KEYS: ReadonlySet<string> = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter", "Escape", "Backspace"]);
const ALWAYS_LIVE_KEYCODES: ReadonlySet<number> = new Set([38, 40, 37, 39, 13, 27, 8]);
/** Gamepad buttons the page always handled (✕ ○ □ △ L1 R1 and the D-pad). */
const ALWAYS_LIVE_PADS: ReadonlySet<number> = new Set([0, 1, 2, 3, 4, 5, 12, 13, 14, 15]);

/** The binding Gamepad API button `index` ("standard" mapping) belongs to, and whether the page acts on it. */
export function bindingForPad(map: InputMap, index: number): { binding: InputBinding; acts: boolean } | null {
  const b = map.find((x) => x.pad === index);
  if (!b) return null;
  return { binding: b, acts: isNavAction(b.action) && (b.verified || ALWAYS_LIVE_PADS.has(index)) };
}

/** The binding a keydown belongs to: by `key`, else by `keyCode` (a key the browser does not name). Inert entries never match. */
export function bindingForKey(map: InputMap, key: string, keyCode: number): InputBinding | null {
  for (const b of map) {
    if (isNavAction(b.action) && b.keys.includes(key) && (b.verified || ALWAYS_LIVE_KEYS.has(key))) return b;
  }
  if (key.length === 1) return null; // a printable key is text, never a button
  if (keyCode) {
    for (const b of map) {
      if (isNavAction(b.action) && b.keyCodes.includes(keyCode) && (b.verified || ALWAYS_LIVE_KEYCODES.has(keyCode))) return b;
    }
  }
  return null;
}

/* ---- spatial focus ---------------------------------------------------- */

export interface FocusCandidate {
  el: HTMLElement;
  rect: DOMRect;
}

/** How far `r` lies beyond `from` in `dir` (negative: it does not). */
function primaryGap(from: DOMRect, r: DOMRect, dir: Direction): number {
  switch (dir) {
    case "down":
      return r.top - from.bottom;
    case "up":
      return from.top - r.bottom;
    case "right":
      return r.left - from.right;
    case "left":
      return from.left - r.right;
  }
}

/** Whether `r` is past `from` in `dir` at all: its centre beyond `from`'s and its
 *  leading edge not behind `from`'s leading edge (1 px of slack for rounding). */
function isBeyond(from: DOMRect, r: DOMRect, dir: Direction): boolean {
  const fcx = (from.left + from.right) / 2;
  const fcy = (from.top + from.bottom) / 2;
  const cx = (r.left + r.right) / 2;
  const cy = (r.top + r.bottom) / 2;
  switch (dir) {
    case "down":
      return cy > fcy && r.top >= from.top + 1;
    case "up":
      return cy < fcy && r.bottom <= from.bottom - 1;
    case "right":
      return cx > fcx && r.left >= from.left + 1;
    case "left":
      return cx < fcx && r.right <= from.right - 1;
  }
}

/** Cross-axis gap between the two (0 when their ranges overlap) and the overlap size. */
function crossAxis(from: DOMRect, r: DOMRect, dir: Direction): { gap: number; overlap: number } {
  const vertical = dir === "up" || dir === "down";
  const a0 = vertical ? from.left : from.top;
  const a1 = vertical ? from.right : from.bottom;
  const b0 = vertical ? r.left : r.top;
  const b1 = vertical ? r.right : r.bottom;
  const overlap = Math.min(a1, b1) - Math.max(a0, b0);
  return overlap > 0 ? { gap: 0, overlap } : { gap: -overlap, overlap: 0 };
}

/**
 * The element focus moves to from `from` in `dir`, or null at the edge.
 * Candidates that overlap `from` on the cross axis (in its "beam") always win;
 * among them the nearest along the direction. Without one, the nearest by
 * distance with the cross-axis offset weighted double.
 */
export function nextFocus(
  from: DOMRect,
  candidates: FocusCandidate[],
  dir: Direction,
): HTMLElement | null {
  let best: HTMLElement | null = null;
  let bestScore = Infinity;
  let bestInBeam = false;
  for (const c of candidates) {
    if (!isBeyond(from, c.rect, dir)) continue;
    const gap = Math.max(0, primaryGap(from, c.rect, dir));
    const cross = crossAxis(from, c.rect, dir);
    const inBeam = cross.overlap > 0;
    if (bestInBeam && !inBeam) continue;
    const score = inBeam ? gap - cross.overlap / 1e6 : Math.hypot(gap, 2 * cross.gap);
    if ((inBeam && !bestInBeam) || score < bestScore) {
      best = c.el;
      bestScore = score;
      bestInBeam = inBeam;
    }
  }
  return best;
}

/* ---- gamepad ---------------------------------------------------------- */

/** Held D-pad: the first repeat after this long, then one every REPEAT_MS. */
export const REPEAT_DELAY_MS = 400;
export const REPEAT_MS = 120;

const DIRECTIONS: ReadonlySet<NavAction> = new Set(["up", "down", "left", "right"]);

/**
 * A stateful reader: given which buttons are down now, the actions to fire.
 * Buttons fire on the press; directions also repeat while held.
 */
export function createPadReader(map: InputMap): (pressed: boolean[], now: number) => NavAction[] {
  const downSince = new Map<number, number>();
  const lastFire = new Map<number, number>();
  const padded = map.filter(
    (b) => b.pad !== undefined && isNavAction(b.action) && (b.verified || ALWAYS_LIVE_PADS.has(b.pad)),
  );
  return (pressed, now) => {
    const out: NavAction[] = [];
    for (const binding of padded) {
      const i = binding.pad as number;
      const action = binding.action as NavAction;
      if (!pressed[i]) {
        downSince.delete(i);
        continue;
      }
      const since = downSince.get(i);
      if (since === undefined) {
        downSince.set(i, now);
        lastFire.set(i, now);
        out.push(action);
        continue;
      }
      if (!DIRECTIONS.has(action)) continue;
      const last = lastFire.get(i) ?? since;
      const due = last === since ? since + REPEAT_DELAY_MS : last + REPEAT_MS;
      if (now >= due) {
        lastFire.set(i, now);
        out.push(action);
      }
    }
    return out;
  };
}

/**
 * R12: the PS5 browser may report one D-pad press twice, as a Gamepad button
 * and as an Arrow keydown. An Arrow keydown within ECHO_MS after the gamepad
 * moved the same way is that echo and is dropped.
 */
export const ECHO_MS = 100;

export function createEchoGuard(windowMs: number = ECHO_MS) {
  const padAt: Partial<Record<Direction, number>> = {};
  return {
    /** The gamepad fired `dir` at `now`. */
    padFired(dir: Direction, now: number): void {
      padAt[dir] = now;
    },
    /** Whether an Arrow keydown for `dir` at `now` is a real press (not the echo). */
    keyAllowed(dir: Direction, now: number): boolean {
      const at = padAt[dir];
      return at === undefined || now - at >= windowMs;
    },
  };
}

function isDirection(a: ButtonAction): a is Direction {
  return a === "up" || a === "down" || a === "left" || a === "right";
}

/* ---- action handlers -------------------------------------------------- */

/** Return true when the action was handled (stops older handlers). */
export type NavHandler = (action: NavAction) => boolean | void;

/** Where a handler belongs: an element getter (a ref), read at dispatch time. */
export type NavScope = () => Element | null;

interface Registered {
  fn: NavHandler;
  scope?: NavScope;
}

const handlers: Registered[] = [];

/**
 * Register a handler for the non-movement actions; the newest runs first.
 *
 * Modals: while an element with [data-nav-modal] is open, only handlers whose
 * `scope` element is that (topmost) modal or inside it run; every other one is
 * skipped, so the page behind a dialog never reacts to ○, L1/R1 or △. A modal
 * (confirm dialog, actions sheet, path picker) registers its own handlers with
 * `scope` = a ref to its root. With no modal open every handler runs.
 *
 * `fallback`: run after every other handler, whenever it was registered. App's
 * ○ (content → rail → Home) is one, so a section's own ○ (Files: up one
 * folder) wins even though App re-registers after the section mounts.
 */
export function onNavAction(fn: NavHandler, scope?: NavScope, opts?: { fallback?: boolean }): () => void {
  const entry: Registered = { fn, scope };
  if (opts?.fallback) handlers.push(entry);
  else handlers.unshift(entry);
  return () => {
    const i = handlers.indexOf(entry);
    if (i >= 0) handlers.splice(i, 1);
  };
}

/** The topmost open [data-nav-modal] (the last in document order), or null. */
export function topModal(): HTMLElement | null {
  const modals = document.querySelectorAll<HTMLElement>("[data-nav-modal]");
  return modals.length ? modals[modals.length - 1] : null;
}

export const FOCUSABLE =
  'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Focusable, visible elements, inside the topmost [data-nav-modal] when one is open. */
function focusables(): FocusCandidate[] {
  const root: ParentNode = topModal() ?? document;
  const out: FocusCandidate[] = [];
  root.querySelectorAll<HTMLElement>(FOCUSABLE).forEach((el) => {
    if (el.closest("[inert], [aria-hidden='true']")) return;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    out.push({ el, rect });
  });
  return out;
}

/* ---- regions ----------------------------------------------------------- */

/*
 * The page is split into regions, [data-nav-region] (App: the rail and the
 * content). Up/Down never leave the focused element's region. Left/Right move
 * inside it while something lies straight that way; otherwise they enter the
 * neighbouring region that way, at its entry: its [data-nav-entry] (the rail's
 * active section), else the element last focused there if it is still shown,
 * else its first control. Open modals ignore regions (focus stays in the modal).
 */

const REGION = "[data-nav-region]";

/** Region element → the element last focused inside it. */
const lastInRegion = new WeakMap<Element, HTMLElement>();

function regionOf(el: Element | null): Element | null {
  return el && typeof el.closest === "function" ? el.closest(REGION) : null;
}

function remember(el: Element | null): void {
  const region = regionOf(el);
  if (region) lastInRegion.set(region, el as HTMLElement);
}

/** Where focus lands when it enters `region` from another one. */
function regionEntry(region: Element, all: FocusCandidate[]): HTMLElement | null {
  const inside = all.filter((c) => region.contains(c.el));
  if (!inside.length) return null;
  const marked = inside.find((c) => c.el.hasAttribute("data-nav-entry"));
  if (marked) return marked.el;
  const last = lastInRegion.get(region);
  const kept = last ? inside.find((c) => c.el === last) : undefined;
  return (kept ?? inside[0]).el;
}

/** Focus `el` and keep it on screen. */
export function focusElement(el: HTMLElement): void {
  if (typeof document !== "undefined") remember(document.activeElement);
  el.focus();
  remember(el);
  if (typeof el.scrollIntoView === "function") el.scrollIntoView({ block: "nearest", inline: "nearest" });
}

function inBeam(from: DOMRect, r: DOMRect, dir: Direction): boolean {
  return crossAxis(from, r, dir).overlap > 0;
}

/** The target of `dir` from `current` when it sits in `region` (no modal open). */
function regionMove(current: FocusCandidate, region: Element, all: FocusCandidate[], dir: Direction): HTMLElement | null {
  const others = all.filter((c) => c !== current);
  const same = others.filter((c) => region.contains(c.el));
  if (dir === "up" || dir === "down") return nextFocus(current.rect, same, dir);
  const aligned = same.filter((c) => isBeyond(current.rect, c.rect, dir) && inBeam(current.rect, c.rect, dir));
  if (aligned.length) return nextFocus(current.rect, aligned, dir);
  // Nothing straight that way here: the neighbouring region that way, if any.
  const regions: FocusCandidate[] = [];
  document.querySelectorAll<HTMLElement>(REGION).forEach((r) => {
    if (r === region || r.contains(region) || region.contains(r)) return;
    if (!all.some((c) => r.contains(c.el))) return;
    regions.push({ el: r, rect: r.getBoundingClientRect() });
  });
  const neighbour = nextFocus((region as HTMLElement).getBoundingClientRect(), regions, dir);
  if (neighbour) return regionEntry(neighbour, all);
  return nextFocus(current.rect, same, dir);
}

/** Move focus one step `dir`; the distance the focus travelled along `dir` (before any scroll), or null. */
function move(dir: Direction): number | null {
  const all = focusables();
  if (!all.length) return null;
  const active = document.activeElement as HTMLElement | null;
  const current = active ? all.find((c) => c.el === active) : undefined;
  if (!current) {
    const preferred = all.find((c) => c.el.hasAttribute("data-nav-default")) ?? all[0];
    focusElement(preferred.el);
    return null;
  }
  const region = topModal() ? null : regionOf(current.el);
  const next = region
    ? regionMove(current, region, all, dir)
    : nextFocus(
        current.rect,
        all.filter((c) => c !== current),
        dir,
      );
  if (!next) return null;
  const to = all.find((c) => c.el === next)?.rect ?? next.getBoundingClientRect();
  const along =
    dir === "up" || dir === "down" ? Math.abs(to.top - current.rect.top) : Math.abs(to.left - current.rect.left);
  focusElement(next);
  return along;
}

/** A page for L2/R2: most of the viewport, so the last row of one page is still in sight on the next. */
export const PAGE_FRACTION = 0.8;
const PAGE_STEPS_MAX = 200;

/** L2/R2: focus moves down (up) the current list until it has travelled about a page. */
function page(dir: "up" | "down"): void {
  const span = (typeof window !== "undefined" && window.innerHeight ? window.innerHeight : 700) * PAGE_FRACTION;
  let travelled = 0;
  for (let i = 0; i < PAGE_STEPS_MAX && travelled < span; i++) {
    const d = move(dir);
    if (d === null || d <= 0) break;
    travelled += d;
  }
}

/** L3: the first control of the focused element's region (or the open modal), scrolled to the top. */
function toTop(): void {
  const all = focusables();
  if (!all.length) return;
  const modal = topModal();
  const active = document.activeElement as HTMLElement | null;
  const region = modal ?? regionOf(active) ?? null;
  const inside = region ? all.filter((c) => region.contains(c.el)) : all;
  if (!inside.length) return;
  if (region && typeof (region as HTMLElement).scrollTop === "number") (region as HTMLElement).scrollTop = 0;
  focusElement(inside[0].el);
}

/** R3: the section rail, at its entry (the active section). */
function toRail(): void {
  if (topModal()) return;
  const all = focusables();
  const rail = document.querySelectorAll<HTMLElement>('[data-nav-region="rail"]')[0];
  const regions = rail ? [rail] : Array.from(document.querySelectorAll<HTMLElement>(REGION));
  const target = regions.length ? regionEntry(regions[0], all) : null;
  if (target) focusElement(target);
}

/**
 * ← / → on an element marked [data-nav-arrows] (the viewer's picture) do not
 * move focus: the element gets a NAV_ARROW_EVENT (detail "left" | "right")
 * instead and decides what they mean (previous / next image).
 */
export const NAV_ARROW_EVENT = "cw-nav-arrow";

function handArrow(dir: Direction): boolean {
  if (dir !== "left" && dir !== "right") return false;
  if (typeof document === "undefined" || typeof CustomEvent === "undefined") return false;
  const active = document.activeElement as HTMLElement | null;
  if (!active || typeof active.hasAttribute !== "function" || !active.hasAttribute("data-nav-arrows")) return false;
  active.dispatchEvent(new CustomEvent(NAV_ARROW_EVENT, { detail: dir }));
  return true;
}

/** Run one action: movement and confirm here, the rest through the handlers. */
export function dispatchNav(action: NavAction): void {
  if (isDirection(action)) {
    if (handArrow(action)) return;
    move(action);
    return;
  }
  const modal = topModal();
  const active = document.activeElement as HTMLElement | null;
  if (action === "confirm") {
    // Focus left behind a modal (it opened over the page) never gets clicked:
    // the first press brings focus into the modal instead.
    if (modal && !(active && modal.contains(active))) {
      const first = focusables()[0];
      if (first) focusElement(first.el);
      return;
    }
    if (active && active !== document.body && typeof active.click === "function") active.click();
    return;
  }
  for (const h of handlers.slice()) {
    if (modal) {
      const scope = h.scope ? h.scope() : null;
      if (!scope || !modal.contains(scope)) continue;
    }
    if (h.fn(action)) return;
  }
  // Nobody took it: the page-wide default, where there is one.
  if (action === "pageUp") page("up");
  else if (action === "pageDown") page("down");
  else if (action === "top") toTop();
  else if (action === "rail") toRail();
}

/** Text fields keep the keys that edit text. */
function isTextField(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  if (tag === "TEXTAREA" || (el as HTMLElement).isContentEditable) return true;
  if (tag !== "INPUT") return false;
  const type = (el as HTMLInputElement).type;
  return !["button", "checkbox", "radio", "submit", "reset", "range", "color", "file"].includes(type);
}

/**
 * A multi-line text box keeps ↑ / ↓ while the caret has a line to move to (the
 * text editor on a PC or phone keyboard); on its first line ↑, and on its last
 * line ↓, move focus out as usual, so the D-pad still reaches Save.
 */
export function textareaKeepsKey(
  el: { tagName: string; value?: string; selectionStart?: number | null; selectionEnd?: number | null } | null,
  action: NavAction,
): boolean {
  if (!el || el.tagName !== "TEXTAREA" || (action !== "up" && action !== "down")) return false;
  const v = el.value ?? "";
  const at = action === "up" ? (el.selectionStart ?? 0) : (el.selectionEnd ?? v.length);
  if (action === "up") return at > 0 && v.lastIndexOf("\n", at - 1) >= 0;
  return v.indexOf("\n", at) >= 0;
}

/**
 * One input's action. Back goes through browser history (backHistory.ts): the
 * PS5 browser's own ○ is history back, so Escape, Backspace and a gamepad ○
 * take that same path and every back runs from popstate.
 */
function act(action: NavAction): void {
  if (action === "back") requestBack(() => dispatchNav("back"));
  else dispatchNav(action);
}

/** Pointer movement smaller than this (px) after a key move is stick drift or a scroll re-send, not the user. */
export const POINTER_SLOP_PX = 6;
/** A field typed into this recently keeps focus against hover. */
export const TYPING_HOLD_MS = 1500;

/** Focus `el` because the pointer is over it: no scroll (the pointer is already there). */
function focusFromPointer(el: HTMLElement): void {
  remember(document.activeElement);
  el.focus({ preventScroll: true });
  remember(el);
}

/** The control a pointer event is over, if hovering may focus it now. */
function hoverTarget(target: EventTarget | null): HTMLElement | null {
  const t = target as Element | null;
  if (!t || typeof t.closest !== "function") return null;
  const el = t.closest<HTMLElement>(FOCUSABLE);
  if (!el || el.closest("[inert], [aria-hidden='true']")) return null;
  // A text field takes focus only from a click/✕ or a key: hovering one would put the ring on it
  // (and may raise the PS5's on-screen keyboard) while the pointer moves on.
  if (isTextField(el)) return null;
  const modal = topModal();
  if (modal && !modal.contains(el)) return null;
  return el;
}

/**
 * Listen for the keys, the pointer and gamepad buttons until the returned
 * function is called.
 *
 * Keyboard: keydown (one a control handled itself is left alone), matched by
 * key then keyCode (bindingForKey); unverified bindings outside the always-live
 * set are inert (ruling R18).
 *
 * Pointer: a mousemove over a control focuses it (hover = focus); text fields
 * excepted, and a field typed into in the last TYPING_HOLD_MS keeps focus. After a key
 * moved focus the pointer is "parked": it stays where it was, so moves within
 * POINTER_SLOP_PX of that spot are ignored (stick drift, the browser re-sending
 * the spot after a scroll), and a pointer click there, which would land on
 * whatever is under the pointer, activates the focused control instead. The
 * ring and what ✕ activates never disagree. The wheel (right stick) is left
 * to the browser.
 *
 * Gamepad: polled once per animation frame where the Gamepad API exists.
 */
export function installNavigation(map: InputMap): () => void {
  const echo = createEchoGuard();
  const ptr = { x: NaN, y: NaN, parked: false, ax: NaN, ay: NaN };
  const park = () => {
    ptr.parked = true;
    ptr.ax = ptr.x;
    ptr.ay = ptr.y;
  };

  let typedAt = -Infinity;

  const onKey = (e: KeyboardEvent) => {
    if (isTextField(e.target as Element | null)) typedAt = performance.now();
    if (e.altKey || e.ctrlKey || e.metaKey || e.defaultPrevented) return;
    const binding = bindingForKey(map, e.key, e.keyCode);
    if (!binding || !isNavAction(binding.action)) return;
    const action = binding.action;
    const typing = isTextField(e.target as Element | null);
    if (
      typing &&
      (action === "left" ||
        action === "right" ||
        action === "confirm" ||
        action === "top" ||
        action === "pageUp" ||
        action === "pageDown" ||
        e.key === "Backspace" ||
        textareaKeepsKey(e.target as HTMLTextAreaElement, action))
    ) {
      return;
    }
    e.preventDefault();
    if (isDirection(action) && !echo.keyAllowed(action, performance.now())) return;
    park();
    act(action);
  };

  const onMove = (e: MouseEvent) => {
    const x = e.clientX;
    const y = e.clientY;
    if (ptr.parked) {
      if (Number.isNaN(ptr.ax)) {
        // Where the pointer sat when the key moved focus was unknown: this is that spot.
        ptr.ax = x;
        ptr.ay = y;
        ptr.x = x;
        ptr.y = y;
        return;
      }
      if (Math.hypot(x - ptr.ax, y - ptr.ay) < POINTER_SLOP_PX) return;
      ptr.parked = false;
    }
    ptr.x = x;
    ptr.y = y;
    const el = hoverTarget(e.target);
    if (!el || el === document.activeElement) return;
    // Never pull focus out of a field while it is being typed into.
    if (isTextField(document.activeElement) && performance.now() - typedAt < TYPING_HOLD_MS) return;
    focusFromPointer(el);
  };

  /** A parked pointer's press would land away from the ring: true when it must be redirected. */
  const strays = (target: EventTarget | null): boolean => {
    if (!ptr.parked) return false;
    const active = document.activeElement as HTMLElement | null;
    if (!active || active === document.body) return false;
    return !active.contains(target as Node | null);
  };
  const onDown = (e: MouseEvent) => {
    if (e.button === 0 && strays(e.target)) e.preventDefault(); // focus stays on the ring
  };
  const onClick = (e: MouseEvent) => {
    // detail 0: a click from a key or from .click(), not the pointer.
    if (!e.isTrusted || !e.detail || !strays(e.target)) return;
    e.preventDefault();
    e.stopPropagation();
    dispatchNav("confirm");
  };

  window.addEventListener("keydown", onKey);
  window.addEventListener("mousemove", onMove, true);
  window.addEventListener("mousedown", onDown, true);
  window.addEventListener("click", onClick, true);

  let frame = 0;
  let stopped = false;
  const readers = new Map<number, ReturnType<typeof createPadReader>>();
  const canPoll = typeof navigator !== "undefined" && typeof navigator.getGamepads === "function";
  const poll = () => {
    if (stopped) return;
    const pads = navigator.getGamepads();
    const now = performance.now();
    for (let i = 0; i < pads.length; i++) {
      const pad = pads[i];
      if (!pad || pad.mapping !== "standard") continue;
      let read = readers.get(pad.index);
      if (!read) {
        read = createPadReader(map);
        readers.set(pad.index, read);
      }
      const pressed = pad.buttons.map((btn) => btn.pressed);
      for (const action of read(pressed, now)) {
        if (isDirection(action)) echo.padFired(action, now);
        park();
        act(action);
      }
    }
    frame = requestAnimationFrame(poll);
  };
  if (canPoll) frame = requestAnimationFrame(poll);

  return () => {
    stopped = true;
    window.removeEventListener("keydown", onKey);
    window.removeEventListener("mousemove", onMove, true);
    window.removeEventListener("mousedown", onDown, true);
    window.removeEventListener("click", onClick, true);
    if (frame) cancelAnimationFrame(frame);
  };
}

