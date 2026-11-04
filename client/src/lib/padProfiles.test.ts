import { describe, expect, it, vi } from "vitest";
import { padFromGamepads, PadButton, STANDARD_PROFILE } from "./remotePlayStream";
import { addExtra, applyStep, blankProfile, detectAxis, detectPress, MAP_STEPS, withStandardGaps } from "./padProfiles";

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

  it("#668: on the Linux app a pad the browser sees gets its d-pad from the kernel's view", async () => {
    const { gamepadsOf, fillDpadFromNative, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    vi.stubGlobal("window", { dispatchEvent: () => true });
    installNativePad();
    const filled: string[] = [];
    fillDpadFromNative(true, (id) => filled.push(id));
    const nav = (pads: Array<Gamepad | null>) => ({ getGamepads: () => pads }) as unknown as Navigator;
    // WebKitGTK with libmanette: sticks and face buttons, no hat.
    const rumble = { playEffect: () => {} };
    const real = {
      ...pad({ id: "Sony Interactive Entertainment DualSense Wireless Controller", buttons: held(0) }),
      vibrationActuator: rumble,
    } as unknown as Gamepad;
    const send = window.__ps5uNativePad!;
    // At rest: the browser's own pad, untouched (its rumble and saved mapping stay).
    send({ slot: 0, id: "DualSense Wireless Controller", buttons: held(), axes: [0, 0, 0, 0] });
    expect(gamepadsOf(nav([real]))[0]).toBe(real);
    // Pressed: the d-pad is added to what the browser reports, nothing is read twice.
    send({ slot: 0, id: "DualSense Wireless Controller", buttons: held(12, 15), axes: [0.9, 0, 0, 0] });
    const [g] = gamepadsOf(nav([real]));
    expect(g?.id).toBe(real.id);
    expect(g?.index).toBe(real.index);
    expect((g as unknown as { vibrationActuator: unknown }).vibrationActuator).toBe(rumble);
    expect(g?.axes).toBe(real.axes);
    expect(padFromGamepads([g]).buttons).toBe(PadButton.CROSS | PadButton.UP | PadButton.RIGHT);
    // Said once for a bug report, not on every poll.
    gamepadsOf(nav([real]));
    expect(filled).toEqual([real.id]);
    // A pad with fewer buttons than the standard layout still takes it.
    const short = pad({ id: "Short", buttons: held().slice(0, 4) });
    expect(padFromGamepads(gamepadsOf(nav([short]))).buttons).toBe(PadButton.UP | PadButton.RIGHT);
    // Android and Windows: the browser's pad alone, as before.
    fillDpadFromNative(false);
    expect(gamepadsOf(nav([real]))[0]).toBe(real);
    resetNativePad();
    vi.unstubAllGlobals();
  });

  it("#668: two controllers each take the d-pad of their own twin", async () => {
    const { gamepadsOf, fillDpadFromNative, installNativePad, resetNativePad } = await import("./nativePad");
    resetNativePad();
    vi.stubGlobal("window", { dispatchEvent: () => true });
    installNativePad();
    fillDpadFromNative();
    const nav = { getGamepads: () => [pad({ id: "Xbox Wireless" }), pad({ id: "DualSense" })] } as unknown as Navigator;
    const send = window.__ps5uNativePad!;
    send({ slot: 0, id: "DualSense", buttons: held(14), axes: [] });
    send({ slot: 1, id: "Xbox Wireless", buttons: held(), axes: [] });
    const [xbox, ds] = gamepadsOf(nav);
    expect(padFromGamepads([xbox]).buttons).toBe(0);
    expect(padFromGamepads([ds]).buttons).toBe(PadButton.LEFT);
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
