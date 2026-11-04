// A controller the Android app forwards from the system, for when its WebView's Gamepad API does
// not deliver it (scripts/release/android-postinit-patch.sh, step 9). MainActivity calls
// window.__ps5uNativePad({id, buttons, axes}) with the W3C standard layout on every change; it
// is offered as one more Gamepad only while the browser itself sees no controller, so a
// controller the WebView does deliver is never read twice.
//
// The Linux desktop app does the same for a WebKitGTK without libmanette, whose Gamepad API sees
// no controller at all (lib/nativePadDesktop.ts, src-tauri commands/pad_native.rs). It forwards
// each controller under its own `slot`, and `connected: false` when one is unplugged.
//
// A WebKitGTK with libmanette does see the controller, but not always its d-pad (#668): the
// kernel reports a DualSense's d-pad as a hat, and WebKit reads only buttons and analog axes, so
// unless libmanette's mapping database knows that exact device the d-pad never reaches the page
// while everything else works. There the browser's pad stays the controller (it has the rumble,
// and any mapping the user saved under its name), and the forwarded pad only adds its d-pad.

interface NativeState {
  id: string;
  buttons: number[];
  axes: number[];
  /** Which controller, when several are forwarded (0 when not said). */
  slot?: number;
  /** False: the controller is gone. */
  connected?: boolean;
}

/** The forwarded controllers by slot. */
const pads = new Map<number, Required<Omit<NativeState, "connected">>>();

/** The standard layout's d-pad: up, down, left, right. */
const DPAD = [12, 13, 14, 15];

/** Fill in the d-pad of a controller the browser sees from the forwarded one (the Linux app). */
let dpadFromNative = false;

/** Told once, the first time the forwarded pad supplies a d-pad press the browser's lacks. */
let onFilled: ((browserPad: string) => void) | null = null;

/** Turn on filling in the d-pad (once, from the Linux app's start-up). */
export function fillDpadFromNative(on = true, filled?: (browserPad: string) => void): void {
  dpadFromNative = on;
  onFilled = filled ?? null;
}

declare global {
  interface Window {
    __ps5uNativePad?: (s: NativeState) => void;
  }
}

/** Install the receiver (once, at start-up). */
export function installNativePad(): void {
  if (typeof window === "undefined" || window.__ps5uNativePad) return;
  window.__ps5uNativePad = (s) => {
    const slot = Math.max(0, Math.floor(Number(s?.slot ?? 0)) || 0);
    // What the browser fires for a controller it sees (the on-screen controller hides on it)
    // and for one that goes away.
    if (s?.connected === false) {
      if (pads.delete(slot)) window.dispatchEvent(new Event("gamepaddisconnected"));
      return;
    }
    const first = !pads.has(slot);
    pads.set(slot, {
      slot,
      id: String(s?.id ?? "Controller"),
      buttons: Array.isArray(s?.buttons) ? s.buttons.map(Number) : [],
      axes: Array.isArray(s?.axes) ? s.axes.map(Number) : [],
    });
    if (first) window.dispatchEvent(new Event("gamepadconnected"));
  };
}

/** A forwarded controller as a Gamepad (slot 0 at index 3, as the Android app's always was). */
function nativeGamepad(s: Required<Omit<NativeState, "connected">>): Gamepad {
  return {
    id: `${s.id} (app)`,
    index: 3 + s.slot,
    connected: true,
    mapping: "standard",
    timestamp: 0,
    axes: s.axes,
    buttons: s.buttons.map((v) => ({ pressed: v > 0.5, touched: v > 0, value: v })),
    vibrationActuator: null,
    hapticActuators: [],
  } as unknown as Gamepad;
}

/** Each connected browser pad's forwarded twin: the unused one of the same name, else the next
 *  unused one (WebKit and the kernel can name a controller differently). */
function twins(own: ReadonlyArray<Gamepad | null>): Map<Gamepad, Required<Omit<NativeState, "connected">>> {
  const free = [...pads.values()].sort((a, b) => a.slot - b.slot);
  const out = new Map<Gamepad, Required<Omit<NativeState, "connected">>>();
  const connected = own.filter((g): g is Gamepad => !!g?.connected);
  const name = (id: string) => id.toLowerCase();
  for (const g of connected) {
    const at = free.findIndex((n) => name(g.id).includes(name(n.id)) || name(n.id).includes(name(g.id)));
    if (at >= 0) out.set(g, free.splice(at, 1)[0]);
  }
  for (const g of connected) {
    if (!out.has(g) && free.length > 0) out.set(g, free.shift()!);
  }
  return out;
}

/** `g` with its twin's d-pad added. `g` itself while the twin's d-pad is at rest, so the rumble
 *  and everything else are the browser's own and nothing is allocated on a poll. */
function withNativeDpad(g: Gamepad, twin: Required<Omit<NativeState, "connected">>): Gamepad {
  if (!DPAD.some((i) => (twin.buttons[i] ?? 0) > 0.5)) return g;
  if (onFilled && !DPAD.some((i) => g.buttons[i]?.pressed)) {
    onFilled(g.id);
    onFilled = null;
  }
  const buttons = Array.from(g.buttons, (b) => ({ pressed: b.pressed, touched: b.touched, value: b.value }));
  for (const i of DPAD) {
    while (buttons.length <= i) buttons.push({ pressed: false, touched: false, value: 0 });
    if ((twin.buttons[i] ?? 0) > 0.5) buttons[i] = { pressed: true, touched: true, value: 1 };
  }
  // A plain copy: a Gamepad's fields are getters that refuse any other `this`.
  return {
    id: g.id,
    index: g.index,
    connected: g.connected,
    mapping: g.mapping,
    timestamp: g.timestamp,
    axes: g.axes,
    buttons,
    vibrationActuator: (g as Gamepad & { vibrationActuator?: unknown }).vibrationActuator ?? null,
    hapticActuators: (g as Gamepad & { hapticActuators?: unknown }).hapticActuators ?? [],
  } as unknown as Gamepad;
}

/** The controllers `nav` sees (on the Linux app with the d-pad the kernel saw added), or the
 *  forwarded ones when it sees none. */
export function gamepadsOf(nav: Navigator | undefined): Array<Gamepad | null> {
  const own = nav?.getGamepads?.() ?? [];
  if (pads.size === 0) return [...own];
  if (!own.some((g) => !!g?.connected)) {
    return [...pads.values()].sort((a, b) => a.slot - b.slot).map(nativeGamepad);
  }
  if (!dpadFromNative) return [...own];
  const twin = twins(own);
  return own.map((g) => {
    const t = g ? twin.get(g) : undefined;
    return g && t ? withNativeDpad(g, t) : g;
  });
}

/** For tests: forget the forwarded controllers. */
export function resetNativePad(): void {
  pads.clear();
  dpadFromNative = false;
  onFilled = null;
}
