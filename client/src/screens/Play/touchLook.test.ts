import { describe, expect, it } from "vitest";

import { controllerScale, thumbOffset, touchOpacity } from "./touchLook";

describe("touchOpacity", () => {
  it("keeps a setting in range", () => {
    expect(touchOpacity(0.6)).toBe(0.6);
    expect(touchOpacity(1)).toBe(1);
    expect(touchOpacity(0.25)).toBe(0.25);
  });
  it("never draws fainter than a quarter or more than solid", () => {
    expect(touchOpacity(0)).toBe(0.25);
    expect(touchOpacity(0.1)).toBe(0.25);
    expect(touchOpacity(3)).toBe(1);
  });
  it("draws an odd saved value solid", () => {
    expect(touchOpacity(undefined)).toBe(1);
    expect(touchOpacity("0.5")).toBe(1);
    expect(touchOpacity(Number.NaN)).toBe(1);
  });
});

describe("controllerScale", () => {
  it("is the size setting alone on the screen it is drawn for", () => {
    expect(controllerScale("m", 844, 390, false)).toBe(1);
    expect(controllerScale("s", 844, 390, false)).toBe(0.85);
    expect(controllerScale("l", 844, 390, false)).toBe(1.15);
    expect(controllerScale("m", 390, 480, true)).toBe(1);
  });
  it("fits the shorter way of the screen", () => {
    // A wider phone sideways: the height decides.
    expect(controllerScale("m", 915, 412, false)).toBeCloseTo(1.056, 3);
    // An upright small phone's deck: the height decides.
    expect(controllerScale("m", 360, 420, true)).toBeCloseTo(0.875, 3);
  });
  it("grows on a tablet, but not without end", () => {
    expect(controllerScale("m", 1180, 820, false)).toBeCloseTo(1.398, 3);
    expect(controllerScale("m", 2560, 1600, false)).toBe(1.4);
    expect(controllerScale("l", 2560, 1600, false)).toBe(1.61);
  });
  it("never shrinks past readable on a tiny space", () => {
    expect(controllerScale("m", 300, 150, false)).toBe(0.7);
  });
  it("is the size setting alone before the space is measured", () => {
    expect(controllerScale("l", 0, 0, false)).toBe(1.15);
    expect(controllerScale("s", Number.NaN, 400, true)).toBe(0.85);
  });
});

describe("thumbOffset", () => {
  it("is centred at rest and follows one axis", () => {
    expect(thumbOffset(0, 0, 30)).toEqual({ dx: 0, dy: 0 });
    expect(thumbOffset(32767, 0, 30)).toEqual({ dx: 30, dy: 0 });
    expect(thumbOffset(0, -32767, 30)).toEqual({ dx: 0, dy: -30 });
  });
  it("keeps a corner push on the ring, not past it", () => {
    const { dx, dy } = thumbOffset(32767, 32767, 30);
    expect(Math.hypot(dx, dy)).toBeCloseTo(30, 6);
    expect(dx).toBeCloseTo(dy, 6);
  });
  it("leaves a push inside the circle as it is", () => {
    const { dx, dy } = thumbOffset(16384, 16384, 30);
    expect(dx).toBeCloseTo(15, 0);
    expect(dy).toBeCloseTo(15, 0);
  });
});
