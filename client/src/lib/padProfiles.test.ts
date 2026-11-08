import { describe, expect, it, vi } from "vitest";
import { padFromGamepads, PadButton, STANDARD_PROFILE } from "./remotePlayStream";
import { addExtra, applyStep, blankProfile, detectAxis, detectPress, loadProfiles, MAP_STEPS, withStandardGaps } from "./padProfiles";
import * as storage from "./safeStorage";
import { installWindowStub } from "../test-utils/storage";

/** A controller as the Gamepad API gives it. */
function pad(over: { buttons?: number[]; axes?: number[]; mapping?: string; id?: string } = {}): Gamepad {
  const buttons = (over.buttons ?? new Array(17).fill(0)).map((v) => ({ pressed: v > 0.5, touched: v > 0, value: v }));
  return {
    id: over.id ?? "Test pad",
    index: 0,
    connected: true,
    mapping: (over.mapping ?? "standard") as GamepadMappingType,
    axes: over.axes ?? [0, 0, 0, 0],
    buttons,
    timestamp: 0,
  } as unknown as Gamepad;
}
const press = (i: number, n = 17) => {
  const b = new Array(n).fill(0);
  b[i] = 1;
  return b;
};
/** Several buttons held at once, in the standard 17. */
const held = (...which: number[]) => {
  const b = new Array(17).fill(0);
  for (const i of which) b[i] = 1;
  return b;
};

describe("padFromGamepads", () => {
  it("reads the standard layout", () => {
    const p = padFromGamepads([pad({ buttons: press(0), axes: [1, 0, 0, -1] })]);
    expect(p.buttons).toBe(PadButton.CROSS);
    expect(p.lx).toBe(32767);
    expect(p.ry).toBe(-32767);
  });
  it("plays a controller the browser does not recognize, in the standard order", () => {
    // Before, a non-standard controller did nothing at all.
    const p = padFromGamepads([pad({ mapping: "", buttons: press(1) })]);
    expect(p.buttons).toBe(PadButton.CIRCLE);
  });
  it("reads a controller through its own profile", () => {
    const prof = { ...STANDARD_PROFILE, buttons: [[PadButton.CROSS, { b: 3 }]] as Array<[number, { b: number }]> };
    const p = padFromGamepads([pad({ buttons: press(3) })], () => prof);
    expect(p.buttons).toBe(PadButton.CROSS);
  });
  it("reads a trigger reported as an axis that rests at -1", () => {
    const prof = { ...STANDARD_PROFILE, l2: { a: 4, d: 1 as const, rest: -1 } };
    expect(padFromGamepads([pad({ axes: [0, 0, 0, 0, -1] })], () => prof).l2).toBe(0);
    expect(padFromGamepads([pad({ axes: [0, 0, 0, 0, 0] })], () => prof).l2).toBe(128);
    expect(padFromGamepads([pad({ axes: [0, 0, 0, 0, 1] })], () => prof).l2).toBe(255);
  });
  it("sends a half-pressed trigger as half, though the browser already calls it pressed", () => {
    // Browsers set `pressed` on an analog trigger from about a tenth of the way: read as pressed,
    // every pull reached the PS5 as 255, a trigger that worked like a button.
    const half = (v: number) => {
      const g = pad();
      const buttons = [...g.buttons];
      buttons[6] = { pressed: true, touched: true, value: v } as GamepadButton;
      buttons[7] = { pressed: true, touched: true, value: v } as GamepadButton;
      return { ...g, buttons } as Gamepad;
    };
    expect(padFromGamepads([half(0.5)]).l2).toBe(128);
    expect(padFromGamepads([half(0.5)]).r2).toBe(128);
    expect(padFromGamepads([half(0.2)]).r2).toBe(51);
    expect(padFromGamepads([half(1)]).l2).toBe(255);
    // A trigger mapped by hand to a button keeps its pull too.
    const mapped = { ...STANDARD_PROFILE, l2: { b: 7 }, r2: { b: 6 } };
    expect(padFromGamepads([half(0.3)], () => mapped).l2).toBe(77);
  });
  it("sends a button that says pressed with no value as all the way", () => {
    const g = pad();
    const buttons = [...g.buttons];
    buttons[7] = { pressed: true, touched: true, value: 0 } as GamepadButton;
    expect(padFromGamepads([{ ...g, buttons } as Gamepad]).r2).toBe(255);
  });
});

describe("mapping a controller", () => {
  const rest = { buttons: new Array(20).fill(0), axes: [0, 0, 0, 0, -1, -1] };
  it("finds the button pressed", () => {
    expect(detectPress(rest, { ...rest, buttons: press(18, 20) })).toEqual({ b: 18 });
    expect(detectPress(rest, rest)).toBeNull();
  });
  it("finds a trigger on an axis and keeps where it rests", () => {
    expect(detectPress(rest, { ...rest, axes: [0, 0, 0, 0, 0.9, -1] })).toEqual({ a: 4, d: 1, rest: -1 });
  });
  it("prefers a button over a stick nudged at the same time", () => {
    expect(detectPress(rest, { buttons: press(2, 20), axes: [0.7, 0, 0, 0, -1, -1] })).toEqual({ b: 2 });
  });
  it("finds a stick's axis and which way it reads", () => {
    expect(detectAxis(rest, { ...rest, axes: [0, 0, 0.9, 0, -1, -1] })).toEqual({ a: 2 });
    expect(detectAxis(rest, { ...rest, axes: [0, -0.9, 0, 0, -1, -1] })).toEqual({ a: 1, inv: true });
    expect(detectAxis(rest, { ...rest, axes: [0.3, 0, 0, 0, -1, -1] })).toBeNull();
  });
  it("builds a profile step by step; skipped steps keep the standard layout", () => {
    let prof = blankProfile();
    prof = applyStep(prof, MAP_STEPS[0], { b: 5 }); // ✕ on button 5
    prof = applyStep(prof, MAP_STEPS[0], { b: 6 }); // asked again: replaced, not doubled
    expect(prof.buttons).toEqual([[PadButton.CROSS, { b: 6 }]]);
    const full = withStandardGaps(prof);
    expect(full.buttons.filter(([bit]) => bit === PadButton.CROSS)).toEqual([[PadButton.CROSS, { b: 6 }]]);
    expect(full.buttons.some(([bit]) => bit === PadButton.CIRCLE)).toBe(true);
    expect(full.lx).toEqual({ a: 0 });
    // ✕ on button 5 (R1's in the standard layout, skipped here): it presses ✕ only, not R1 too.
    const five = withStandardGaps(applyStep(blankProfile(), MAP_STEPS[0], { b: 5 }));
    expect(padFromGamepads([pad({ buttons: press(5) })], () => five).buttons).toBe(PadButton.CROSS);
  });
  it("gives an extra button (a back paddle) any PS5 button", () => {
    const prof = addExtra(STANDARD_PROFILE, PadButton.L3, { b: 17 });
    const p = padFromGamepads([pad({ buttons: press(17, 19) })], () => prof);
    expect(p.buttons).toBe(PadButton.L3);
  });
});

describe("a controller forwarded by the Android app", () => {
  it("is offered only while the browser sees no controller", async () => {
    const { gamepadsOf, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    vi.stubGlobal("window", { dispatchEvent: () => true });
    installNativePad();
    const nav = (pads: Array<Gamepad | null>) => ({ getGamepads: () => pads }) as unknown as Navigator;
    expect(gamepadsOf(nav([null]))).toEqual([null]); // nothing forwarded yet
    window.__ps5uNativePad!({ id: "Backbone One", buttons: press(0), axes: [0, 0.5, 0, 0] });
    const [g] = gamepadsOf(nav([null]));
    expect(g?.id).toBe("Backbone One (app)");
    expect(padFromGamepads([g]).buttons).toBe(PadButton.CROSS);
    // The browser sees one itself: that one only, never both.
    const real = pad({ id: "Backbone One" });
    expect(gamepadsOf(nav([real]))).toEqual([real]);
    resetNativePad();
    vi.unstubAllGlobals();
  });

  it("keeps a forwarded trigger's pull (Linux, Windows and Android apps)", async () => {
    const { gamepadsOf, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    vi.stubGlobal("window", { dispatchEvent: () => true });
    installNativePad();
    const nav = { getGamepads: () => [null] } as unknown as Navigator;
    const buttons = new Array(17).fill(0);
    // Past half way the forwarded button reads as pressed: it still sends how far it is pulled.
    buttons[6] = 0.7;
    buttons[7] = 0.3;
    window.__ps5uNativePad!({ id: "DualSense", buttons, axes: [0, 0, 0, 0] });
    const p = padFromGamepads(gamepadsOf(nav));
    expect(p.l2).toBe(179);
    expect(p.r2).toBe(77);
    resetNativePad();
    vi.unstubAllGlobals();
  });

  it("#668: on the Linux app the kernel's reading is the controller, with the browser's rumble", async () => {
    const { gamepadsOf, preferNativePads, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    vi.stubGlobal("window", { dispatchEvent: () => true });
    installNativePad();
    preferNativePads();
    const nav = (pads: Array<Gamepad | null>) => ({ getGamepads: () => pads }) as unknown as Navigator;
    // WebKitGTK with an unmapped pad: no d-pad, and R2 reads as the left stick pushed up.
    const rumble = { playEffect: () => {} };
    const real = {
      ...pad({ id: "Sony Interactive Entertainment DualSense Wireless Controller", buttons: held(0), axes: [0, -1, 0, 0] }),
      vibrationActuator: rumble,
    } as unknown as Gamepad;
    const send = window.__ps5uNativePad!;
    send({ slot: 0, id: "DualSense Wireless Controller", buttons: held(0, 12, 15), axes: [0, 0, 0, 0] });
    const list = gamepadsOf(nav([null, real]));
    expect(list.filter(Boolean)).toHaveLength(1); // never read twice
    const g = list[1]!;
    expect(padFromGamepads(list).buttons).toBe(PadButton.CROSS | PadButton.UP | PadButton.RIGHT);
    expect(padFromGamepads(list).ly).toBe(0); // the browser's noise is gone
    expect((g as unknown as { vibrationActuator: unknown }).vibrationActuator).toBe(rumble);
    // The same controller whether or not the browser has noticed it yet.
    const before = gamepadsOf(nav([null]))[0]!;
    expect([before.id, before.index]).toEqual([g.id, g.index]);
    // Android and Windows: the browser's pad alone, as before.
    preferNativePads(false);
    expect(gamepadsOf(nav([real]))[0]).toBe(real);
    resetNativePad();
    vi.unstubAllGlobals();
  });

  it("#668: two controllers each find their twin; one the app does not read stays", async () => {
    const { gamepadsOf, preferNativePads, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    vi.stubGlobal("window", { dispatchEvent: () => true });
    installNativePad();
    preferNativePads();
    const rumble = { playEffect: () => {} };
    const xbox = { ...pad({ id: "Xbox Wireless" }), vibrationActuator: rumble } as unknown as Gamepad;
    const other = pad({ id: "Flight Stick", buttons: held(1) });
    const nav = { getGamepads: () => [xbox, pad({ id: "DualSense" }), other] } as unknown as Navigator;
    const send = window.__ps5uNativePad!;
    send({ slot: 0, id: "DualSense", buttons: held(14), axes: [] });
    send({ slot: 1, id: "Xbox Wireless", buttons: held(), axes: [] });
    const list = gamepadsOf(nav);
    expect(list.map((g) => g?.id)).toEqual(["Xbox Wireless (app)", "DualSense (app)", "Flight Stick"]);
    expect((list[0] as unknown as { vibrationActuator: unknown }).vibrationActuator).toBe(rumble);
    expect(padFromGamepads([list[1]]).buttons).toBe(PadButton.LEFT);
    expect(padFromGamepads([list[2]]).buttons).toBe(PadButton.CIRCLE);
    // A controller only the app has read so far is still offered.
    const seenOne = { getGamepads: () => [pad({ id: "Xbox Wireless" })] } as unknown as Navigator;
    expect(gamepadsOf(seenOne).map((g) => g?.id)).toEqual(["Xbox Wireless (app)", "DualSense (app)"]);
    resetNativePad();
    vi.unstubAllGlobals();
  });

  it("the Linux app's controllers: several by slot, and gone when unplugged", async () => {
    const { gamepadsOf, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    const fired: string[] = [];
    vi.stubGlobal("window", { dispatchEvent: (e: Event) => fired.push(e.type) });
    installNativePad();
    const nav = { getGamepads: () => [null] } as unknown as Navigator;
    const send = window.__ps5uNativePad!;
    send({ slot: 1, id: "Xbox", connected: true, buttons: press(1), axes: [0, 0, 0, 0] });
    send({ slot: 0, id: "DualSense", connected: true, buttons: press(0), axes: [0, 0, 0, 0] });
    send({ slot: 0, id: "DualSense", connected: true, buttons: press(3), axes: [0, 0, 0, 0] });
    expect(fired).toEqual(["gamepadconnected", "gamepadconnected"]); // once each, not per change
    expect(gamepadsOf(nav).map((g) => [g?.id, g?.index])).toEqual([
      ["DualSense (app)", 3],
      ["Xbox (app)", 4],
    ]);
    expect(padFromGamepads(gamepadsOf(nav)).buttons).toBe(PadButton.TRIANGLE | PadButton.CIRCLE);
    send({ slot: 0, id: "DualSense", connected: false, buttons: [], axes: [] });
    expect(fired[fired.length - 1]).toBe("gamepaddisconnected");
    expect(gamepadsOf(nav).map((g) => g?.id)).toEqual(["Xbox (app)"]);
    send({ slot: 1, id: "Xbox", connected: false, buttons: [], axes: [] });
    expect(gamepadsOf(nav)).toEqual([null]);
    // A second unplug of the same slot fires nothing.
    send({ slot: 1, id: "Xbox", connected: false, buttons: [], axes: [] });
    expect(fired.filter((t) => t === "gamepaddisconnected")).toHaveLength(2);
    resetNativePad();
    vi.unstubAllGlobals();
  });
});

describe("loadProfiles", () => {
  it("keeps good profiles and drops damaged ones", () => {
    const good = { buttons: [[1, { b: 0 }]], l2: null, r2: { b: 7 }, lx: null, ly: null, rx: null, ry: null };
    const get = vi.spyOn(storage, "safeGetItem").mockReturnValue(
      JSON.stringify({ a: good, b: { buttons: "x" }, c: 7, d: { ...good, buttons: [[1]] } }),
    );
    expect(Object.keys(loadProfiles())).toEqual(["a"]);
    get.mockReturnValue("[]");
    expect(loadProfiles()).toEqual({});
    get.mockRestore();
  });
});

describe("a mapping saved before the app read controllers itself (#668)", () => {
  const paddles = blankProfile();

  it("applies under the app's name, and is copied there without losing the old entry", async () => {
    installWindowStub();
    const mod = await import("./padProfiles");
    mod.saveProfile("Sony DualSense", paddles);
    const applied = mod.profileFor(pad({ id: "Sony DualSense (app)" }));
    expect(applied).toEqual(paddles);
    expect(Object.keys(mod.loadProfiles()).sort()).toEqual(["Sony DualSense", "Sony DualSense (app)"]);
    vi.unstubAllGlobals();
  });

  it("does not guess between two saved controllers", async () => {
    installWindowStub();
    const mod = await import("./padProfiles");
    mod.saveProfile("0ce6 Pad", paddles);
    mod.saveProfile("0df2 Pad", paddles);
    expect(Object.keys(mod.loadProfiles())).toHaveLength(2);
    mod.profileFor(pad({ id: "Pad (app)" }));
    expect(Object.keys(mod.loadProfiles())).toHaveLength(2);
    vi.unstubAllGlobals();
  });
});
