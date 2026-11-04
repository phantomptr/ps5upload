import { describe, expect, it } from "vitest";

import { rowWindow, WINDOW_FROM, windowMoved } from "./useWindowedRows";

describe("rowWindow", () => {
  it("renders every row of an ordinary folder", () => {
    expect(rowWindow(0, 800, 40, WINDOW_FROM - 1)).toEqual({
      start: 0,
      end: WINDOW_FROM - 1,
    });
  });

  it("renders one screen and a margin of a long one, wherever it is scrolled", () => {
    const top = rowWindow(120, 800, 40, 100_000);
    expect(top.start).toBe(0);
    expect(top.end).toBe(40);
    const middle = rowWindow(-40_000, 800, 40, 100_000);
    expect(middle.start).toBe(980);
    expect(middle.end).toBe(1040);
    const last = rowWindow(-40 * 100_000, 800, 40, 100_000);
    expect(last.end).toBe(100_000);
    expect(last.start).toBeLessThan(100_000);
  });

  it("before the first layout, still a screenful and no more", () => {
    const w = rowWindow(0, 0, 40, 100_000);
    expect(w.end).toBeGreaterThan(0);
    expect(w.end).toBeLessThan(100);
  });
});

describe("windowMoved", () => {
  const count = 10_000;
  it("waits until the screen has come near the rendered edge", () => {
    const cur = { start: 100, end: 160 };
    expect(windowMoved(cur, cur, count)).toBe(false);
    expect(windowMoved(cur, { start: 105, end: 165 }, count)).toBe(false);
    expect(windowMoved(cur, { start: 110, end: 170 }, count)).toBe(true);
  });

  it("always reaches the very ends of the list", () => {
    expect(
      windowMoved({ start: 3, end: 63 }, { start: 0, end: 60 }, count),
    ).toBe(true);
    expect(
      windowMoved(
        { start: count - 62, end: count - 2 },
        { start: count - 60, end: count },
        count,
      ),
    ).toBe(true);
  });
});
