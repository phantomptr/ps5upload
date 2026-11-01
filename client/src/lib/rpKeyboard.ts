// Typing into the PS5's on-screen keyboard during Remote Play.
//
// When a game or the PS5 opens its keyboard, the engine says so (`{"keyboard": {"open": true,
// "text": "…"}}`, engine rp_keyboard.rs) and the stream shows a text box over the picture. What
// is typed goes to the console as the whole text, a short while after the last key (`{
// "keyboard_text": "…"}`); Enter keeps it (`{"keyboard_accept": true}`), Esc cancels (`{
// "keyboard_reject": true}`). The console closing its keyboard (`{"keyboard": {"open": false}}`)
// closes the box. An engine without this never sends `keyboard`, so the box never shows.

/** What the engine says about the console's keyboard. */
export interface RpKeyboardMsg {
  open?: boolean;
  text?: string;
}

/** The console's keyboard is open: the text in its field, and which opening this is (each one
 *  starts the box afresh). */
export interface RpKeyboardState {
  text: string;
  seq: number;
}

/** What the page sends about the keyboard. */
export type RpKeyboardSend =
  | { keyboard_text: string }
  | { keyboard_accept: true }
  | { keyboard_reject: true };

/** Wait after the last key before the text goes to the console. */
export const KEYBOARD_DEBOUNCE_MS = 150;

let opened = 0;

/** The keyboard state after an engine message. A text-only message (the console's answer to
 *  ours) updates the state but is not a new opening, so a box being typed in is left alone. */
export function nextKeyboard(
  prev: RpKeyboardState | null,
  m: RpKeyboardMsg,
): RpKeyboardState | null {
  if (!m || typeof m !== "object") return prev;
  if (m.open === false) return null;
  const text = typeof m.text === "string" ? m.text : "";
  if (m.open === true) return { text, seq: ++opened };
  if (prev && typeof m.text === "string") return { ...prev, text };
  return prev;
}

/** Sends the text a short while after the last change; `flush` sends what is waiting now. */
export class KeyboardTyper {
  private timer: ReturnType<typeof setTimeout> | null = null;
  private waiting: string | null = null;
  private sent: string;

  constructor(
    private readonly send: (text: string) => void,
    initial = "",
    private readonly ms = KEYBOARD_DEBOUNCE_MS,
  ) {
    this.sent = initial;
  }

  type(text: string): void {
    this.waiting = text;
    if (this.timer) clearTimeout(this.timer);
    this.timer = setTimeout(() => this.flush(), this.ms);
  }

  flush(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    const t = this.waiting;
    this.waiting = null;
    if (t === null || t === this.sent) return;
    this.sent = t;
    this.send(t);
  }

  /** Drop what is waiting (the box was cancelled or closed). */
  cancel(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.waiting = null;
  }
}
