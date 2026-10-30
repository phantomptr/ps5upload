import { describe, expect, it } from "vitest";

import { visibleArea } from "./pipArea";

// #566, #575: the Windows pop-out's page laid out bigger than its window; the player is fitted
// to the part that shows, and nothing changes where the sizes agree.
describe("visibleArea", () => {
  it("is nothing to do when the window shows the whole page", () => {
    expect(
      visibleArea({ width: 960, height: 540 }, 2, { width: 480, height: 270 }),
    ).toBeNull();
    expect(
      visibleArea({ width: 721, height: 406 }, 1.5, {
        width: 480,
        height: 270,
      }),
    ).toBeNull();
    // A pixel of rounding is not a mismatch.
    expect(
      visibleArea({ width: 959, height: 539 }, 2, { width: 480, height: 270 }),
    ).toBeNull();
  });

  it("is the window's part when the page is laid out bigger than it", () => {
    // 1685x860 device pixels at 150%: 1123x573 shows of a page laid out 1600x900.
    expect(
      visibleArea({ width: 1685, height: 860 }, 1.5, {
        width: 1600,
        height: 900,
      }),
    ).toEqual({
      width: 1685 / 1.5,
      height: 860 / 1.5,
    });
    expect(
      visibleArea({ width: 800, height: 450 }, 1, { width: 1200, height: 675 }),
    ).toEqual({ width: 800, height: 450 });
  });

  it("never grows the page, and takes a missing ratio as 1", () => {
    expect(
      visibleArea({ width: 2000, height: 300 }, 0, {
        width: 1000,
        height: 600,
      }),
    ).toEqual({ width: 1000, height: 300 });
  });
});
