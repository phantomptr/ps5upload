/**
 * What each controller button does on each screen, for the HintBar and the
 * help overlay (OPTIONS). One place, so the two never disagree.
 *
 * ✕ and ○ always work (✕ is the browser's click, ○ its Back); every other
 * button is advertised as working only once its table entry (nav.ts
 * DEFAULT_INPUT_MAP) is `verified`. The help overlay lists the rest as "not
 * confirmed yet", so a user is never told a button works when it may not.
 */
import type { ButtonAction, ButtonId, InputMap, NavAction } from "../nav";

/** May the page say this button works? ✕ and ○ always; any other once it was measured on a console. */
export function buttonWorks(map: InputMap, button: ButtonId): boolean {
  if (button === "cross" || button === "circle") return true;
  return map.some((b) => b.button === button && b.verified);
}

/** A screen: a section id (home, files, cheats, fan, settings) or an overlay. */
export type ScreenId = string;

/** Overlays: only ✕ and ○ do anything there. */
export const OVERLAY_SCREENS: ReadonlySet<string> = new Set(["help", "controls", "tour"]);

/** The page-wide buttons (App and nav.ts handle them on every section). */
const EVERY_SECTION: NavAction[] = [
  "prevSection",
  "nextSection",
  "pageUp",
  "pageDown",
  "top",
  "rail",
  "help",
  "refresh",
  "fullscreen",
];

/** Buttons with a job of their own on a screen, beyond ✕ ○ and the D-pad. */
export function screenActions(screen: ScreenId): NavAction[] {
  if (OVERLAY_SCREENS.has(screen)) return [];
  if (screen === "files") return ["actions", "select", ...EVERY_SECTION];
  if (screen === "cheats") return ["select", ...EVERY_SECTION];
  return EVERY_SECTION;
}

/** The short label (a cw_ key) of an action on a screen. */
export function hintLabel(action: ButtonAction, screen: ScreenId): string {
  if (screen === "cheats" && action === "select") return "cw_hint_switch";
  switch (action) {
    case "confirm":
      return "cw_hint_open";
    case "back":
      return "cw_hint_back";
    case "actions":
      return "cw_actions";
    case "select":
      return "cw_select";
    case "prevSection":
    case "nextSection":
      return "cw_hint_switch_section";
    case "pageUp":
    case "pageDown":
      return "cw_hint_page";
    case "top":
      return "cw_hint_top";
    case "rail":
      return "cw_nav_sections";
    case "help":
      return "cw_hint_help";
    case "refresh":
      return "cw_hint_refresh";
    case "fullscreen":
      return "cw_hint_fullscreen";
    default:
      return "cw_job_move";
  }
}

export interface Hint {
  /** cw_ key of the label. */
  labelKey: string;
  glyphs: string[];
  /** ✕/○, or every button behind it was measured on a console. */
  works: boolean;
}

const DIRECTION: ReadonlySet<ButtonAction> = new Set(["up", "down", "left", "right"]);

/**
 * The hints of `screen`: ✕ and ○ first (always working), then each button
 * with a job there, grouped by label (L1/R1 share "Switch section"), then the
 * D-pad as "Move". A group works only when all of its buttons are verified.
 */
export function screenHints(map: InputMap, screen: ScreenId): Hint[] {
  const out: Hint[] = [];
  const add = (labelKey: string, glyph: string, works: boolean) => {
    const g = out.find((h) => h.labelKey === labelKey);
    if (g) {
      g.glyphs.push(glyph);
      g.works = g.works && works;
    } else out.push({ labelKey, glyphs: [glyph], works });
  };
  for (const b of map) {
    if (b.button === "cross" || b.button === "circle") add(hintLabel(b.action, screen), b.glyph, true);
  }
  const wanted = new Set<ButtonAction>(screenActions(screen));
  for (const b of map) {
    if (b.button === "cross" || b.button === "circle" || !wanted.has(b.action)) continue;
    add(hintLabel(b.action, screen), b.glyph, b.verified);
  }
  if (!OVERLAY_SCREENS.has(screen)) {
    for (const b of map) if (DIRECTION.has(b.action)) add("cw_job_move", b.glyph, b.verified);
  }
  return out;
}

/** The HintBar's share: only what works (✕, ○ and verified buttons), no D-pad. */
export function barHints(map: InputMap, screen: ScreenId): Hint[] {
  return screenHints(map, screen).filter((h) => h.works && h.labelKey !== "cw_job_move");
}
