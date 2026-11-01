import { describe, expect, it } from "vitest";

import {
  KbmSender,
  decodeReport,
  emitPadKbm,
  encodeReport,
  fromHex,
  mergeHeld,
  onPadKbm,
  routeFor,
  unionCaps,
  type PadKbmMsg,
} from "./kbmInput";

const ctx = { owner: true, appFirst: false, pointerLocked: false };

describe("routeFor with capture (mode and capture are separate)", () => {
  it("released: keys stay on this computer in Keyboard & mouse and Mixed", () => {
    expect(routeFor("native", { ...ctx, captured: false })).toEqual({ keys: "none", mouse: "none" });
    expect(routeFor("mixed", { ...ctx, captured: false })).toEqual({ keys: "none", mouse: "none" });
    // Controller sends its keys to the pad as it always has.
    expect(routeFor("controller", { ...ctx, captured: false })).toEqual({ keys: "pad", mouse: "pad" });
  });

  it("captured: native keys and mouse, Mixed keys to the pad", () => {
    expect(routeFor("native", { ...ctx, captured: true, pointerLocked: true })).toEqual({ keys: "native", mouse: "native" });
    expect(routeFor("mixed", { ...ctx, captured: true, pointerLocked: true })).toEqual({ keys: "pad", mouse: "native" });
  });

  it("the hover fallback (nothing can capture) sends the mouse only after the click", () => {
    expect(routeFor("native", { ...ctx, captured: false, hoverFallback: true }).mouse).toBe("none");
    expect(routeFor("native", { ...ctx, captured: true, hoverFallback: true }).mouse).toBe("native");
  });
});

describe("phones' reports merged into the page's", () => {
  it("decodes a report and its hex", () => {
    const b = encodeReport({ buttons: 1, mods: 2, keys: [0x04, 0x05] }, -3, 7, 1, -1);
    const hex = [...b.slice(1)].map((x) => x.toString(16).padStart(2, "0")).join("");
    expect(decodeReport(fromHex(hex)!)).toEqual({ buttons: 1, mods: 2, dx: -3, dy: 7, wheel: 1, hwheel: -1, keys: [4, 5] });
    expect(fromHex("zz")).toBeNull();
    expect(fromHex("abc")).toBeNull();
    expect(decodeReport(new Uint8Array(3))).toBeNull();
  });

  it("merges held keys and buttons (the page's first, at most 8) and wants", () => {
    expect(mergeHeld([{ buttons: 1, mods: 1, keys: [4] }, { buttons: 2, mods: 4, keys: [4, 5] }])).toEqual({ buttons: 3, mods: 5, keys: [4, 5] });
    expect(unionCaps({ mouse: false, keyboard: false }, { mouse: true, keyboard: false })).toEqual({ mouse: true, keyboard: false });
  });

  it("the sender sends page + phone held state; the page's release-all keeps the phone's", () => {
    const sent: number[][] = [];
    const s = new KbmSender({
      send: (m) => (m instanceof Uint8Array && sent.push([...m.slice(1)]), true),
      setTimer: () => 1,
      clearTimer: () => {},
      setRepeat: () => 1,
      clearRepeat: () => {},
    });
    s.key("KeyA", true); // 0x04
    s.guest(7, { buttons: 1, mods: 0, keys: [0x05] });
    expect(sent.slice(-1)[0]!.slice(0, 1)).toEqual([1]);
    expect(sent.slice(-1)[0]!.slice(8, 10)).toEqual([0x04, 0x05]);
    s.releaseAll();
    expect(sent.slice(-1)[0]![0]).toBe(1); // the phone's button still held
    expect(sent.slice(-1)[0]!.slice(8, 10)).toEqual([0x05, 0]);
    s.guest(7, null);
    expect(sent.slice(-1)[0]).toEqual(new Array(16).fill(0));
  });
});

describe("pad_kbm messages", () => {
  it("are read and handed to the listeners", () => {
    const got: PadKbmMsg[] = [];
    const off = onPadKbm((m) => got.push(m));
    emitPadKbm({ id: 3, on: true, mouse: true, keyboard: false });
    emitPadKbm({ id: 3, r: "00" });
    emitPadKbm({ nope: 1 });
    off();
    emitPadKbm({ id: 4, left: true });
    expect(got.map((m) => [m.id, m.on, m.r ?? null])).toEqual([
      [3, true, null],
      [3, undefined, "00"],
    ]);
  });
});
