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
// A WebKitGTK with libmanette does see the controller, but cannot always read it (#668).
// libmanette up to 0.2.7 (the AppImage's, Ubuntu 24.04's) has no DualSense mapping, and for an
// unmapped pad WebKit drops the hat, so the d-pad never reaches the page, and folds other axes
// into the sticks, so L2 and R2 move them; it still reports "standard", so the page cannot tell.
// So on the Linux app the kernel's reading is the controller whenever the app has one, and the
// browser's pad only lends its rumble.

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

/** Prefer the forwarded controllers to the browser's (the Linux app). */
let preferNative = false;

/** Prefer the forwarded controllers (once, from the Linux app's start-up). */
export function preferNativePads(on = true): void {
  preferNative = on;
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
function nativeGamepad(s: Required<Omit<NativeState, "connected">>, rumbleFrom?: Gamepad): Gamepad {
  return {
    id: `${s.id} (app)`,
    index: 3 + s.slot,
    connected: true,
    mapping: "standard",
    timestamp: 0,
    axes: s.axes,
    buttons: s.buttons.map((v) => ({ pressed: v > 0.5, touched: v > 0, value: v })),
    vibrationActuator: (rumbleFrom as (Gamepad & { vibrationActuator?: unknown }) | undefined)?.vibrationActuator ?? null,
    hapticActuators: (rumbleFrom as (Gamepad & { hapticActuators?: unknown }) | undefined)?.hapticActuators ?? [],
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

/** The controllers `nav` sees, or the forwarded ones when it sees none. On the Linux app the
 *  forwarded ones come first: each takes the place of its twin in the browser's list, and a
 *  browser pad the app does not read stays. The result's ids and indexes do not change when the
 *  browser notices a pad (it does on the pad's first input), so nothing jumps in mid-game. */
export function gamepadsOf(nav: Navigator | undefined): Array<Gamepad | null> {
  const own = nav?.getGamepads?.() ?? [];
  if (pads.size === 0) return [...own];
  const native = () => [...pads.values()].sort((a, b) => a.slot - b.slot);
  if (!own.some((g) => !!g?.connected)) return native().map((n) => nativeGamepad(n));
  if (!preferNative) return [...own];
  const twin = twins(own);
  const taken = new Set([...twin.values()].map((n) => n.slot));
  const out: Array<Gamepad | null> = own.map((g) => {
    const t = g ? twin.get(g) : undefined;
    return t && g ? nativeGamepad(t, g) : g;
  });
  for (const n of native()) if (!taken.has(n.slot)) out.push(nativeGamepad(n));
  return out;
}

/** For tests: forget the forwarded controllers. */
export function resetNativePad(): void {
  pads.clear();
  preferNative = false;
}
