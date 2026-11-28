import { describe, expect, it } from "vitest";

import { stepRateEta } from "./useRateEta";
import { ETA_AFTER_MS, STALL_MS, newRateTracker } from "./transferRate";

const MiB = 2 ** 20;

describe("stepRateEta", () => {
  it("has no rate and no estimate from a single sample", () => {
    const t = newRateTracker();
    expect(stepRateEta(t, 1000, 0, 100 * MiB)).toEqual({ rate: 0, etaSeconds: null, stalled: false });
  });

  it("reads the rate from the window and the time left once it has a few seconds of data", () => {
    const t = newRateTracker();
    stepRateEta(t, 0, 0, 100 * MiB);
    let r = stepRateEta(t, 2000, 2 * MiB, 100 * MiB);
    expect(r.rate).toBeCloseTo(MiB);
    expect(r.etaSeconds).toBeNull();
    r = stepRateEta(t, ETA_AFTER_MS, 3 * MiB, 100 * MiB);
    expect(r.etaSeconds).toBeCloseTo(97);
  });

  it("gives no estimate once nothing is left", () => {
    const t = newRateTracker();
    stepRateEta(t, 0, 0, 10 * MiB);
    expect(stepRateEta(t, 5000, 10 * MiB, 10 * MiB).etaSeconds).toBeNull();
  });

  it("starts again when the count falls (a new item)", () => {
    const t = newRateTracker();
    stepRateEta(t, 0, 0, 100 * MiB);
    stepRateEta(t, 1000, 90 * MiB, 100 * MiB);
    expect(stepRateEta(t, 2000, 0, 500 * MiB)).toEqual({ rate: 0, etaSeconds: null, stalled: false });
  });

  it("counts files at any pace, and says when they stop", () => {
    const t = newRateTracker();
    stepRateEta(t, 0, 0, 100, "count");
    const r = stepRateEta(t, 4000, 8, 100, "count");
    expect(r.rate).toBeCloseTo(2);
    expect(r.etaSeconds).toBeCloseTo(46);
    expect(stepRateEta(t, 4000 + STALL_MS, 8, 100, "count").stalled).toBe(true);
  });
});
