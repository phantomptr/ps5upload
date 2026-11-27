import { afterEach, describe, expect, it, vi } from "vitest";
import { padFromGamepads, stickDeadzone, STANDARD_PROFILE, type StickDeadzones } from "./remotePlayStream";
import * as storage from "./safeStorage";
import { installWindowStub } from "../test-utils/storage";

/** A standard controller with these axes (left x, left y, right x, right y). */
function pad(axes: number[], id = "Xbox 360 Controller (XInput STANDARD GAMEPAD)"): Gamepad {
  return {
    id,
    index: 0,
    connected: true,
    mapping: "standard" as GamepadMappingType,
    axes,
    buttons: new Array(17).fill(0).map(() => ({ pressed: false, touched: false, value: 0 })),
    timestamp: 0,
  } as unknown as Gamepad;
}

const std = () => STANDARD_PROFILE;
const dzs = (dz: StickDeadzones) => () => dz;

describe("stickDeadzone", () => {
  it("leaves the stick alone at 0", () => {
    expect(stickDeadzone(0.3, -0.2, 0)).toEqual([0.3, -0.2]);
  });

  it("reads centered inside the zone", () => {
    // A worn right stick resting 83% up.
    expect(stickDeadzone(0.06, -0.83, 0.87)).toEqual([0, 0]);
  });

  it("still reaches the end, and keeps the direction", () => {
    const [x, y] = stickDeadzone(0, -1, 0.87);
    expect(x).toBeCloseTo(0);
    expect(y).toBeCloseTo(-1);
    const [dx, dy] = stickDeadzone(0.7, 0.7, 0.5);
    expect(dx).toBeCloseTo(dy);
    expect(dx).toBeGreaterThan(0);
  });

  it("starts small just past the zone, with no jump", () => {
    const [x] = stickDeadzone(0.92, 0, 0.87);
    expect(x).toBeCloseTo((0.92 - 0.87) / 0.13);
  });
});

describe("padFromGamepads with stick dead zones", () => {
  it("keeps the usual small zone without one", () => {
    const p = padFromGamepads([pad([0, 0, 0.05, -0.83])], std);
    expect(p.rx).toBe(0);
    expect(p.ry).toBe(-27197);
  });

  it("centers the drifting stick and leaves the other one", () => {
    const p = padFromGamepads([pad([0.5, 0, 0.05, -0.83])], std, dzs({ left: 0, right: 0.87 }));
    expect(p.rx).toBe(0);
    expect(p.ry).toBe(0);
    expect(p.lx).toBe(16384);
  });

  it("pushed all the way is still all the way", () => {
    const p = padFromGamepads([pad([0, 0, 1, 0])], std, dzs({ left: 0, right: 0.87 }));
    expect(p.rx).toBe(32767);
  });

  it("a hat used as a stick stays full on its diagonal", () => {
    const hatProfile = { ...STANDARD_PROFILE, rx: { a: 9, hat: "x" as const }, ry: { a: 9, hat: "y" as const } };
    // 9 axes, the 10th is the hat at its down-right position (3/7).
    const g = pad([0, 0, 0, 0, 0, 0, 0, 0, 0, -1 + (3 * 2) / 7]);
    const p = padFromGamepads([g], () => hatProfile, dzs({ left: 0, right: 0.5 }));
    expect(p.rx).toBe(32767);
    expect(p.ry).toBe(32767);
  });

  it("a zone under the usual one plays as the usual one", () => {
    const p = padFromGamepads([pad([0, 0, 0.07, 0])], std, dzs({ left: 0, right: 0.02 }));
    expect(p.rx).toBe(0);
  });

  it("a drifting pad no longer outvotes a second, resting one", () => {
    const worn = pad([0, 0, 0.05, -0.83], "worn");
    const fine = pad([0, 0, 0, 0], "fine");
    const p = padFromGamepads([worn, fine], std, (g) => (g.id === "worn" ? { left: 0, right: 0.87 } : null));
    expect(p.ry).toBe(0);
  });
});

describe("saved stick dead zones", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("are kept per controller and forgotten at 0", async () => {
    installWindowStub();
    const mod = await import("./padProfiles");
    mod.saveDeadzones("worn", { left: 0, right: 0.87 });
    expect(mod.deadzonesFor(pad([], "worn"))).toEqual({ left: 0, right: 0.87 });
    expect(mod.deadzonesFor(pad([], "other"))).toBeNull();
    mod.saveDeadzones("worn", { left: 0, right: 0 });
    expect(mod.deadzonesFor(pad([], "worn"))).toBeNull();
    expect(mod.loadDeadzones()).toEqual({});
  });

  it("follow the controller under the app's name, and back", async () => {
    installWindowStub();
    const mod = await import("./padProfiles");
    mod.saveDeadzones("Xbox Wireless Controller", { left: 0, right: 0.87 });
    expect(mod.deadzonesFor(pad([], "Xbox Wireless Controller (app)"))).toEqual({ left: 0, right: 0.87 });
    mod.saveDeadzones("Xbox Wireless Controller", null);
    mod.saveDeadzones("DualSense (app)", { left: 0.3, right: 0 });
    expect(mod.deadzonesFor(pad([], "DualSense"))).toEqual({ left: 0.3, right: 0 });
    expect(mod.deadzonesFor(pad([], "DualSense Edge"))).toBeNull();
  });

  it("drop damaged entries and cap the size", async () => {
    const get = vi.spyOn(storage, "safeGetItem").mockReturnValue(
      JSON.stringify({ a: { left: 0.2, right: 5 }, b: "x", c: { left: "y" }, d: { left: -1, right: 0 } }),
    );
    const mod = await import("./padProfiles");
    expect(mod.loadDeadzones()).toEqual({ a: { left: 0.2, right: 0.95 } });
    get.mockReturnValue("[]");
    expect(mod.loadDeadzones()).toEqual({});
    get.mockRestore();
  });
});
