import { describe, expect, it } from "vitest";
import { DEFAULT_POINTS } from "./index";

describe("fan curve restore default (R9)", () => {
  it("is a valid curve in the existing {temp_c, duty_pct} format", () => {
    expect(DEFAULT_POINTS.length).toBeGreaterThan(1);
    for (const p of DEFAULT_POINTS) {
      expect(Object.keys(p).sort()).toEqual(["duty_pct", "temp_c"]);
      expect(p.duty_pct).toBeGreaterThanOrEqual(0);
      expect(p.duty_pct).toBeLessThanOrEqual(100);
    }
    const temps = DEFAULT_POINTS.map((p) => p.temp_c);
    expect([...temps].sort((a, b) => a - b)).toEqual(temps);
  });
});
