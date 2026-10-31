// A controller the Android app forwards from the system, for when its WebView's Gamepad API does
// not deliver it (scripts/release/android-postinit-patch.sh, step 9). MainActivity calls
// window.__ps5uNativePad({id, buttons, axes}) with the W3C standard layout on every change; it
// is offered as one more Gamepad only while the browser itself sees no controller, so a
// controller the WebView does deliver is never read twice.
//
// The Linux desktop app does the same for a WebKitGTK without libmanette, whose Gamepad API sees
// no controller at all (lib/nativePadDesktop.ts, src-tauri commands/pad_native.rs). It forwards
// each controller under its own `slot`, and `connected: false` when one is unplugged.

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

/** The controllers `nav` sees, or the forwarded ones when it sees none. */
export function gamepadsOf(nav: Navigator | undefined): Array<Gamepad | null> {
  const own = nav?.getGamepads?.() ?? [];
  if (own.some((g) => !!g?.connected) || pads.size === 0) return [...own];
  return [...pads.values()].sort((a, b) => a.slot - b.slot).map(nativeGamepad);
}

/** For tests: forget the forwarded controllers. */
export function resetNativePad(): void {
  pads.clear();
}
