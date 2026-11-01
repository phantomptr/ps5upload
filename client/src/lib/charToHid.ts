// Typed characters as key presses on the PS5's keyboard (a phone's on-screen keyboard has no key
// positions, only characters). The PS5's keyboard is a US layout here: printable ASCII maps to a
// key (KeyboardEvent.code, see HID_USAGE in kbmInput.ts) and whether Shift is held. Anything else
// (é, 日本) has no key; the PS5's own text box (RpKeyboardBox) takes those.

/** Where the presses go (KbmSender.key, or the phone link's sender). */
export interface KeyTarget {
  key(code: string, down: boolean): unknown;
  /** Shift already held by this sender (the person's own Shift): typing leaves it alone. */
  shiftHeld?(): boolean;
}

export interface KeyPress {
  code: string;
  shift: boolean;
}

const UNSHIFTED: Record<string, string> = {
  " ": "Space",
  "-": "Minus",
  "=": "Equal",
  "[": "BracketLeft",
  "]": "BracketRight",
  "\\": "Backslash",
  ";": "Semicolon",
  "'": "Quote",
  "`": "Backquote",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
  "\n": "Enter",
  "\r": "Enter",
  "\t": "Tab",
};

const SHIFTED: Record<string, string> = {
  "!": "Digit1",
  "@": "Digit2",
  "#": "Digit3",
  $: "Digit4",
  "%": "Digit5",
  "^": "Digit6",
  "&": "Digit7",
  "*": "Digit8",
  "(": "Digit9",
  ")": "Digit0",
  _: "Minus",
  "+": "Equal",
  "{": "BracketLeft",
  "}": "BracketRight",
  "|": "Backslash",
  ":": "Semicolon",
  '"': "Quote",
  "~": "Backquote",
  "<": "Comma",
  ">": "Period",
  "?": "Slash",
};

/** The key for one character, or null when the US keyboard has none. */
export function charToKey(ch: string): KeyPress | null {
  if (ch.length !== 1) return null;
  if (ch >= "a" && ch <= "z") return { code: `Key${ch.toUpperCase()}`, shift: false };
  if (ch >= "A" && ch <= "Z") return { code: `Key${ch}`, shift: true };
  if (ch >= "0" && ch <= "9") return { code: `Digit${ch}`, shift: false };
  if (UNSHIFTED[ch]) return { code: UNSHIFTED[ch], shift: false };
  if (SHIFTED[ch]) return { code: SHIFTED[ch], shift: true };
  return null;
}

/** Keys a phone keyboard (or the hidden input) delivers by name (KeyboardEvent.key). */
const NAMED: Record<string, string> = {
  Backspace: "Backspace",
  Enter: "Enter",
  Tab: "Tab",
  Escape: "Escape",
  Delete: "Delete",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
};

/** The key for a KeyboardEvent.key name that types no character (Backspace, arrows…), or null. */
export function namedKey(key: string): string | null {
  return NAMED[key] ?? null;
}

/** Presses and lets go of each character's key, with Shift where it needs it. Returns the
 *  characters that have no key (not sent). */
export function typeText(sender: KeyTarget, text: string): string {
  let skipped = "";
  for (const ch of text) {
    const k = charToKey(ch);
    if (!k) {
      skipped += ch;
      continue;
    }
    pressKey(sender, k.code, k.shift);
  }
  return skipped;
}

/** One press and release of `code`, with Shift around it when asked. A Shift the sender already
 *  holds already shifts the key, and letting go of it would let go of the person's own Shift. */
export function pressKey(sender: KeyTarget, code: string, shift = false): void {
  const wrap = shift && !sender.shiftHeld?.();
  if (wrap) sender.key("ShiftLeft", true);
  sender.key(code, true);
  sender.key(code, false);
  if (wrap) sender.key("ShiftLeft", false);
}

/**
 * What changed in a text field: how many characters were deleted from the end of what it had,
 * and what was added after them. Phone keyboards change the field (autocorrect, composing a word)
 * rather than sending keys, so the hidden input's value is diffed.
 */
export function textDiff(before: string, after: string): { deleted: number; inserted: string } {
  const a = [...before];
  const b = [...after];
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  return { deleted: a.length - i, inserted: b.slice(i).join("") };
}
