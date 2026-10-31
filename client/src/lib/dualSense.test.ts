import { beforeEach, describe, expect, it, vi } from "vitest";

const raw = vi.fn();
const logged = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => raw(...a) }));
vi.mock("./invokeLogged", () => ({ invoke: (...a: unknown[]) => logged(...a) }));

const { DualSenseEffects, HapticsRumble, findDualSense } = await import("./dualSense");

/** A 10 ms haptics frame (30 stereo samples) at one level on both sides. */
function frame(l: number, r = l): Uint8Array {
  const b = new Uint8Array(120);
  const v = new DataView(b.buffer);
  for (let i = 0; i < 30; i++) {
    v.setInt16(i * 4, l, true);
    v.setInt16(i * 4 + 2, r, true);
  }
  return b;
}

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  raw.mockReset();
  logged.mockReset();
  logged.mockResolvedValue(undefined);
});

describe("HapticsRumble", () => {
  it("averages three frames into one rumble level, the louder side", () => {
    const h = new HapticsRumble();
    // |±8192| × 2 = 16384 per frame on the right, quiet left: level 16384 >> 8 = 64.
    expect(h.push(frame(0, 8192))).toBeNull();
    expect(h.push(frame(0, -8192))).toBeNull();
    expect(h.push(frame(0, 8192))).toBe(64);
  });

  it("ignores near-silence, lifts faint haptics to a level a pad feels, and stops once", () => {
    const h = new HapticsRumble();
    for (let i = 0; i < 3; i++) expect(h.push(frame(40))).toBeNull(); // 80 < 100: nothing
    h.push(frame(100));
    h.push(frame(100));
    expect(h.push(frame(100))).toBe(2); // 200 → floor 512 → 2
    h.push(frame(0));
    h.push(frame(0));
    expect(h.push(frame(0))).toBe(0); // stop
    h.push(frame(0));
    h.push(frame(0));
    expect(h.push(frame(0))).toBeNull(); // already stopped
  });
});

describe("DualSenseEffects", () => {
  it("plays haptics on the pad over USB, and rumbles once the pad cannot", async () => {
    const rumble = vi.fn();
    raw.mockResolvedValue(true);
    const fx = new DualSenseEffects({ connected: true, usb: true, haptics: true }, rumble);
    expect(logged).toHaveBeenCalledWith("pad_haptics_set", { on: true });
    fx.haptics(frame(1000));
    await flush();
    expect(raw).toHaveBeenCalledWith("pad_haptics_push", { pcm: Array.from(frame(1000)) });
    expect(rumble).not.toHaveBeenCalled();
    // The pad's audio could not be opened: from then on, rumble.
    raw.mockResolvedValue(false);
    fx.haptics(frame(1000));
    await flush();
    for (let i = 0; i < 3; i++) fx.haptics(frame(8192));
    expect(rumble).toHaveBeenCalledWith(64, 64);
  });

  it("rumbles straight away over Bluetooth, and not at all with vibration off on the console", () => {
    const rumble = vi.fn();
    const fx = new DualSenseEffects({ connected: true, usb: false, haptics: false }, rumble);
    expect(logged).not.toHaveBeenCalledWith("pad_haptics_set", { on: true });
    for (let i = 0; i < 3; i++) fx.haptics(frame(8192));
    expect(rumble).toHaveBeenCalledTimes(1);
    expect(raw).not.toHaveBeenCalled();
    fx.intensity(0, 1);
    expect(logged).toHaveBeenCalledWith("pad_dualsense_intensity", { haptics: 0, triggers: 1 });
    for (let i = 0; i < 3; i++) fx.haptics(frame(8192));
    expect(rumble).toHaveBeenCalledTimes(1);
  });

  it("hands the triggers to the app and lets them go at the end", () => {
    const fx = new DualSenseEffects({ connected: true, usb: false, haptics: false }, vi.fn());
    const left = [0x21, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    const right = [0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    fx.triggers(left, right);
    expect(logged).toHaveBeenCalledWith("pad_dualsense_triggers", { left, right });
    fx.stop();
    expect(logged).toHaveBeenCalledWith("pad_dualsense_reset");
    expect(logged).toHaveBeenCalledWith("pad_haptics_set", { on: false });
  });
});

describe("findDualSense", () => {
  it("never looks outside the desktop app", async () => {
    expect(await findDualSense()).toBeNull();
    expect(logged).not.toHaveBeenCalled();
  });
});
