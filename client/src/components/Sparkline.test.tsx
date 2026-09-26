import { describe, expect, it } from "vitest";

import { computeSparkPoints } from "./Sparkline";

describe("computeSparkPoints", () => {
  it("returns empty strings for fewer than 2 data points", () => {
    expect(computeSparkPoints([], 80, 24, 1.5)).toEqual({
      points: "",
      fillPoints: "",
    });
    expect(computeSparkPoints([50], 80, 24, 1.5)).toEqual({
      points: "",
      fillPoints: "",
    });
  });

  it("returns N coordinate pairs for N data points", () => {
    const { points } = computeSparkPoints([10, 20, 30, 40, 50], 80, 24, 1.5);
    expect(points.split(" ")).toHaveLength(5);
  });

  it("includes baseline corners in fillPoints", () => {
    const { fillPoints } = computeSparkPoints([10, 20, 30], 100, 30, 1.5);
    // Should start at bottom-left "0,30" and end at bottom-right "100,30"
    expect(fillPoints.startsWith("0,30 ")).toBe(true);
    expect(fillPoints.endsWith(" 100,30")).toBe(true);
  });

  it("scales data to the given height (value=0 → bottom, value=max → top)", () => {
    const { points } = computeSparkPoints([0, 100], 100, 40, 1.5);
    const [p0, p1] = points.split(" ");
    const y0 = parseFloat(p0.split(",")[1]);
    const y1 = parseFloat(p1.split(",")[1]);
    // value=0 → high y (bottom); value=100 → low y (top)
    expect(y0).toBeGreaterThan(y1);
    // pad=1.5, usableH=37 → y0 = 1.5 + 37*(1-0) = 38.5
    expect(y0).toBeCloseTo(38.5, 1);
    // y1 = 1.5 + 37*(1-1) = 1.5
    expect(y1).toBeCloseTo(1.5, 1);
  });

  it("uses explicit minY/maxY when provided", () => {
    const { points } = computeSparkPoints(
      [30, 70],
      100,
      40,
      1.5,
      0, // minY
      100, // maxY
    );
    const [p0, p1] = points.split(" ");
    const y0 = parseFloat(p0.split(",")[1]);
    const y1 = parseFloat(p1.split(",")[1]);
    // value=30 → y = 1.5 + 37*0.7 = 27.4
    expect(y0).toBeCloseTo(27.4, 1);
    // value=70 → y = 1.5 + 37*0.3 = 12.6
    expect(y1).toBeCloseTo(12.6, 1);
  });

  it("handles flat data (all same value) without NaN", () => {
    const { points, fillPoints } = computeSparkPoints([50, 50, 50], 80, 24, 1.5);
    expect(points).not.toContain("NaN");
    expect(fillPoints).not.toContain("NaN");
    // All y values should be equal (flat line in the middle)
    const ys = points.split(" ").map((p) => parseFloat(p.split(",")[1]));
    expect(ys.every((y) => y === ys[0])).toBe(true);
  });

  it("spreads X coordinates evenly across the width", () => {
    const { points } = computeSparkPoints([10, 20, 30, 40], 120, 24, 1.5);
    const xs = points.split(" ").map((p) => parseFloat(p.split(",")[0]));
    // 4 points across width=120 → stepX = 120/3 = 40
    expect(xs[0]).toBeCloseTo(0, 1);
    expect(xs[1]).toBeCloseTo(40, 1);
    expect(xs[2]).toBeCloseTo(80, 1);
    expect(xs[3]).toBeCloseTo(120, 1);
  });

  it("pads the Y range by strokeWidth to keep the stroke inside the viewBox", () => {
    // With height=10 and strokeWidth=3, pad=3, usableH=4
    const { points } = computeSparkPoints([0, 100], 100, 10, 3);
    const [, p1] = points.split(" ");
    const y1 = parseFloat(p1.split(",")[1]);
    // Top value (100) should be at pad = 3
    expect(y1).toBeCloseTo(3, 1);
  });
});
