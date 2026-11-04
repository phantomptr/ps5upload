// A d-pad reported as one axis: non-standard pads in Chromium and Firefox (axes[9], in sevenths from
// -1, resting at 9/7). Generic USB pads and arcade sticks, the ones the mapping wizard is for.
import { describe, expect, it } from "vitest";

import { detectPress } from "./padProfiles";
import { guessProfile, HAT_LEFT, HAT_RIGHT, HAT_UP, hatPosition, padFromGamepads, PadButton, playRumble, sourceValue, STANDARD_PROFILE } from "./remotePlayStream";

const REST = 9 / 7;
const at = (i: number) => -1 + (i * 2) / 7;
const pad = (hat: number, over: Partial<Gamepad> = {}) =>
  ({
    id: "Generic USB pad",
    connected: true,
    mapping: "",
    axes: [0, 0, 0, 0, 0, 0, 0, 0, 0, hat],
    buttons: Array.from({ length: 12 }, () => ({ pressed: false, touched: false, value: 0 })),
    ...over,
  }) as unknown as Gamepad;

describe("a hat d-pad", () => {
  it("reads eight positions, and none at rest", () => {
    expect([0, 1, 2, 3, 4, 5, 6, 7].map((i) => hatPosition(at(i)))).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
    expect(hatPosition(REST)).toBeNull();
    // An axis the pad doesn't have.
    expect(hatPosition(NaN)).toBeNull();
  });

  it("counts a diagonal for both its directions, and not the rest", () => {
    const up = { h: 9, at: HAT_UP };
    const right = { h: 9, at: HAT_RIGHT };
    expect(sourceValue(pad(at(1)), up)).toBe(1);
    expect(sourceValue(pad(at(1)), right)).toBe(1);
    expect(sourceValue(pad(at(7)), up)).toBe(1);
    expect(sourceValue(pad(at(4)), up)).toBe(0);
    expect(sourceValue(pad(REST), up)).toBe(0);
  });

  it("is mapped as what it is, each direction its own, left too", () => {
    const rest = { buttons: [], axes: [0, 0, 0, 0, 0, 0, 0, 0, 0, REST] };
    const pressed = (i: number) => detectPress(rest, { buttons: [], axes: [0, 0, 0, 0, 0, 0, 0, 0, 0, at(i)] });
    expect(pressed(0)).toEqual({ h: 9, at: HAT_UP });
    expect(pressed(2)).toEqual({ h: 9, at: HAT_RIGHT });
    // Only 4/7 from rest: too small a move to count as half an axis.
    expect(pressed(6)).toEqual({ h: 9, at: HAT_LEFT });
    // A plain stick is still half an axis.
    expect(detectPress({ buttons: [], axes: [0] }, { buttons: [], axes: [0.9] })).toEqual({ a: 0, d: 1 });
  });

  it("plays without mapping, on a pad the browser doesn't lay out", () => {
    const id = `pad-${Math.random()}`;
    const p = (hat: number) => pad(hat, { id });
    // Seen at rest once, it stays known while pressed.
    expect(guessProfile(p(REST))).not.toBe(STANDARD_PROFILE);
    const buttons = (hat: number) => padFromGamepads([p(hat)], guessProfile).buttons;
    expect(buttons(at(0)) & PadButton.UP).toBeTruthy();
    expect(buttons(at(6)) & PadButton.LEFT).toBeTruthy();
    expect(buttons(REST)).toBe(0);
  });

  it("leaves a standard pad and one with no hat alone", () => {
    expect(guessProfile(pad(REST, { mapping: "standard" }))).toBe(STANDARD_PROFILE);
    expect(guessProfile(pad(0, { id: "no hat" }))).toBe(STANDARD_PROFILE);
  });
});

describe("rumble", () => {
  const shaker = () => {
    const calls: unknown[] = [];
    return { calls, g: { vibrationActuator: { playEffect: (...a: unknown[]) => calls.push(a) } } as unknown as Gamepad };
  };

  it("goes to the browser's pad when the app's own reader has no motors", () => {
    const { calls, g } = shaker();
    const reader = { vibrationActuator: null } as unknown as Gamepad;
    playRumble([reader], 255, 0, () => [g]);
    expect(calls).toHaveLength(1);
  });

  it("stays with the list it was given when that can shake", () => {
    const own = shaker();
    const other = shaker();
    playRumble([own.g], 255, 0, () => [other.g]);
    expect(own.calls).toHaveLength(1);
    expect(other.calls).toHaveLength(0);
  });
});
