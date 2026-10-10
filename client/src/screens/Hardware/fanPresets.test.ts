import { describe, expect, it } from "vitest";

import { FAN_THRESHOLD_MAX_C, FAN_THRESHOLD_MIN_C } from "../../api/ps5";
import { FAN_PRESETS } from "./fanPresets";

// The value is the temperature the fan control holds: lower is louder. The
// presets once had Quiet on the lowest (loudest) value, so pin the direction.
describe("fan presets", () => {
  it("puts Quiet on the highest target and Cool on the lowest", () => {
    const byId = Object.fromEntries(FAN_PRESETS.map((p) => [p.id, p.c]));
    expect(byId.quiet).toBeGreaterThan(byId.balanced);
    expect(byId.balanced).toBeGreaterThan(byId.cool);
  });

  it("stays inside the range the console accepts", () => {
    for (const p of FAN_PRESETS) {
      expect(p.c).toBeGreaterThanOrEqual(FAN_THRESHOLD_MIN_C);
      expect(p.c).toBeLessThanOrEqual(FAN_THRESHOLD_MAX_C);
    }
  });

  it("does not claim any preset is the console's own setting", () => {
    for (const p of FAN_PRESETS) {
      expect(p.hintFallback).not.toMatch(/default/i);
    }
  });
});
