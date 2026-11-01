// The stream's two keyboard & mouse shortcuts (Settings › Controls › Shortcuts):
//   release  gives the keyboard and mouse back to this computer. By default a chord: Ctrl+Option
//            (Mac) / Ctrl+Alt pressed and let go with no other key in between, as VMware does.
//            Ctrl+Alt+<key> still reaches the PS5: the modifiers go to it as they happen, and when
//            the chord completes everything is let go.
//   cycle    switches the input mode (Controller → Keyboard & mouse → Mixed). Ctrl+Alt+M.
// Kept per device. A shortcut is modifiers plus one key, or (release only) two or more modifiers.

/** Modifiers and a key (KeyboardEvent.code); `code: null` = modifiers alone (a chord). */
export interface Shortcut {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  code: string | null;
}

export type ShortcutKind = "release" | "cycle";

export interface Shortcuts {
  release: Shortcut;
  cycle: Shortcut;
}

export const DEFAULT_SHORTCUTS: Readonly<Shortcuts> = Object.freeze({
  release: { ctrl: true, alt: true, shift: false, meta: false, code: null },
  cycle: { ctrl: true, alt: true, shift: false, meta: false, code: "KeyM" },
});

type Mod = "ctrl" | "alt" | "shift" | "meta";
const MODS: readonly Mod[] = ["ctrl", "alt", "shift", "meta"];

/** The modifier a KeyboardEvent.code is, or null. */
export function modOf(code: string): Mod | null {
  if (code === "ControlLeft" || code === "ControlRight") return "ctrl";
  if (code === "AltLeft" || code === "AltRight") return "alt";
  if (code === "ShiftLeft" || code === "ShiftRight") return "shift";
  if (code === "MetaLeft" || code === "MetaRight" || code === "OSLeft" || code === "OSRight") return "meta";
  return null;
}

const modCount = (s: Shortcut) => MODS.filter((m) => s[m]).length;

export function sameShortcut(a: Shortcut, b: Shortcut): boolean {
  return MODS.every((m) => a[m] === b[m]) && a.code === b.code;
}

/** A stored value read back (anything malformed is null). */
export function readShortcut(v: unknown): Shortcut | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const code = typeof o.code === "string" && /^[A-Za-z0-9]{1,24}$/.test(o.code) ? o.code : null;
  if (o.code !== null && code === null) return null;
  const s = { ctrl: o.ctrl === true, alt: o.alt === true, shift: o.shift === true, meta: o.meta === true, code };
  return s.code === null && modCount(s) < 2 ? null : s;
}

/** Apple keyboards: the glyphs ⌃ ⌥ ⇧ ⌘ (also an iPad's keyboard). */
export function isAppleKeyboard(): boolean {
  return typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.userAgent || "");
}

/** A key as it is printed on the key. */
export function keyName(code: string): string {
  const names: Record<string, string> = {
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
    Space: "Space",
    Escape: "Esc",
    Backquote: "`",
    Minus: "-",
    Equal: "=",
    BracketLeft: "[",
    BracketRight: "]",
    Backslash: "\\",
    Semicolon: ";",
    Quote: "'",
    Comma: ",",
    Period: ".",
    Slash: "/",
    PageUp: "PgUp",
    PageDown: "PgDn",
    Delete: "Del",
    Insert: "Ins",
  };
  return names[code] ?? code.replace(/^Key/, "").replace(/^Digit/, "").replace(/^Numpad/, "Num ");
}

/** The keys one by one, as key caps show them: ["⌃", "⌥"] on a Mac, ["Ctrl", "Alt", "M"] elsewhere. */
export function shortcutParts(s: Shortcut, apple = isAppleKeyboard()): string[] {
  const out: string[] = [];
  if (apple) {
    if (s.ctrl) out.push("⌃");
    if (s.alt) out.push("⌥");
    if (s.shift) out.push("⇧");
    if (s.meta) out.push("⌘");
  } else {
    if (s.ctrl) out.push("Ctrl");
    if (s.alt) out.push("Alt");
    if (s.shift) out.push("Shift");
    if (s.meta) out.push("Win");
  }
  if (s.code) out.push(keyName(s.code));
  return out;
}

/** The shortcut in a sentence: "⌃ ⌥", "⌃ ⌥ M"; "Ctrl+Alt", "Ctrl+Alt+M". */
export function shortcutLabel(s: Shortcut, apple = isAppleKeyboard()): string {
  return shortcutParts(s, apple).join(apple ? " " : "+");
}

type KeyLike = Pick<KeyboardEvent, "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">;

/** This keydown is the shortcut (one with a key; a chord is ChordDetector's). */
export function matchesKey(s: Shortcut, e: KeyLike): boolean {
  return (
    s.code !== null &&
    e.code === s.code &&
    e.ctrlKey === s.ctrl &&
    e.altKey === s.alt &&
    e.shiftKey === s.shift &&
    e.metaKey === s.meta
  );
}

/**
 * A modifier chord: true on the key-up that completes it — the chord's modifiers (exactly those)
 * were down together and are all up again, and no other key went down or up in between.
 */
export class ChordDetector {
  private held = new Set<Mod>();
  private peak = new Set<Mod>();
  private tainted = false;

  constructor(private target: Shortcut) {}

  setTarget(s: Shortcut): void {
    if (sameShortcut(this.target, s)) return;
    this.target = s;
    this.reset();
  }

  /** A key went down or up. True when this completes the chord. */
  key(code: string, down: boolean): boolean {
    const m = modOf(code);
    if (!m) {
      if (this.held.size) this.tainted = true;
      return false;
    }
    if (down) {
      if (!this.held.size) {
        this.peak.clear();
        this.tainted = false;
      }
      this.held.add(m);
      this.peak.add(m);
      return false;
    }
    if (!this.held.has(m)) return false;
    this.held.delete(m);
    if (this.held.size) return false;
    const t = this.target;
    const hit =
      t.code === null &&
      !this.tainted &&
      this.peak.size === modCount(t) &&
      MODS.every((x) => this.peak.has(x) === t[x]);
    this.peak.clear();
    return hit;
  }

  reset(): void {
    this.held.clear();
    this.peak.clear();
    this.tainted = false;
  }
}

/** What a key event does under the shortcuts: `consume` = the key-up of a shortcut's key. */
export type ShortcutAction = "release" | "cycle" | "consume" | null;

/** Watches the key events going to the stream for the two shortcuts. */
export class ShortcutListener {
  private chord: ChordDetector;
  /** Keys whose key-down was a shortcut: their key-up is the shortcut's too. */
  private swallowed = new Set<string>();

  constructor(private get: () => Shortcuts) {
    this.chord = new ChordDetector(get().release);
  }

  feed(e: KeyLike & { repeat?: boolean }, down: boolean): ShortcutAction {
    const s = this.get();
    this.chord.setTarget(s.release);
    if (!down && this.swallowed.delete(e.code)) {
      this.chord.key(e.code, false);
      return "consume";
    }
    if (down && !modOf(e.code)) {
      for (const kind of ["release", "cycle"] as const) {
        if (!matchesKey(s[kind], e)) continue;
        this.chord.key(e.code, true); // taints a chord in progress
        this.swallowed.add(e.code);
        return e.repeat ? "consume" : kind;
      }
    }
    return this.chord.key(e.code, down) ? "release" : null;
  }

  reset(): void {
    this.chord.reset();
    this.swallowed.clear();
  }
}

// ── Recording a new shortcut ────────────────────────────────────────────────────────────────────

/** What the recorder made of a key event: still recording, a shortcut, or cancelled (Esc alone). */
export type RecordResult = { done: false } | { done: true; shortcut: Shortcut } | { done: true; cancelled: true };

/**
 * Records a shortcut as it is pressed: modifiers then a key gives that combination at the key's
 * key-down; modifiers alone gives the chord when they are all let go. Esc alone cancels.
 */
export class ShortcutRecorder {
  private held = new Set<Mod>();
  private peak = new Set<Mod>();

  feed(e: KeyLike, down: boolean): RecordResult {
    const m = modOf(e.code);
    if (m) {
      if (down) {
        this.held.add(m);
        this.peak.add(m);
        return { done: false };
      }
      this.held.delete(m);
      if (this.held.size || !this.peak.size) return { done: false };
      const s: Shortcut = {
        ctrl: this.peak.has("ctrl"),
        alt: this.peak.has("alt"),
        shift: this.peak.has("shift"),
        meta: this.peak.has("meta"),
        code: null,
      };
      this.peak.clear();
      return { done: true, shortcut: s };
    }
    if (!down) return { done: false };
    if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) return { done: true, cancelled: true };
    const s: Shortcut = { ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey, code: e.code };
    this.held.clear();
    this.peak.clear();
    return { done: true, shortcut: s };
  }
}

// ── What a shortcut may be ──────────────────────────────────────────────────────────────────────

/** Why a shortcut cannot be used (null: it can). */
export type ShortcutProblem =
  /** A chord needs two modifiers at least (one alone would fire on every Ctrl tap). */
  | "one_modifier"
  /** Switching modes needs a key, not modifiers alone. */
  | "needs_key"
  /** Esc alone (hold Esc is its own setting). */
  | "esc"
  /** A letter, digit or other typing key alone. */
  | "lone_key"
  /** The app's own shortcut (Ctrl/⌘+K, reload, ?, the system's ⌘Q …). */
  | "app"
  /** A key of the keyboard controls. */
  | "keymap"
  /** The other shortcut. */
  | "same";

/** Keys that may be a shortcut alone: they type nothing and games rarely use them. */
const LONE_OK = /^(F([1-9]|1[0-9]|2[0-4])|Pause|ScrollLock)$/;

const sc = (mods: string, code: string): Shortcut => ({
  ctrl: mods.includes("c"),
  alt: mods.includes("a"),
  shift: mods.includes("s"),
  meta: mods.includes("m"),
  code,
});

/** The app's and the system's shortcuts a stream shortcut must not take. */
export const APP_SHORTCUTS: readonly Shortcut[] = [
  sc("c", "KeyK"), // command palette
  sc("m", "KeyK"),
  sc("c", "KeyR"), // reload / Collection scan
  sc("m", "KeyR"),
  sc("cs", "KeyR"),
  sc("ms", "KeyR"),
  sc("", "F5"),
  sc("c", "F5"),
  sc("s", "Slash"), // ? shortcuts help
  sc("m", "KeyQ"), // the system's
  sc("m", "KeyW"),
  sc("m", "KeyH"),
  sc("m", "KeyM"),
  sc("m", "Tab"),
  sc("m", "Space"),
  sc("a", "Tab"),
  sc("a", "F4"),
  sc("c", "KeyW"),
  sc("ca", "Delete"),
];

export interface ShortcutContext {
  /** The other shortcut. */
  other: Shortcut;
  /** The keyboard controls (KeyboardEvent.code → pad action). */
  keymap: Readonly<Record<string, unknown>>;
}

export function shortcutProblem(kind: ShortcutKind, s: Shortcut, ctx: ShortcutContext): ShortcutProblem | null {
  const mods = modCount(s);
  if (s.code === null) {
    if (kind === "cycle") return "needs_key";
    if (mods < 2) return "one_modifier";
  } else if (mods === 0) {
    if (s.code === "Escape") return "esc";
    if (!LONE_OK.test(s.code)) return "lone_key";
  }
  // Esc with any modifier stays the app's and the system's: held, it leaves full screen (Shift+Esc
  // too), and Ctrl/Alt/⌘ with Esc are the system's.
  if (s.code === "Escape" && mods > 0) return "app";
  if (s.code !== null && APP_SHORTCUTS.some((a) => sameShortcut(a, s))) return "app";
  if (s.code !== null && mods === 0 && ctx.keymap[s.code] !== undefined) return "keymap";
  if (sameShortcut(s, ctx.other)) return "same";
  return null;
}

/** The text for each problem: the i18n key and its English. */
export const SHORTCUT_PROBLEM_TEXT: Readonly<Record<ShortcutProblem, { key: string; en: string }>> = {
  one_modifier: { key: "rp_sc_one_modifier", en: "Use two modifier keys, or add a key." },
  needs_key: { key: "rp_sc_needs_key", en: "Add a key to the modifiers." },
  esc: { key: "rp_sc_esc", en: "Esc alone can't be used. Turn on Hold Esc to release instead." },
  lone_key: { key: "rp_sc_lone_key", en: "A key alone would type in games. Add Ctrl, Alt or another modifier." },
  app: { key: "rp_sc_app", en: "The app or the system already uses this." },
  keymap: { key: "rp_sc_keymap", en: "This key is in the keyboard controls." },
  same: { key: "rp_sc_same", en: "The other shortcut already uses this." },
};
