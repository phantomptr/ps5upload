import { describe, expect, it } from "vitest";
import { dutyAtTemp } from "./index";
import type { FanCurvePoint } from "../../api/ps5";

// The graph's "you are here" marker reads the duty off this function, so if it
// disagrees with how the payload applies the curve the marker lies about what
// the console is doing. Pinned here rather than trusted.
const curve: FanCurvePoint[] = [
  { temp_c: 50, duty_pct: 30 },
  { temp_c: 65, duty_pct: 55 },
  { temp_c: 75, duty_pct: 80 },
  { temp_c: 85, duty_pct: 100 },
];

describe("dutyAtTemp", () => {
  it("returns the exact duty at a defined point", () => {
    expect(dutyAtTemp(curve, 50)).toBe(30);
    expect(dutyAtTemp(curve, 65)).toBe(55);
    expect(dutyAtTemp(curve, 85)).toBe(100);
  });

  it("interpolates linearly between points", () => {
    // Midway 50->65 is 57.5C, midway 30->55 is 42.5 -> rounds to 43.
    expect(dutyAtTemp(curve, 57.5)).toBe(43);
    // Midway 75->85 is 80C, midway 80->100 is 90.
    expect(dutyAtTemp(curve, 80)).toBe(90);
  });

  it("is flat outside the ends rather than extrapolating", () => {
    // Below the first point the fan does not run slower than the first duty,
    // and above the last it does not exceed it. Extrapolating would draw a
    // marker at a duty the console will never use.
    expect(dutyAtTemp(curve, 20)).toBe(30);
    expect(dutyAtTemp(curve, 0)).toBe(30);
    expect(dutyAtTemp(curve, 95)).toBe(100);
    expect(dutyAtTemp(curve, 120)).toBe(100);
  });

  it("survives degenerate curves", () => {
    expect(dutyAtTemp([], 60)).toBe(0);
    expect(dutyAtTemp([{ temp_c: 60, duty_pct: 42 }], 10)).toBe(42);
    expect(dutyAtTemp([{ temp_c: 60, duty_pct: 42 }], 90)).toBe(42);
    // Two points at the same temperature must not divide by zero.
    const flat: FanCurvePoint[] = [
      { temp_c: 60, duty_pct: 20 },
      { temp_c: 60, duty_pct: 80 },
    ];
    expect(dutyAtTemp(flat, 60)).toBe(20);
  });
});
