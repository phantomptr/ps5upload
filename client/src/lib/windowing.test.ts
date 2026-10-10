import { describe, expect, it } from "vitest";

import { windowRange } from "./windowing";

describe("windowRange", () => {
  it("renders the visible rows plus overscan", () => {
    // 20px rows, 200px viewport → 10 visible rows, scrolled to row 100.
    expect(windowRange(2000, 200, 20, 5000, 5)).toEqual({ start: 95, end: 115 });
  });

  it("clamps at the top and the bottom", () => {
    expect(windowRange(0, 200, 20, 5000, 5)).toEqual({ start: 0, end: 15 });
    expect(windowRange(99_999, 200, 20, 50, 5)).toEqual({ start: 44, end: 50 });
  });

  it("handles empty lists and an unmeasured viewport", () => {
    expect(windowRange(0, 200, 20, 0)).toEqual({ start: 0, end: 0 });
    const r = windowRange(0, 0, 20, 5000, 0);
    expect(r.start).toBe(0);
    expect(r.end).toBe(50);
  });

  it("never renders more than a screenful plus overscan of 5,000 rows", () => {
    const r = windowRange(40_000, 600, 18, 5000, 20);
    expect(r.end - r.start).toBeLessThanOrEqual(Math.ceil(600 / 18) + 40);
  });
});
