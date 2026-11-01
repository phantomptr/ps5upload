import { describe, expect, it } from "vitest";

import {
  ChordDetector,
  DEFAULT_SHORTCUTS,
  ShortcutListener,
  ShortcutRecorder,
  readShortcut,
  shortcutLabel,
  shortcutParts,
  shortcutProblem,
  type Shortcut,
  type Shortcuts,
} from "./kbmShortcuts";

const ev = (code: string, mods: Partial<Record<"ctrlKey" | "altKey" | "shiftKey" | "metaKey", boolean>> = {}) => ({
  code,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

const chord = (d: ChordDetector, seq: Array<[string, boolean]>) => seq.map(([c, down]) => d.key(c, down));

describe("ChordDetector (Ctrl+Option / Ctrl+Alt pressed and let go)", () => {
  const ctrlAlt = DEFAULT_SHORTCUTS.release;

  it("fires on the last key-up of the chord, either order, either side", () => {
    const d = new ChordDetector(ctrlAlt);
    expect(chord(d, [["ControlLeft", true], ["AltLeft", true], ["AltLeft", false], ["ControlLeft", false]])).toEqual([
      false,
      false,
      false,
      true,
    ]);
    expect(chord(d, [["AltRight", true], ["ControlRight", true], ["ControlRight", false], ["AltRight", false]]).slice(-1)[0]).toBe(true);
  });

  it("does not fire when another key went down in between (Ctrl+Alt+Del goes to the PS5)", () => {
    const d = new ChordDetector(ctrlAlt);
    expect(chord(d, [["ControlLeft", true], ["AltLeft", true], ["Delete", true], ["Delete", false], ["AltLeft", false], ["ControlLeft", false]])).not.toContain(true);
    // And the next clean chord works again.
    expect(chord(d, [["ControlLeft", true], ["AltLeft", true], ["AltLeft", false], ["ControlLeft", false]]).slice(-1)[0]).toBe(true);
  });

  it("needs exactly the chord's modifiers: Ctrl alone, or Ctrl+Alt+Shift, is not it", () => {
    const d = new ChordDetector(ctrlAlt);
    expect(chord(d, [["ControlLeft", true], ["ControlLeft", false]])).not.toContain(true);
    expect(chord(d, [["ControlLeft", true], ["AltLeft", true], ["ShiftLeft", true], ["ShiftLeft", false], ["AltLeft", false], ["ControlLeft", false]])).not.toContain(true);
  });

  it("ignores key repeats and a key-up it never saw go down", () => {
    const d = new ChordDetector(ctrlAlt);
    expect(chord(d, [["AltLeft", false]])).toEqual([false]);
    expect(chord(d, [["ControlLeft", true], ["ControlLeft", true], ["AltLeft", true], ["AltLeft", true], ["ControlLeft", false], ["AltLeft", false]]).slice(-1)[0]).toBe(true);
  });

  it("is never a shortcut with a key", () => {
    const d = new ChordDetector(DEFAULT_SHORTCUTS.cycle);
    expect(chord(d, [["ControlLeft", true], ["AltLeft", true], ["AltLeft", false], ["ControlLeft", false]])).not.toContain(true);
  });

  it("reset forgets a chord in progress", () => {
    const d = new ChordDetector(ctrlAlt);
    chord(d, [["ControlLeft", true], ["AltLeft", true]]);
    d.reset();
    expect(chord(d, [["AltLeft", false], ["ControlLeft", false]])).not.toContain(true);
  });
});

describe("ShortcutListener", () => {
  it("cycles on Ctrl+Alt+M (and swallows its key-up) without releasing", () => {
    let s: Shortcuts = { ...DEFAULT_SHORTCUTS };
    const l = new ShortcutListener(() => s);
    expect(l.feed(ev("ControlLeft", { ctrlKey: true }), true)).toBe(null);
    expect(l.feed(ev("AltLeft", { ctrlKey: true, altKey: true }), true)).toBe(null);
    expect(l.feed(ev("KeyM", { ctrlKey: true, altKey: true }), true)).toBe("cycle");
    expect(l.feed({ ...ev("KeyM", { ctrlKey: true, altKey: true }), repeat: true }, true)).toBe("consume");
    expect(l.feed(ev("KeyM", { ctrlKey: true, altKey: true }), false)).toBe("consume");
    expect(l.feed(ev("AltLeft", { ctrlKey: true }), false)).toBe(null);
    expect(l.feed(ev("ControlLeft"), false)).toBe(null);
    // A clean chord releases.
    l.feed(ev("ControlLeft", { ctrlKey: true }), true);
    l.feed(ev("AltLeft", { ctrlKey: true, altKey: true }), true);
    l.feed(ev("AltLeft", { ctrlKey: true }), false);
    expect(l.feed(ev("ControlLeft"), false)).toBe("release");
    // A customised release with a key fires on its key-down.
    s = { ...s, release: { ctrl: false, alt: false, shift: false, meta: false, code: "F12" } };
    expect(l.feed(ev("F12"), true)).toBe("release");
    expect(l.feed(ev("F12"), false)).toBe("consume");
    expect(l.feed(ev("KeyA"), true)).toBe(null);
  });
});

describe("ShortcutRecorder", () => {
  it("records modifiers and a key at the key-down", () => {
    const r = new ShortcutRecorder();
    expect(r.feed(ev("ControlLeft", { ctrlKey: true }), true)).toEqual({ done: false });
    expect(r.feed(ev("ShiftLeft", { ctrlKey: true, shiftKey: true }), true)).toEqual({ done: false });
    expect(r.feed(ev("KeyP", { ctrlKey: true, shiftKey: true }), true)).toEqual({
      done: true,
      shortcut: { ctrl: true, alt: false, shift: true, meta: false, code: "KeyP" },
    });
  });

  it("records a chord when the modifiers are all let go", () => {
    const r = new ShortcutRecorder();
    r.feed(ev("ControlLeft", { ctrlKey: true }), true);
    r.feed(ev("MetaLeft", { ctrlKey: true, metaKey: true }), true);
    expect(r.feed(ev("MetaLeft", { ctrlKey: true }), false)).toEqual({ done: false });
    expect(r.feed(ev("ControlLeft"), false)).toEqual({
      done: true,
      shortcut: { ctrl: true, alt: false, shift: false, meta: true, code: null },
    });
  });

  it("Esc alone cancels; Ctrl+Esc is a combination", () => {
    expect(new ShortcutRecorder().feed(ev("Escape"), true)).toEqual({ done: true, cancelled: true });
    expect(new ShortcutRecorder().feed(ev("Escape", { ctrlKey: true }), true)).toMatchObject({ done: true, shortcut: { code: "Escape", ctrl: true } });
  });
});

describe("shortcutProblem (the conflict check)", () => {
  const none: Shortcut = { ctrl: false, alt: false, shift: false, meta: false, code: null };
  const s = (p: Partial<Shortcut>): Shortcut => ({ ...none, ...p });
  const ctx = { other: DEFAULT_SHORTCUTS.cycle, keymap: { KeyW: "up", Enter: "cross" } };

  it("accepts the defaults", () => {
    expect(shortcutProblem("release", DEFAULT_SHORTCUTS.release, { ...ctx, other: DEFAULT_SHORTCUTS.cycle })).toBe(null);
    expect(shortcutProblem("cycle", DEFAULT_SHORTCUTS.cycle, { ...ctx, other: DEFAULT_SHORTCUTS.release })).toBe(null);
  });

  it("refuses a lone letter, Esc alone, one modifier, and a chord for the mode switch", () => {
    expect(shortcutProblem("release", s({ code: "KeyQ" }), ctx)).toBe("lone_key");
    expect(shortcutProblem("release", s({ code: "Escape" }), ctx)).toBe("esc");
    expect(shortcutProblem("release", s({ ctrl: true }), ctx)).toBe("one_modifier");
    expect(shortcutProblem("cycle", s({ ctrl: true, shift: true }), ctx)).toBe("needs_key");
    // A function key alone is fine.
    expect(shortcutProblem("release", s({ code: "F12" }), ctx)).toBe(null);
  });

  it("refuses the app's shortcuts, keyboard-controls keys and the other shortcut", () => {
    expect(shortcutProblem("cycle", s({ ctrl: true, code: "KeyK" }), ctx)).toBe("app");
    expect(shortcutProblem("cycle", s({ meta: true, code: "KeyQ" }), ctx)).toBe("app");
    expect(shortcutProblem("cycle", s({ shift: true, code: "Slash" }), ctx)).toBe("app");
    expect(shortcutProblem("release", s({ code: "F5" }), ctx)).toBe("app");
    // Esc with any modifier stays reserved: Shift+Esc would never reach the shortcut in full screen.
    expect(shortcutProblem("release", s({ shift: true, code: "Escape" }), ctx)).toBe("app");
    expect(shortcutProblem("cycle", s({ ctrl: true, alt: true, code: "Escape" }), ctx)).toBe("app");
    expect(shortcutProblem("release", s({ code: "F2" }), { ...ctx, keymap: { F2: "options" } })).toBe("keymap");
    expect(shortcutProblem("release", DEFAULT_SHORTCUTS.cycle, ctx)).toBe("same");
  });
});

describe("labels", () => {
  it("glyphs on a Mac, words elsewhere", () => {
    expect(shortcutLabel(DEFAULT_SHORTCUTS.release, true)).toBe("⌃ ⌥");
    expect(shortcutLabel(DEFAULT_SHORTCUTS.cycle, true)).toBe("⌃ ⌥ M");
    expect(shortcutLabel(DEFAULT_SHORTCUTS.release, false)).toBe("Ctrl+Alt");
    expect(shortcutLabel(DEFAULT_SHORTCUTS.cycle, false)).toBe("Ctrl+Alt+M");
    expect(shortcutParts({ ctrl: false, alt: false, shift: true, meta: true, code: "ArrowUp" }, false)).toEqual(["Shift", "Win", "↑"]);
  });

  it("reads back only well-formed shortcuts", () => {
    expect(readShortcut(DEFAULT_SHORTCUTS.release)).toEqual(DEFAULT_SHORTCUTS.release);
    expect(readShortcut({ ctrl: true, code: null })).toBe(null); // one modifier
    expect(readShortcut({ ctrl: true, code: "<script>" })).toBe(null);
    expect(readShortcut("x")).toBe(null);
  });
});
