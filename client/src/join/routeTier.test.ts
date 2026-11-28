import { describe, expect, it } from "vitest";
import { classify, LIMITS, readTier, RttMeter, tierLabel, type Reading } from "./routeTier";

const r = (rttMs: number, jitterMs: number | null = null, lossPermille: number | null = null, headroomPct: number | null = null, behind = 0): Reading => ({
  rttMs,
  jitterMs,
  lossPermille,
  headroomPct,
  behind,
});

describe("route tiers", () => {
  // The same table as engine join_route.rs tiers_match_the_app_and_the_page and
  // apps/play/tests/test_route.cpp test_tiers.
  it("matches the app and the engine", () => {
    expect(classify(r(2, 1, 0, 400))).toBe("excellent");
    expect(classify(r(20, 5, 1, 150))).toBe("excellent");
    expect(classify(r(21))).toBe("good");
    expect(classify(r(40, 15, 5, 125))).toBe("good");
    expect(classify(r(41))).toBe("playable");
    expect(classify(r(80, 30, 10, 100))).toBe("playable");
    expect(classify(r(81))).toBe("poor");
    expect(classify(r(225))).toBe("poor");
    expect(classify(r(3, 20))).toBe("playable");
    expect(classify(r(3, null, 20))).toBe("poor");
    expect(classify(r(3, null, 3))).toBe("good");
    expect(classify(r(3, null, null, 110))).toBe("playable");
    expect(classify(r(3, null, null, 90))).toBe("poor");
    expect(classify(r(3, 1, 0, null, 1))).toBe("playable");
    expect(classify(r(3, 1, 0, null, 3))).toBe("poor");
    expect(classify(r(10))).toBe("excellent");
    expect(classify({ rttMs: null })).toBeNull();
    expect(LIMITS.map((l) => l.rttMs)).toEqual([20, 40, 80]);
  });

  it("reads the engine's words only", () => {
    expect(readTier("good")).toBe("good");
    expect(readTier("down")).toBeNull();
    expect(readTier(3)).toBeNull();
  });

  it("smooths the page's round trips", () => {
    const m = new RttMeter();
    expect(m.reading()).toEqual({ rttMs: null, jitterMs: null });
    for (let i = 0; i < 10; i++) m.add(4);
    expect(m.reading()).toEqual({ rttMs: 4, jitterMs: 0 });
    for (let i = 0; i < 10; i++) m.add(i % 2 ? 10 : 60);
    const x = m.reading();
    expect(x.rttMs).toBeGreaterThan(20);
    expect(x.jitterMs).toBeGreaterThan(30);
    expect(classify(x)).toBe("poor");
    m.add(Number.NaN);
    expect(m.reading()).toEqual(x);
  });

  it("says the tier in words", () => {
    const tr = (_k: string, _v: undefined, f: string) => f;
    expect(tierLabel(tr, "playable")).toBe("Playable");
  });
});
