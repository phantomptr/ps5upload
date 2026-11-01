// Native keyboard & mouse for Remote Play: this computer's keyboard and mouse as the PS5's own
// (for games and apps that take a keyboard and mouse), next to the controller emulation the stream
// has always had (lib/remotePlayStream.ts padFromKeys, lib/gameControls.ts mouse aim).
//
// The page sends the engine (rp_stream_api.rs / vinput_api.rs):
//   {"kbm": {"on", "mouse", "keyboard"}}   which virtual devices it wants, again every 500 ms as
//                                          the heartbeat; on: false turns it off
//   binary [8][16 bytes]                    one report: buttons, HID modifiers, dx, dy (i16 LE),
//                                          wheel, hwheel (i8), 8 HID usage IDs held
// and gets {"kbm": {"on", "mouse", "keyboard", "err", "reason"}}: what the PS5 sees.
// The 16-byte report is the helper's (payload/include/vinput.h); 17 bytes a report on the socket
// against ~80 for the same thing as JSON, at up to 250 a second while the mouse moves.

/** How the keyboard and mouse play a game. */
export type KbmMode = "controller" | "native" | "mixed";

export const KBM_MODES: readonly KbmMode[] = ["controller", "native", "mixed"];

/** Page → engine binary kind for a report. */
export const KIND_KBM = 8;
export const REPORT_LEN = 16;
export const MAX_KEYS = 8;
/** Mouse movement is gathered this long and sent as one report (keys and buttons go at once). */
export const MOVE_BATCH_MS = 4;
/** The heartbeat: the engine (and the console) let go of everything 2 s after the last one. */
export const HEARTBEAT_MS = 500;

/** Mouse buttons in a report (MouseEvent.button 0..4 → HID order). */
export const BUTTON_BITS: Record<number, number> = { 0: 0x01, 2: 0x02, 1: 0x04, 3: 0x08, 4: 0x10 };

/** HID modifier bits by KeyboardEvent.code. */
export const MODIFIER_BITS: Record<string, number> = {
  ControlLeft: 0x01,
  ShiftLeft: 0x02,
  AltLeft: 0x04,
  MetaLeft: 0x08,
  OSLeft: 0x08,
  ControlRight: 0x10,
  ShiftRight: 0x20,
  AltRight: 0x40,
  MetaRight: 0x80,
  OSRight: 0x80,
};

function table(): Record<string, number> {
  const t: Record<string, number> = {};
  for (let i = 0; i < 26; i++) t[`Key${String.fromCharCode(65 + i)}`] = 0x04 + i;
  for (let i = 1; i <= 9; i++) t[`Digit${i}`] = 0x1e + i - 1;
  t.Digit0 = 0x27;
  Object.assign(t, {
    Enter: 0x28,
    Escape: 0x29,
    Backspace: 0x2a,
    Tab: 0x2b,
    Space: 0x2c,
    Minus: 0x2d,
    Equal: 0x2e,
    BracketLeft: 0x2f,
    BracketRight: 0x30,
    Backslash: 0x31,
    IntlHash: 0x32,
    Semicolon: 0x33,
    Quote: 0x34,
    Backquote: 0x35,
    Comma: 0x36,
    Period: 0x37,
    Slash: 0x38,
    CapsLock: 0x39,
    PrintScreen: 0x46,
    ScrollLock: 0x47,
    Pause: 0x48,
    Insert: 0x49,
    Home: 0x4a,
    PageUp: 0x4b,
    Delete: 0x4c,
    End: 0x4d,
    PageDown: 0x4e,
    ArrowRight: 0x4f,
    ArrowLeft: 0x50,
    ArrowDown: 0x51,
    ArrowUp: 0x52,
    NumLock: 0x53,
    NumpadDivide: 0x54,
    NumpadMultiply: 0x55,
    NumpadSubtract: 0x56,
    NumpadAdd: 0x57,
    NumpadEnter: 0x58,
    NumpadDecimal: 0x63,
    IntlBackslash: 0x64,
    ContextMenu: 0x65,
    NumpadEqual: 0x67,
    IntlRo: 0x87,
    KanaMode: 0x88,
    IntlYen: 0x89,
    Convert: 0x8a,
    NonConvert: 0x8b,
    Lang1: 0x90,
    Lang2: 0x91,
  });
  for (let i = 1; i <= 9; i++) t[`Numpad${i}`] = 0x59 + i - 1;
  t.Numpad0 = 0x62;
  for (let i = 1; i <= 12; i++) t[`F${i}`] = 0x3a + i - 1;
  for (let i = 13; i <= 24; i++) t[`F${i}`] = 0x68 + i - 13;
  return t;
}

/** USB HID keyboard usage IDs by KeyboardEvent.code: the key's place, whatever the layout. */
export const HID_USAGE: Readonly<Record<string, number>> = table();

/** The input state a report carries. */
export interface KbmState {
  buttons: number;
  mods: number;
  /** HID usages held, in the order they went down (at most MAX_KEYS). */
  keys: number[];
}

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, Math.trunc(v)));

/** One report as the socket carries it: `[8][16 bytes]`. */
export function encodeReport(
  s: KbmState,
  dx = 0,
  dy = 0,
  wheel = 0,
  hwheel = 0,
): Uint8Array<ArrayBuffer> {
  const b = new Uint8Array(1 + REPORT_LEN);
  const v = new DataView(b.buffer);
  b[0] = KIND_KBM;
  b[1] = s.buttons & 0xff;
  b[2] = s.mods & 0xff;
  v.setInt16(3, clamp(dx, -32768, 32767), true);
  v.setInt16(5, clamp(dy, -32768, 32767), true);
  v.setInt8(7, clamp(wheel, -128, 127));
  v.setInt8(8, clamp(hwheel, -128, 127));
  s.keys.slice(0, MAX_KEYS).forEach((k, i) => (b[9 + i] = k & 0xff));
  return b;
}

/** A report back from its 16 bytes (a phone's, forwarded by the engine as hex). Null when malformed. */
export function decodeReport(
  bytes: Uint8Array,
): (KbmState & { dx: number; dy: number; wheel: number; hwheel: number }) | null {
  if (bytes.length !== REPORT_LEN) return null;
  const v = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    buttons: bytes[0],
    mods: bytes[1],
    dx: v.getInt16(2, true),
    dy: v.getInt16(4, true),
    wheel: v.getInt8(6),
    hwheel: v.getInt8(7),
    keys: [...bytes.slice(8)].filter((k) => k !== 0),
  };
}

/** Bytes from lower-case hex ("0a1b…"); null when it is not hex of whole bytes. */
export function fromHex(hex: string): Uint8Array | null {
  if (typeof hex !== "string" || hex.length % 2 || !/^[0-9a-f]*$/i.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** What is held by the page and every phone together: buttons and modifiers from all, keys in
 *  the order they came (the page's first), at most MAX_KEYS. */
export function mergeHeld(states: Iterable<KbmState>): KbmState {
  const m: KbmState = { buttons: 0, mods: 0, keys: [] };
  for (const s of states) {
    m.buttons |= s.buttons;
    m.mods |= s.mods;
    for (const k of s.keys) if (!m.keys.includes(k) && m.keys.length < MAX_KEYS) m.keys.push(k);
  }
  return m;
}

/** The union of what each input wants (the mode, the touchpad, phones). */
export function unionCaps(...all: KbmCaps[]): KbmCaps {
  return { mouse: all.some((c) => c.mouse), keyboard: all.some((c) => c.keyboard) };
}

/** Wheel notches from a WheelEvent's delta (pixels, lines or pages), with what is left over.
 *  Up (away) is positive, as HID has it: the opposite of deltaY. */
export function wheelNotches(carry: number, delta: number, mode: number): { notches: number; carry: number } {
  const unit = mode === 1 ? 3 : mode === 2 ? 1 : 100; // 3 lines or 100 px a notch
  const total = carry + delta / unit;
  const notches = Math.trunc(total);
  return { notches: -notches, carry: total - notches };
}

// ── Routing: which sink each kind of input goes to (one at a time, never two) ──────────────────

export interface KbmCaps {
  mouse: boolean;
  keyboard: boolean;
}

/** The mode actually in use: a remembered native choice the console cannot take yet plays as
 *  Controller (the choice is kept for when it can). Native needs both devices, Mixed the mouse. */
export function effectiveMode(remembered: KbmMode, caps: KbmCaps | null): KbmMode {
  if (remembered === "native") return caps?.mouse && caps.keyboard ? "native" : "controller";
  if (remembered === "mixed") return caps?.mouse ? "mixed" : "controller";
  return "controller";
}

/** Why a mode cannot be picked on this console, or null when it can. */
export function modeUnavailable(mode: KbmMode, caps: KbmCaps | null): "no_mouse" | "no_keyboard" | null {
  if (mode === "controller") return null;
  if (!caps?.mouse) return "no_mouse";
  if (mode === "native" && !caps.keyboard) return "no_keyboard";
  return null;
}

/** pad: the emulated controller (key map, mouse aim); native: the PS5's virtual device; none. */
export type Sink = "pad" | "native" | "none";

export interface Route {
  keys: Sink;
  /** Movement, buttons and wheel go together. */
  mouse: Sink;
}

export interface RouteContext {
  /** This page shows the session (spectators never send input). */
  owner: boolean;
  /** Settings is open over the stream, or a text box of the app has the focus. */
  appFirst: boolean;
  /** The mouse is captured by the picture (the browser's lock or the desktop app's grab). */
  pointerLocked: boolean;
  /** The PS5 has the keyboard and mouse now: the picture was clicked and nothing released them
   *  since (a capture, or a click where nothing can capture). Released, keys stay on this
   *  computer in Keyboard & mouse and Mixed; Controller sends its keys as it always has. */
  captured?: boolean;
  /** Neither capture works here (a browser that refuses the lock) and the pointer is over the
   *  picture: its movement is sent as it is. */
  hoverFallback?: boolean;
}

/** Where keys and the mouse go now, in `mode` (already effective). */
export function routeFor(mode: KbmMode, ctx: RouteContext): Route {
  if (!ctx.owner || ctx.appFirst) return { keys: "none", mouse: "none" };
  const mouseNative = mode !== "controller";
  const held = ctx.captured ?? true;
  return {
    keys: mode === "controller" ? "pad" : !held ? "none" : mode === "native" ? "native" : "pad",
    // The mouse reaches the PS5 only while the picture has it captured (or, where nothing can
    // capture it, while it is over the picture). Mouse aim (pad) reads the capture itself.
    mouse: mouseNative ? (ctx.pointerLocked || (ctx.hoverFallback && held) ? "native" : "none") : "pad",
  };
}

/** The devices the engine is asked for in `mode`. */
export function devicesFor(mode: KbmMode): KbmCaps {
  return { mouse: mode !== "controller", keyboard: mode === "native" };
}

/** The next mode for the cycle hotkey, skipping ones the console cannot take. */
export function nextMode(cur: KbmMode, caps: KbmCaps | null): KbmMode {
  for (let i = 1; i <= KBM_MODES.length; i++) {
    const m = KBM_MODES[(KBM_MODES.indexOf(cur) + i) % KBM_MODES.length];
    if (!modeUnavailable(m, caps)) return m;
  }
  return "controller";
}

/** The cycle hotkey: Ctrl+Alt+M (app-reserved, never sent to the game; Ctrl+K is the app's command
 *  palette, and no key of the stream's key list uses Ctrl or Alt). */
export function isCycleHotkey(e: Pick<KeyboardEvent, "code" | "ctrlKey" | "altKey" | "metaKey" | "shiftKey">): boolean {
  return e.code === "KeyM" && e.ctrlKey && e.altKey && !e.metaKey && !e.shiftKey;
}

// ── The sender: what is held, batched motion, release-all and the heartbeat ──────────────────

export interface KbmSenderDeps {
  /** Sends a report or a control message; false when the socket is down. */
  send: (msg: { kbm: unknown } | Uint8Array<ArrayBuffer>) => boolean;
  setTimer?: (fn: () => void, ms: number) => number;
  clearTimer?: (id: number) => void;
  setRepeat?: (fn: () => void, ms: number) => number;
  clearRepeat?: (id: number) => void;
}

/** The keys, buttons and motion going to the PS5's virtual devices, for one socket. */
export class KbmSender {
  private st: KbmState = { buttons: 0, mods: 0, keys: [] };
  private dx = 0;
  private dy = 0;
  private wheel = 0;
  private hwheel = 0;
  private wheelCarry = 0;
  private hwheelCarry = 0;
  private moveTimer: number | null = null;
  private hb: number | null = null;
  private want: KbmCaps = { mouse: false, keyboard: false };
  /** What each phone holds (the phone link's Touchpad & keyboard), merged into every report. */
  private guests = new Map<number, KbmState>();
  private readonly d: Required<KbmSenderDeps>;

  constructor(deps: KbmSenderDeps) {
    this.d = {
      setTimer: (fn, ms) => window.setTimeout(fn, ms),
      clearTimer: (id) => window.clearTimeout(id),
      setRepeat: (fn, ms) => window.setInterval(fn, ms),
      clearRepeat: (id) => window.clearInterval(id),
      ...deps,
    };
  }

  get state(): Readonly<KbmState> {
    return this.st;
  }

  /** Either Shift held by this sender (not a phone's). */
  shiftHeld(): boolean {
    return (this.st.mods & (MODIFIER_BITS.ShiftLeft | MODIFIER_BITS.ShiftRight)) !== 0;
  }

  /** Anything held. */
  get holding(): boolean {
    return this.st.buttons !== 0 || this.st.mods !== 0 || this.st.keys.length > 0;
  }

  /** Which devices are wanted (none = off). Sends at once, then every HEARTBEAT_MS. */
  setWant(want: KbmCaps): void {
    const on = want.mouse || want.keyboard;
    const changed = want.mouse !== this.want.mouse || want.keyboard !== this.want.keyboard;
    this.want = { ...want };
    if (!changed) return;
    if (this.hb !== null) this.d.clearRepeat(this.hb);
    this.hb = null;
    this.beat();
    if (on) this.hb = this.d.setRepeat(() => this.beat(), HEARTBEAT_MS);
  }

  get wanted(): Readonly<KbmCaps> {
    return this.want;
  }

  private beat(): void {
    const on = this.want.mouse || this.want.keyboard;
    this.d.send({ kbm: on ? { on: true, mouse: this.want.mouse, keyboard: this.want.keyboard } : { on: false } });
  }

  /** A key went down or up (KeyboardEvent.code). False for a key with no HID usage. */
  key(code: string, down: boolean): boolean {
    const mod = MODIFIER_BITS[code];
    if (mod) {
      const mods = down ? this.st.mods | mod : this.st.mods & ~mod;
      if (mods !== this.st.mods) {
        this.st = { ...this.st, mods };
        this.flush();
      }
      return true;
    }
    const u = HID_USAGE[code];
    if (!u) return false;
    const has = this.st.keys.includes(u);
    if (down && !has && this.st.keys.length < MAX_KEYS) {
      this.st = { ...this.st, keys: [...this.st.keys, u] };
      this.flush();
    } else if (!down && has) {
      this.st = { ...this.st, keys: this.st.keys.filter((k) => k !== u) };
      this.flush();
    }
    return true;
  }

  /** A mouse button (MouseEvent.button). */
  button(button: number, down: boolean): void {
    const bit = BUTTON_BITS[button];
    if (!bit) return;
    const buttons = down ? this.st.buttons | bit : this.st.buttons & ~bit;
    if (buttons === this.st.buttons) return;
    this.st = { ...this.st, buttons };
    this.flush();
  }

  /** A phone's held keys and buttons (null: it let go or left). Sent at once, merged. */
  guest(id: number, state: KbmState | null): void {
    if (state) this.guests.set(id, { buttons: state.buttons, mods: state.mods, keys: state.keys.slice(0, MAX_KEYS) });
    else if (!this.guests.delete(id)) return;
    this.flush();
  }

  /** A phone's motion, merged into the page's. */
  guestMotion(dx: number, dy: number, wheel: number, hwheel: number): void {
    this.dx += dx;
    this.dy += dy;
    this.wheel += wheel;
    this.hwheel += hwheel;
    if (dx || dy || wheel || hwheel) this.later();
  }

  get guestIds(): number[] {
    return [...this.guests.keys()];
  }

  /** Mouse movement (movementX/Y), sent within MOVE_BATCH_MS. */
  move(dx: number, dy: number): void {
    this.dx += dx;
    this.dy += dy;
    this.later();
  }

  /** A wheel turn (WheelEvent deltaY / deltaX and deltaMode). */
  scroll(deltaY: number, deltaX: number, mode: number): void {
    const v = wheelNotches(this.wheelCarry, deltaY, mode);
    const h = wheelNotches(this.hwheelCarry, deltaX, mode);
    this.wheelCarry = v.carry;
    this.hwheelCarry = h.carry;
    this.wheel += v.notches;
    this.hwheel -= h.notches; // right is positive
    if (this.wheel || this.hwheel) this.later();
  }

  private later(): void {
    if (this.moveTimer === null) this.moveTimer = this.d.setTimer(() => this.flush(), MOVE_BATCH_MS);
  }

  /** Sends the state now, with the motion gathered so far (split when it is too big for one). */
  flush(): void {
    if (this.moveTimer !== null) this.d.clearTimer(this.moveTimer);
    this.moveTimer = null;
    const held = this.guests.size ? mergeHeld([this.st, ...this.guests.values()]) : this.st;
    do {
      const dx = clamp(this.dx, -32768, 32767);
      const dy = clamp(this.dy, -32768, 32767);
      const w = clamp(this.wheel, -128, 127);
      const hw = clamp(this.hwheel, -128, 127);
      this.dx -= dx;
      this.dy -= dy;
      this.wheel -= w;
      this.hwheel -= hw;
      this.d.send(encodeReport(held, dx, dy, w, hw));
    } while (this.dx || this.dy || this.wheel || this.hwheel);
  }

  /** Lets go of every key and button of this page (motion not yet sent is dropped). What a phone
   *  holds stays held: it lets go on its own. */
  releaseAll(): void {
    this.dx = this.dy = this.wheel = this.hwheel = 0;
    this.wheelCarry = this.hwheelCarry = 0;
    const was = this.holding;
    this.st = { buttons: 0, mods: 0, keys: [] };
    if (this.moveTimer !== null) this.d.clearTimer(this.moveTimer);
    this.moveTimer = null;
    if (was) this.d.send(encodeReport(this.guests.size ? mergeHeld(this.guests.values()) : this.st));
  }

  /** Lets go of everything (phones' too), turns it off and stops the heartbeat. */
  dispose(): void {
    this.guests.clear();
    this.releaseAll();
    this.setWant({ mouse: false, keyboard: false });
  }
}

/** The engine's `{"kbm": …}` answer. */
export interface KbmStatus {
  on: boolean;
  mouse: boolean;
  keyboard: boolean;
  err: string | null;
  reason: string | null;
}

export function readKbmStatus(v: unknown): KbmStatus | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const str = (x: unknown) => (typeof x === "string" ? x : null);
  return { on: o.on === true, mouse: o.mouse === true, keyboard: o.keyboard === true, err: str(o.err), reason: str(o.reason) };
}

// ── The stream bar's mode button: what it shows, and why it cannot switch ──────────────────────

/** What the console said about its devices (state/kbm.ts KbmConsoleCaps), as far as the button
 *  needs it. `companion` is the on-console keyboard & mouse process: idle, starting, running,
 *  stopped or unavailable (absent from older engines). */
export interface KbmCapsInfo extends KbmCaps {
  supported?: boolean;
  reason?: string | null;
  companion?: string | null;
}

/** Why no keyboard & mouse mode can be picked on this console. */
export type KbmBlock = "helper_old" | "needs_loader" | "companion_failed" | "unavailable";

/** The text for each KbmBlock: the i18n key and its English. */
export const KBM_BLOCK_TEXT: Readonly<Record<KbmBlock, { key: string; en: string }>> = {
  helper_old: { key: "rp_kbm_helper_old", en: "Update the PS5 helper to use a keyboard and mouse." },
  needs_loader: { key: "rp_kbm_needs_loader", en: "Needs a payload loader on port 9021" },
  companion_failed: {
    key: "rp_kbm_companion_failed",
    en: "The keyboard & mouse helper could not start on the PS5",
  },
  unavailable: { key: "rp_kbm_unavailable", en: "Native keyboard & mouse isn't available on this console yet." },
};

/** Why `caps` rules out every mode but Controller (no caps yet, or a reason the engine gave). */
export function kbmBlockReason(caps: KbmCapsInfo | null): KbmBlock {
  if (caps && caps.supported === false) return "helper_old";
  if (caps?.reason === "helper_old") return "helper_old";
  if (caps?.reason === "vinput_needs_loader") return "needs_loader";
  if (caps?.reason === "vinput_companion_failed") return "companion_failed";
  return "unavailable";
}

/** Null when some keyboard & mouse mode can be picked on this console, else why not. */
export function kbmBlocked(caps: KbmCapsInfo | null): KbmBlock | null {
  const open = KBM_MODES.some((m) => m !== "controller" && !modeUnavailable(m, caps));
  return open ? null : kbmBlockReason(caps);
}

/** The console's keyboard & mouse helper is on its way up: the devices are not there yet. */
export function kbmStarting(caps: KbmCapsInfo | null, status: KbmStatus | null): boolean {
  return (!!status?.on && status.err === "vinput_starting") || caps?.companion === "starting";
}

/** The bar button: the icon (the mode, or a spinner), and the reason it is off. */
export interface KbmButtonView {
  mode: KbmMode;
  icon: KbmMode | "spinner";
  blocked: KbmBlock | null;
}

export function kbmButtonView(mode: KbmMode, caps: KbmCapsInfo | null, status: KbmStatus | null): KbmButtonView {
  const blocked = kbmBlocked(caps);
  return { mode, icon: kbmStarting(caps, status) ? "spinner" : mode, blocked };
}

// ── Phones' touchpad & keyboard (the phone link), forwarded by the engine ───────────────────────

/** `{"pad_kbm": …}` from the engine: a phone's devices wanted, a report (hex), or it left. */
export interface PadKbmMsg {
  id: number;
  on?: boolean;
  mouse?: boolean;
  keyboard?: boolean;
  r?: string;
  left?: boolean;
}

export function readPadKbm(v: unknown): PadKbmMsg | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  if (typeof o.id !== "number" || !Number.isFinite(o.id)) return null;
  return {
    id: o.id,
    on: typeof o.on === "boolean" ? o.on : undefined,
    mouse: o.mouse === true,
    keyboard: o.keyboard === true,
    r: typeof o.r === "string" ? o.r : undefined,
    left: o.left === true,
  };
}

const padKbmSubs = new Set<(m: PadKbmMsg) => void>();

/** Hears phones' keyboard & mouse messages (the stream screen's sender merges them). */
export function onPadKbm(fn: (m: PadKbmMsg) => void): () => void {
  padKbmSubs.add(fn);
  return () => {
    padKbmSubs.delete(fn);
  };
}

/** The stream got a `{"pad_kbm": …}` message. */
export function emitPadKbm(v: unknown): void {
  const m = readPadKbm(v);
  if (!m) return;
  for (const fn of [...padKbmSubs]) fn(m);
}
