import { describe, expect, it } from "vitest";

import { DEFAULT_BUTTON_MAP, isDefaultMap, readButtonMap, remapPad, setMapping } from "./rpButtonMap";
import { IDLE_PAD, PadButton } from "./remotePlayStream";

describe("controller button remap", () => {
  it("leaves the pad alone with the default map", () => {
    const p = { ...IDLE_PAD, buttons: PadButton.CROSS | PadButton.UP, l2: 100, lx: 5000 };
    expect(remapPad(p, DEFAULT_BUTTON_MAP)).toBe(p);
    expect(remapPad(p, { cross: "cross" })).toBe(p);
  });

  it("swaps two buttons", () => {
    const map = { cross: "circle", circle: "cross" } as const;
    expect(remapPad({ ...IDLE_PAD, buttons: PadButton.CROSS }, map).buttons).toBe(PadButton.CIRCLE);
    expect(remapPad({ ...IDLE_PAD, buttons: PadButton.CIRCLE }, map).buttons).toBe(PadButton.CROSS);
    expect(remapPad({ ...IDLE_PAD, buttons: PadButton.CROSS | PadButton.CIRCLE }, map).buttons).toBe(
      PadButton.CROSS | PadButton.CIRCLE,
    );
  });

  it("turns a button off, and keeps sticks, touch and other buttons", () => {
    const p = { ...IDLE_PAD, buttons: PadButton.PS | PadButton.L1, lx: -32768, ry: 1200, touch: [1, 2] as [number, number] };
    const out = remapPad(p, { ps: "none" });
    expect(out.buttons).toBe(PadButton.L1);
    expect(out.lx).toBe(-32768);
    expect(out.ry).toBe(1200);
    expect(out.touch).toEqual([1, 2]);
  });

  it("maps triggers to buttons past half way, and buttons to full triggers", () => {
    const map = { l2: "l1", l1: "l2" } as const;
    expect(remapPad({ ...IDLE_PAD, l2: 100 }, map)).toMatchObject({ buttons: 0, l2: 0 });
    expect(remapPad({ ...IDLE_PAD, l2: 200 }, map)).toMatchObject({ buttons: PadButton.L1, l2: 0 });
    expect(remapPad({ ...IDLE_PAD, buttons: PadButton.L1 }, map)).toMatchObject({ buttons: 0, l2: 255 });
    // Trigger to trigger keeps how far it is pulled.
    expect(remapPad({ ...IDLE_PAD, l2: 90 }, { l2: "r2", r2: "l2" })).toMatchObject({ l2: 0, r2: 90 });
  });

  it("lets two buttons send the same one", () => {
    const map = { l3: "create" } as const;
    expect(remapPad({ ...IDLE_PAD, buttons: PadButton.L3 }, map).buttons).toBe(PadButton.SHARE);
    expect(remapPad({ ...IDLE_PAD, buttons: PadButton.SHARE }, map).buttons).toBe(PadButton.SHARE);
  });

  it("sets and clears one mapping", () => {
    const m = setMapping({}, "cross", "circle");
    expect(m).toEqual({ cross: "circle" });
    expect(setMapping(m, "cross", "cross")).toEqual({});
    expect(isDefaultMap({})).toBe(true);
    expect(isDefaultMap(m)).toBe(false);
  });

  it("reads a stored map safely", () => {
    expect(readButtonMap(undefined)).toEqual({});
    expect(readButtonMap("x")).toEqual({});
    expect(readButtonMap([1])).toEqual({});
    expect(readButtonMap({ cross: "circle", ps: "none", bogus: "cross", l1: "rocket", r1: 3, square: "square" })).toEqual({
      cross: "circle",
      ps: "none",
    });
  });
});
