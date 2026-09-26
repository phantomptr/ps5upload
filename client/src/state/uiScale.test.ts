import { describe, expect, it } from "vitest";

import { clampUiScale, uiScaleLabel, UI_SCALE_STEPS } from "./uiScale";

/**
 * The UI-scale (Text size) multiplier MUST stay clamped to the designed
 * range — an out-of-bounds value (corrupt localStorage, a future wider
 * control) would make the UI illegibly tiny or overflow every layout (the
 * class of bug behind the Payloads send-screen report).
 */
describe("clampUiScale", () => {
  const MIN = UI_SCALE_STEPS[0];
  const MAX = UI_SCALE_STEPS[UI_SCALE_STEPS.length - 1];

  it("caps an over-large scale at the max step", () => {
    expect(clampUiScale(99)).toBe(MAX);
  });

  it("floors an under-small scale at the min step", () => {
    expect(clampUiScale(0.01)).toBe(MIN);
    expect(clampUiScale(-3)).toBe(MIN);
  });

  it("passes an in-range value through unchanged", () => {
    expect(clampUiScale(1.1)).toBe(1.1);
  });

  it("falls back to 1.0 for non-finite input (NaN, Infinity)", () => {
    expect(clampUiScale(Number.NaN)).toBe(1.0);
    expect(clampUiScale(Number.POSITIVE_INFINITY)).toBe(1.0);
  });
});

describe("uiScaleLabel", () => {
  it("renders a percentage", () => {
    expect(uiScaleLabel(0.8)).toBe("80%");
    expect(uiScaleLabel(1.0)).toBe("100%");
    expect(uiScaleLabel(1.25)).toBe("125%");
  });
});
