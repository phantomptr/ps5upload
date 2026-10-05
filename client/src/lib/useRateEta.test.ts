import { describe, expect, it } from "vitest";

import { stepRateEta } from "./useRateEta";
import type { RateSample } from "./rollingRate";

describe("stepRateEta", () => {
  it("has no rate and no estimate from a single sample", () => {
    const s: RateSample[] = [];
    expect(stepRateEta(s, 1000, 0, 100)).toEqual({ rate: 0, etaSeconds: null });
  });

  it("reads the rate from the window and the time left from it", () => {
    const s: RateSample[] = [];
    stepRateEta(s, 0, 0, 1000);
    const r = stepRateEta(s, 2000, 200, 1000);
    expect(r.rate).toBeCloseTo(100);
    expect(r.etaSeconds).toBeCloseTo(8);
  });

  it("gives no estimate once nothing is left", () => {
    const s: RateSample[] = [];
    stepRateEta(s, 0, 0, 100);
    expect(stepRateEta(s, 1000, 100, 100).etaSeconds).toBeNull();
  });

  it("starts again when the count falls (a new item)", () => {
    const s: RateSample[] = [];
    stepRateEta(s, 0, 0, 100);
    stepRateEta(s, 1000, 90, 100);
    const r = stepRateEta(s, 2000, 0, 500);
    expect(r).toEqual({ rate: 0, etaSeconds: null });
  });
});
