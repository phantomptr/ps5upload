import { afterEach, describe, expect, it, vi } from "vitest";

import { PadButton } from "../lib/remotePlayStream";
import { rpInput } from "../state/remotePlay";
import { GUEST_KEYMAP, JoinSession, devicePad, guestHeld, isPadKey, pageStats } from "./session";
import { PictureStats } from "../pad/watchPlayer";

describe("pageStats (#720)", () => {
  it("says nothing before the first picture, unless the picture failed: then the decoder, codec and why", () => {
    const st = new PictureStats("webcodecs", () => 0);
    expect(pageStats(null, null)).toBeNull();
    expect(pageStats(st, null)).toBeNull();
    st.codec = "avc1.640028";
    expect(pageStats(st, "decode")).toMatchObject({ frames: 0, decoder: "webcodecs", codec: "avc1.640028", problem: "decode" });
    st.drawn();
    const s = pageStats(st, null);
    expect(s).toMatchObject({ frames: 1, codec: "avc1.640028" });
    expect(s).not.toHaveProperty("problem");
  });

  it("carries how the sound went once it started", () => {
    const st = new PictureStats("webcodecs", () => 0);
    st.drawn();
    const sound = { snd: "wasm", snd_decoded: 900, snd_concealed: 4, snd_underruns: 12, snd_dropped: 30 };
    expect(pageStats(st, null, sound)).toMatchObject({ frames: 1, ...sound });
    expect(pageStats(st, "decode", { snd: "mse", snd_jumps: 3 })).toMatchObject({ problem: "decode", snd: "mse", snd_jumps: 3 });
    // No picture report, no sound report either (the host keeps "never reported its picture").
    expect(pageStats(new PictureStats("webcodecs", () => 0), null, sound)).toBeNull();
  });
});

/** A socket that never connects (the session's keyboard does not need one). */
class QuietSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  readyState = 0;
  binaryType = "blob";
  onopen = null;
  onclose = null;
  onerror = null;
  onmessage = null;
  send() {}
  close() {}
}

/** KeyboardEvent for the node runner: a cancelable event with a code. */
class KeyEvent extends Event {
  ctrlKey = false;
  metaKey = false;
  altKey = false;
  constructor(
    type: string,
    readonly code: string,
  ) {
    super(type, { cancelable: true });
  }
}

/** A window and document to listen on (node has EventTarget, not the DOM). */
function stubDom(): EventTarget {
  const win = Object.assign(new EventTarget(), {
    requestAnimationFrame: () => 0,
    cancelAnimationFrame: () => {},
  });
  const doc = Object.assign(new EventTarget(), { visibilityState: "visible" });
  vi.stubGlobal("window", win);
  vi.stubGlobal("document", doc);
  return win;
}

function gamepad(pressed: number[]): Gamepad {
  return {
    id: "Xbox Wireless Controller (STANDARD GAMEPAD)",
    index: 0,
    connected: true,
    mapping: "standard",
    timestamp: 0,
    axes: [0, 0, 0, 0],
    buttons: Array.from({ length: 17 }, (_, i) => ({ pressed: pressed.includes(i), touched: false, value: pressed.includes(i) ? 1 : 0 })),
  } as unknown as Gamepad;
}

afterEach(() => {
  guestHeld.clear();
  vi.unstubAllGlobals();
});

describe("guest keyboard (#654)", () => {
  it("uses Remote Play's keys, but leaves Escape to the browser", () => {
    expect(GUEST_KEYMAP.Escape).toBeUndefined();
    expect(GUEST_KEYMAP.KeyW).toEqual({ stick: "ly", dir: -1 });
    expect(GUEST_KEYMAP.Enter).toEqual({ button: PadButton.CROSS });
  });

  it("drives the pad from held keys with no controller", () => {
    vi.stubGlobal("navigator", { getGamepads: () => [] });
    guestHeld.add("KeyW").add("Enter").add("Digit3");
    const p = devicePad();
    expect(p.buttons & PadButton.CROSS).toBeTruthy();
    expect(p.ly).toBe(-32767);
    expect(p.r2).toBe(255);
  });

  it("merges the keyboard with a controller", () => {
    // Standard mapping: button 1 is Circle.
    vi.stubGlobal("navigator", { getGamepads: () => [gamepad([1])] });
    guestHeld.add("KeyQ").add("KeyD");
    const p = devicePad();
    expect(p.buttons & PadButton.CIRCLE).toBeTruthy();
    expect(p.buttons & PadButton.SQUARE).toBeTruthy();
    expect(p.lx).toBe(32767);
  });

  it("an Escape held does nothing", () => {
    vi.stubGlobal("navigator", { getGamepads: () => [] });
    guestHeld.add("Escape");
    expect(devicePad().buttons).toBe(0);
  });

  it("is a pad key only for mapped keys outside text fields and shortcuts", () => {
    const body = { tagName: "BODY" } as unknown as EventTarget;
    const input = { tagName: "INPUT" } as unknown as EventTarget;
    const area = { tagName: "TEXTAREA" } as unknown as EventTarget;
    const editable = { tagName: "DIV", isContentEditable: true } as unknown as EventTarget;
    const base = { ctrlKey: false, metaKey: false, altKey: false };
    expect(isPadKey({ ...base, code: "KeyW", target: body })).toBe(true);
    expect(isPadKey({ ...base, code: "KeyW", target: input })).toBe(false);
    expect(isPadKey({ ...base, code: "Space", target: area })).toBe(false);
    expect(isPadKey({ ...base, code: "KeyE", target: editable })).toBe(false);
    expect(isPadKey({ ...base, code: "Escape", target: body })).toBe(false);
    expect(isPadKey({ ...base, code: "KeyP", target: body })).toBe(false);
    expect(isPadKey({ ...base, metaKey: true, code: "KeyC", target: body })).toBe(false);
  });

  it("leaves a focused button's keys to it, and gives a keyboard the way to Leave", () => {
    const body = { tagName: "BODY" } as unknown as EventTarget;
    const button = { tagName: "BUTTON" } as unknown as EventTarget;
    const link = { tagName: "A" } as unknown as EventTarget;
    const roled = { tagName: "DIV", getAttribute: () => "button" } as unknown as EventTarget;
    const base = { ctrlKey: false, metaKey: false, altKey: false };
    for (const target of [button, link, roled]) {
      expect(isPadKey({ ...base, code: "Enter", target })).toBe(false);
      expect(isPadKey({ ...base, code: "Space", target })).toBe(false);
      expect(isPadKey({ ...base, code: "Tab", target })).toBe(false);
    }
    // Tab is the game's Options; Shift+Tab moves to the page's own buttons.
    expect(isPadKey({ ...base, code: "Tab", target: body })).toBe(true);
    expect(isPadKey({ ...base, shiftKey: true, code: "Tab", target: body })).toBe(false);
  });

  it("holds keys while joined, blocks their default, and lets go on blur", () => {
    const window = stubDom();
    vi.stubGlobal("WebSocket", QuietSocket);
    const s = new JoinSession({ url: "ws://127.0.0.1:1/join", invite: null, name: "Pal", host: null });
    s.start();
    try {
      // Not joined yet: the keyboard is the page's.
      const early = new KeyEvent("keydown", "KeyW");
      window.dispatchEvent(early);
      expect(early.defaultPrevented).toBe(false);
      expect(guestHeld.size).toBe(0);

      (s.client as unknown as { stage: { kind: string } }).stage = { kind: "joined" };
      const down = new KeyEvent("keydown", "KeyW");
      window.dispatchEvent(down);
      expect(down.defaultPrevented).toBe(true);
      expect([...guestHeld]).toEqual(["KeyW"]);

      const esc = new KeyEvent("keydown", "Escape");
      window.dispatchEvent(esc);
      expect(esc.defaultPrevented).toBe(false);

      window.dispatchEvent(new KeyEvent("keyup", "KeyW"));
      expect(guestHeld.size).toBe(0);

      window.dispatchEvent(new KeyEvent("keydown", "KeyA"));
      window.dispatchEvent(new Event("blur"));
      expect(guestHeld.size).toBe(0);
    } finally {
      s.stop();
      rpInput.touchPad = { buttons: 0, l2: 0, r2: 0, lx: 0, ly: 0, rx: 0, ry: 0 };
    }
    // Stopped: no longer listening.
    (s.client as unknown as { stage: { kind: string } }).stage = { kind: "joined" };
    window.dispatchEvent(new KeyEvent("keydown", "KeyW"));
    expect(guestHeld.size).toBe(0);
  });
});
