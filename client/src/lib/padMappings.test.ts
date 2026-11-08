import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  eightBitDoHow,
  isWeak8BitDo,
  knownProfile,
  padName,
  padPlatform,
  parseGamepadId,
  resetKnownProfiles,
  tableMapping,
} from "./padMappings";
import { PAD_MAPS, PAD_TABLE } from "./padMappingTable";
import { CHECK_STEPS, PadLearner, snapshot } from "./padProfiles";
import { padFromGamepads, PadButton, STANDARD_PROFILE, type PadProfile } from "./remotePlayStream";
import { memoryStorage } from "../test-utils/storage";

/** This device's storage, and the page's address (the engine's store reads it on import). */
function installWindowStub(): Storage {
  const storage = memoryStorage();
  vi.stubGlobal("window", {
    localStorage: storage,
    location: { origin: "http://127.0.0.1:19113", pathname: "/", href: "http://127.0.0.1:19113/" },
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => true,
  });
  return storage;
}

const UA = {
  mac: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
  macFirefox: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0",
  win: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
  winFirefox: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0",
  linux: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
  android: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
  iphone: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1",
  ipad: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
};

/** A controller as a browser gives it: raw buttons by index (1 = pressed), raw axes. */
function pad(o: { id: string; mapping?: string; buttons?: number[]; nButtons?: number; axes: number[]; index?: number }): Gamepad {
  const n = Math.max(o.nButtons ?? 17, ...(o.buttons ?? []).map((b) => b + 1));
  const buttons = Array.from({ length: n }, (_, i) => {
    const v = o.buttons?.includes(i) ? 1 : 0;
    return { pressed: v > 0.5, touched: v > 0, value: v };
  });
  return {
    id: o.id,
    index: o.index ?? 0,
    connected: true,
    mapping: (o.mapping ?? "") as GamepadMappingType,
    axes: o.axes,
    buttons,
    timestamp: 0,
  } as unknown as Gamepad;
}

/** A hat axis at rest, and at a direction (sevenths from -1, clockwise from up). */
const HAT_REST = 9 / 7;
const hat = (at: number) => -1 + (at * 2) / 7;

const play = (g: Gamepad | Gamepad[], prof: (g: Gamepad) => PadProfile) => padFromGamepads(Array.isArray(g) ? g : [g], prof);

const known = (platform: "m" | "w" | "l") => (g: Gamepad) => {
  const p = knownProfile(g, platform);
  if (!p) throw new Error(`no table profile for ${g.id}`);
  return p;
};

beforeEach(() => resetKnownProfiles());

describe("parseGamepadId", () => {
  it.each([
    ["Chrome, not mapped", "8BitDo Pro 2 (Vendor: 2dc8 Product: 6006)", "2dc8", "6006", "8BitDo Pro 2"],
    ["Chrome, mapped", "DualSense Wireless Controller (STANDARD GAMEPAD Vendor: 054c Product: 0ce6)", "054c", "0ce6", "DualSense Wireless Controller"],
    ["Firefox", "054c-0ce6-DualSense Wireless Controller", "054c", "0ce6", "DualSense Wireless Controller"],
    ["Firefox, older Windows (unpadded)", "54c-ce6-Wireless Controller", "054c", "0ce6", "Wireless Controller"],
    ["Safari macOS HID (unpadded)", "79-6-USB Gamepad", "0079", "0006", "USB Gamepad"],
    ["Safari, upper case", "2DC8-6006-8BitDo Pro 2", "2dc8", "6006", "8BitDo Pro 2"],
  ])("%s", (_, id, vendor, product, name) => {
    expect(parseGamepadId(id)).toEqual({ vendor, product, name });
  });

  it.each([
    ["Chrome Windows XInput", "Xbox 360 Controller (XInput STANDARD GAMEPAD)"],
    ["Chrome macOS GameController", "DualSense Wireless Controller (STANDARD GAMEPAD)"],
    ["Safari GameController", "DualSense Wireless Controller Extended Gamepad"],
    ["Firefox Windows XInput", "xinput"],
  ])("%s: no numbers", (_, id) => {
    expect(parseGamepadId(id)).toBeNull();
  });

  it("names a pad without the numbers", () => {
    expect(padName("8BitDo Pro 2 (Vendor: 2dc8 Product: 6006)")).toBe("8BitDo Pro 2");
    expect(padName("054c-0ce6-DualSense Wireless Controller")).toBe("DualSense Wireless Controller");
    expect(padName("Xbox 360 Controller (XInput STANDARD GAMEPAD)")).toBe("Xbox 360 Controller");
  });
});

describe("padPlatform", () => {
  it.each([
    [UA.mac, "m"],
    [UA.macFirefox, "m"],
    [UA.win, "w"],
    [UA.winFirefox, "w"],
    [UA.linux, "l"],
    [UA.android, null],
    [UA.iphone, null],
  ])("%s", (userAgent, want) => {
    expect(padPlatform({ userAgent, maxTouchPoints: 0 })).toBe(want);
  });
  it("an iPad asking for the desktop site is not a Mac", () => {
    expect(padPlatform({ userAgent: UA.ipad, maxTouchPoints: 5 })).toBeNull();
  });
});

describe("the table", () => {
  it("is small and every entry points at a mapping", () => {
    const keys = Object.keys(PAD_TABLE);
    expect(keys.length).toBeGreaterThan(100);
    expect(keys.length).toBeLessThan(1000);
    for (const k of keys) {
      expect(k).toMatch(/^[wml][0-9a-f]{8}$/);
      expect(PAD_MAPS[PAD_TABLE[k]]).toBeTypeOf("string");
    }
  });
  it("serves Windows from the macOS line", () => {
    const ident = { vendor: "2dc8", product: "6006", name: "8BitDo Pro 2" };
    expect(tableMapping(ident, "w")).toBe(tableMapping(ident, "m"));
    expect(tableMapping(ident, null)).toBeNull();
  });
});

describe("a standard pad", () => {
  it("is left alone", () => {
    const g = pad({ id: "Xbox Wireless Controller (STANDARD GAMEPAD Vendor: 045e Product: 0b13)", mapping: "standard", buttons: [0], axes: [0.5, 0, 0, 0] });
    expect(knownProfile(g, "m")).toBeNull();
    // Even a pad the table knows, once the browser lays it out itself.
    const ds = pad({ id: "DualSense Wireless Controller (STANDARD GAMEPAD Vendor: 054c Product: 0ce6)", mapping: "standard", axes: [0, 0, 0, 0] });
    expect(knownProfile(ds, "w")).toBeNull();
    expect(play(g, () => STANDARD_PROFILE).buttons).toBe(PadButton.CROSS);
  });
});

describe("8BitDo Pro 2 in D-input mode", () => {
  // Chrome on macOS and Windows: axes by HID usage (X 0, Y 1, Z 2, Rz 5), the triggers on
  // Accelerator/Brake in the gaps (R2 3, L2 4, resting at -1), the hat at 9. Buttons by label:
  // A b0 (right, Nintendo layout), B b1 (bottom), X b3 (top), Y b4 (left).
  const rest = () => [0, 0, 0, -1, -1, 0, 0, 0, 0, HAT_REST];
  const id = "8BitDo Pro 2 (Vendor: 2dc8 Product: 6006)";

  it("maps by position: the bottom button (B) is ✕, the right one (A) ○", () => {
    expect(play(pad({ id, axes: rest(), buttons: [1] }), known("m")).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ id, axes: rest(), buttons: [0] }), known("m")).buttons).toBe(PadButton.CIRCLE);
    expect(play(pad({ id, axes: rest(), buttons: [4] }), known("m")).buttons).toBe(PadButton.SQUARE);
    expect(play(pad({ id, axes: rest(), buttons: [3] }), known("m")).buttons).toBe(PadButton.TRIANGLE);
    expect(play(pad({ id, axes: rest(), buttons: [6, 7] }), known("w")).buttons).toBe(PadButton.L1 | PadButton.R1);
    expect(play(pad({ id, axes: rest(), buttons: [12] }), known("m")).buttons).toBe(PadButton.PS);
  });

  it("reads the d-pad from the hat", () => {
    knownProfile(pad({ id, axes: rest() }), "m");
    const at = (h: number) => {
      const a = rest();
      a[9] = hat(h);
      return play(pad({ id, axes: a }), known("m")).buttons;
    };
    expect(at(0)).toBe(PadButton.UP);
    expect(at(2)).toBe(PadButton.RIGHT);
    expect(at(4)).toBe(PadButton.DOWN);
    expect(at(6)).toBe(PadButton.LEFT);
    expect(at(1)).toBe(PadButton.UP | PadButton.RIGHT);
  });

  it("reads the triggers from their axes and the right stick from Z and Rz", () => {
    const a = rest();
    a[4] = 1; // Brake: L2 all the way
    a[3] = 0; // Accelerator: R2 half way
    a[5] = 1; // Rz: right stick down
    a[2] = -1; // Z: right stick left
    const p = play(pad({ id, axes: a }), known("m"));
    expect(p.l2).toBe(255);
    expect(p.r2).toBe(128);
    expect(p.ry).toBe(32767);
    expect(p.rx).toBe(-32767);
    expect(p.buttons).toBe(0);
  });

  it("waits for the hat to rest before working the layout out", () => {
    const a = rest();
    a[9] = hat(0);
    expect(knownProfile(pad({ id, axes: a }), "m")).toBeNull();
    expect(knownProfile(pad({ id, axes: rest() }), "m")).not.toBeNull();
  });

  it("is an 8BitDo in a weak mode, with its own way to X-input", () => {
    expect(isWeak8BitDo(pad({ id, axes: rest() }))).toBe(true);
    expect(isWeak8BitDo(pad({ id: "8BitDo Pro 2 (STANDARD GAMEPAD Vendor: 2dc8 Product: 6006)", mapping: "standard", axes: [] }))).toBe(false);
    expect(eightBitDoHow(id)).toBe("switch");
    expect(eightBitDoHow("2dc8-6001-8BitDo SN30 Pro")).toBe("startx");
    expect(eightBitDoHow("8BitDo Ultimate 2C (Vendor: 2dc8 Product: 301d)")).toBe("receiver");
    expect(eightBitDoHow("2dc8-9020-8BitDo Micro")).toBe("none");
    expect(eightBitDoHow("2dc8-ffff-8BitDo Something New")).toBe("manual");
  });
});

describe("DualSense in Firefox before its own mapping (non-standard)", () => {
  // Firefox counts axes in order: X Y Z Rx Ry Rz, then the hat; buttons by HID usage:
  // □ b0, ✕ b1, ○ b2, △ b3, L1 b4, R1 b5, L2 b6, R2 b7, Create b8, Options b9, L3 b10, R3 b11, PS b12, touchpad b13.
  const id = "054c-0ce6-DualSense Wireless Controller";
  const rest = () => [0, 0, 0, -1, -1, 0, HAT_REST];
  it("maps the buttons, the PS button and the touchpad click", () => {
    expect(play(pad({ id, axes: rest(), buttons: [1] }), known("w")).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ id, axes: rest(), buttons: [0] }), known("w")).buttons).toBe(PadButton.SQUARE);
    expect(play(pad({ id, axes: rest(), buttons: [12, 13] }), known("m")).buttons).toBe(PadButton.PS | PadButton.TOUCHPAD);
  });
  it("puts the right stick on Z and Rz and the triggers on Rx and Ry", () => {
    knownProfile(pad({ id, axes: rest() }), "w");
    const a = rest();
    a[5] = -1;
    a[3] = 1;
    a[6] = hat(4);
    const p = play(pad({ id, axes: a }), known("w"));
    expect(p.ry).toBe(-32767);
    expect(p.l2).toBe(255);
    expect(p.buttons).toBe(PadButton.DOWN);
  });
});

describe("Switch Pro Controller, non-standard (Safari's HID reading)", () => {
  // Raw buttons by position: b0 bottom (B), b1 right (A), b2 left (Y), b3 top (X).
  const id = "57e-2009-Pro Controller";
  it("plays the bottom button as ✕", () => {
    const rest = [0, 0, 0, 0, HAT_REST];
    expect(play(pad({ id, axes: rest, buttons: [0] }), known("m")).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ id, axes: rest, buttons: [1] }), known("m")).buttons).toBe(PadButton.CIRCLE);
    expect(play(pad({ id, axes: rest, buttons: [3] }), known("m")).buttons).toBe(PadButton.TRIANGLE);
  });
});

describe("Joy-Con pair, each a pad of its own", () => {
  // Held sideways, each Joy-Con's hat is its stick; Chrome on Windows puts the hat at 9.
  const left = (h: number, buttons: number[] = []) =>
    pad({ id: "Joy-Con (L) (Vendor: 057e Product: 2006)", axes: [0, 0, 0, 0, 0, 0, 0, 0, 0, h], buttons, index: 0 });
  const right = (h: number, buttons: number[] = []) =>
    pad({ id: "Joy-Con (R) (Vendor: 057e Product: 2007)", axes: [0, 0, 0, 0, 0, 0, 0, 0, 0, h], buttons, index: 1 });
  it("plays as one: each one's stick and buttons", () => {
    knownProfile(left(HAT_REST), "w");
    knownProfile(right(HAT_REST), "w");
    const p = play([left(hat(2), [0]), right(HAT_REST, [1])], known("w"));
    expect(p.lx).toBe(32767);
    expect(p.buttons).toBe(PadButton.CROSS | PadButton.CIRCLE);
    const q = play([left(HAT_REST), right(hat(4))], known("w"));
    expect(q.ly).toBe(32767);
  });
});

describe("Logitech F310 in D mode (non-standard)", () => {
  // Compact axes (Firefox's reading): X Y Z Rz, hat after. Buttons: X b0 (left), A b1 (bottom), B b2 (right), Y b3 (top).
  const id = "046d-c216-Logitech Dual Action";
  it("maps the face buttons and the hat", () => {
    const rest = [0, 0, 0, 0, HAT_REST];
    expect(play(pad({ id, axes: rest, buttons: [1] }), known("w")).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ id, axes: rest, buttons: [0] }), known("w")).buttons).toBe(PadButton.SQUARE);
    expect(play(pad({ id, axes: [0, 0, 0, 0, hat(6)] }), known("w")).buttons).toBe(PadButton.LEFT);
    expect(play(pad({ id, axes: [0, 0, 0, 0.9, HAT_REST] }), known("w")).ry).toBeGreaterThan(29000);
  });
});

describe("generic DragonRise USB Gamepad (0079:0006)", () => {
  // Chrome on Windows (the only Chrome that leaves it unmapped): axes by usage, right stick Z/Rz
  // (2, 5), the hat at 9. Buttons: △ b0, ○ b1, ✕ b2, □ b3.
  const id = "USB Gamepad (Vendor: 0079 Product: 0006)";
  const rest = () => [0, 0, 0, 0, 0, 0, 0, 0, 0, HAT_REST];
  it("maps the face buttons, the hat and the right stick", () => {
    expect(play(pad({ id, axes: rest(), buttons: [2] }), known("w")).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ id, axes: rest(), buttons: [0] }), known("w")).buttons).toBe(PadButton.TRIANGLE);
    const a = rest();
    a[5] = 1;
    a[9] = hat(0);
    const p = play(pad({ id, axes: a }), known("w"));
    expect(p.ry).toBe(32767);
    expect(p.buttons).toBe(PadButton.UP);
  });
  it("on Linux: the hat as two axes after the others", () => {
    // Linux joydev: X Y Z Rx Ry, then the hat's x and y.
    const g = (hx: number, hy: number, buttons: number[] = []) => pad({ id, axes: [0, 0, 0, 0, 0, hx, hy], buttons });
    expect(play(g(0, -1), known("l")).buttons).toBe(PadButton.UP);
    expect(play(g(1, 0), known("l")).buttons).toBe(PadButton.RIGHT);
    expect(play(g(0, 0, [2]), known("l")).buttons).toBe(PadButton.CROSS);
  });
});

describe("a pad in no list", () => {
  it("gets no table profile (it plays in the standard order, and the check is offered)", () => {
    const g = pad({ id: "Mystery Pad (Vendor: 1234 Product: 5678)", axes: [0, 0, 0, 0] });
    expect(knownProfile(g, "m")).toBeNull();
    expect(knownProfile(g, null)).toBeNull();
  });
});

describe("profileFor, padKind and the check", () => {
  const ua = (userAgent: string) => vi.stubGlobal("navigator", { userAgent, maxTouchPoints: 0 });
  afterEach(() => vi.unstubAllGlobals());

  it("orders: saved, standard, table, guess", async () => {
    installWindowStub();
    ua(UA.mac);
    vi.resetModules();
    const { padKind, profileFor } = await import("./padProfiles");
    const { STANDARD_PROFILE: STD } = await import("./remotePlayStream");
    const std = pad({ id: "Xbox (STANDARD GAMEPAD Vendor: 045e Product: 0b13)", mapping: "standard", axes: [0, 0, 0, 0] });
    expect(padKind(std)).toBe("standard");
    expect(profileFor(std)).toBe(STD);
    const eight = pad({ id: "8BitDo Pro 2 (Vendor: 2dc8 Product: 6006)", axes: [0, 0, 0, -1, -1, 0, 0, 0, 0, HAT_REST] });
    expect(padKind(eight)).toBe("known");
    const unknown = pad({ id: "Mystery Pad (Vendor: 1234 Product: 5678)", axes: [0, 0, 0, 0] });
    expect(padKind(unknown)).toBe("guess");
    expect(play(pad({ id: unknown.id, axes: [0, 0, 0, 0], buttons: [0] }), profileFor).buttons).toBe(PadButton.CROSS);
  });

  it("learns a pad, keeps it across a reload, and redoes it", async () => {
    const storage = installWindowStub();
    ua(UA.mac);
    vi.resetModules();
    let mod = await import("./padProfiles");
    const id = "Mystery Pad (Vendor: 1234 Product: 5678)";
    const rest = { id, axes: [0, 0, 0, 0], nButtons: 12 };
    // This pad's ✕ is raw button 3, ○ 2, □ 1, △ 0; the rest of the steps are skipped.
    const learn = (order: number[]) => {
      const l = new mod.PadLearner(CHECK_STEPS);
      l.feed(snapshot(pad(rest)));
      for (const b of order) {
        expect(l.feed(snapshot(pad({ ...rest, buttons: [b] })))).toBe(true);
        l.feed(snapshot(pad(rest)));
      }
      while (!l.done) l.skip();
      return l.profile(mod.defaultProfileFor(pad(rest)));
    };
    mod.saveProfile(id, learn([3, 2, 1, 0]));
    expect(mod.padKind(pad(rest))).toBe("saved");
    expect(play(pad({ ...rest, buttons: [3] }), mod.profileFor).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ ...rest, buttons: [0] }), mod.profileFor).buttons).toBe(PadButton.TRIANGLE);
    // Skipped steps keep what the pad had (standard order), except buttons now taken.
    expect(play(pad({ ...rest, buttons: [4] }), mod.profileFor).buttons).toBe(PadButton.L1);
    expect(storage.getItem("ps5upload.rp_pad_profiles.v1")).toContain(id);

    // A reload: a fresh module reads it back from storage.
    vi.resetModules();
    mod = await import("./padProfiles");
    expect(mod.padKind(pad(rest))).toBe("saved");
    expect(play(pad({ ...rest, buttons: [3] }), mod.profileFor).buttons).toBe(PadButton.CROSS);

    // Redo: a different answer replaces it.
    mod.saveProfile(id, learn([0, 1, 2, 3]));
    expect(play(pad({ ...rest, buttons: [0] }), mod.profileFor).buttons).toBe(PadButton.CROSS);
    expect(play(pad({ ...rest, buttons: [3] }), mod.profileFor).buttons).toBe(PadButton.TRIANGLE);

    // Reset: back to the guess.
    mod.saveProfile(id, null);
    expect(mod.padKind(pad(rest))).toBe("guess");
  });

  it("waits for everything to be let go between steps, and goes back", () => {
    const l = new PadLearner(CHECK_STEPS);
    const r = { id: "x", axes: [0, 0, 0, 0] };
    l.feed(snapshot(pad(r)));
    expect(l.feed(snapshot(pad({ ...r, buttons: [5] })))).toBe(true);
    // Still held: not the next step's answer.
    expect(l.feed(snapshot(pad({ ...r, buttons: [5] })))).toBe(false);
    expect(l.step).toBe(1);
    l.back();
    expect(l.step).toBe(0);
  });
});
