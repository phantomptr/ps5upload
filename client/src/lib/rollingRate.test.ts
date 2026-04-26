import { describe, expect, it } from "vitest";

import {
  averageRate,
  computeRate,
  DEFAULT_RATE_WINDOW,
  pushRateSample,
  type RateSample,
} from "./rollingRate";

describe("pushRateSample", () => {
  it("pushes the first sample even into an empty window", () => {
    const s: RateSample[] = [];
    pushRateSample(s, 100, 0);
    expect(s).toEqual([{ ts: 100, bytes: 0 }]);
  });

  it("appends when bytesSent advances", () => {
    const s: RateSample[] = [{ ts: 100, bytes: 0 }];
    pushRateSample(s, 200, 1024);
    expect(s).toEqual([
      { ts: 100, bytes: 0 },
      { ts: 200, bytes: 1024 },
    ]);
  });

  it("skips flat samples to keep the window anchored to real progress", () => {
    const s: RateSample[] = [{ ts: 100, bytes: 1024 }];
    pushRateSample(s, 200, 1024);
    pushRateSample(s, 300, 1024);
    expect(s).toEqual([{ ts: 100, bytes: 1024 }]);
  });

  it("trims from the front when over windowSize", () => {
    const s: RateSample[] = [];
    for (let i = 0; i <= DEFAULT_RATE_WINDOW; i += 1) {
      pushRateSample(s, 100 + i * 100, i * 1024);
    }
    expect(s).toHaveLength(DEFAULT_RATE_WINDOW);
    // First entry was (100, 0); after one extra push the window starts
    // at the second sample.
    expect(s[0].bytes).toBe(1024);
  });

  it("respects a custom windowSize", () => {
    const s: RateSample[] = [];
    pushRateSample(s, 100, 0, 2);
    pushRateSample(s, 200, 1024, 2);
    pushRateSample(s, 300, 2048, 2);
    expect(s).toHaveLength(2);
    expect(s[0].bytes).toBe(1024);
    expect(s[1].bytes).toBe(2048);
  });
});

describe("computeRate", () => {
  it("returns 0 when fewer than two samples (no rate yet)", () => {
    expect(computeRate([], 1000)).toBe(0);
    expect(computeRate([{ ts: 100, bytes: 1024 }], 1000)).toBe(0);
  });

  it("computes (newest - oldest) / elapsed across the window", () => {
    const s: RateSample[] = [
      { ts: 1000, bytes: 0 },
      { ts: 2000, bytes: 500_000 },
    ];
    expect(computeRate(s, 2000)).toBe(500_000);
  });

  it("decays the rate when no new samples arrive (uses `now`, not newest.ts)", () => {
    const s: RateSample[] = [
      { ts: 1000, bytes: 0 },
      { ts: 2000, bytes: 1_000_000 },
    ];
    const fresh = computeRate(s, 2000);
    const stale = computeRate(s, 4000);
    expect(stale).toBeLessThan(fresh);
    // 1 MB over 3 s of wall time
    expect(stale).toBeCloseTo(1_000_000 / 3, 0);
  });

  it("clamps negatives to 0 (defensive — bytesSent should never go down)", () => {
    const s: RateSample[] = [
      { ts: 1000, bytes: 1_000_000 },
      { ts: 2000, bytes: 500_000 },
    ];
    expect(computeRate(s, 2000)).toBe(0);
  });

  it("avoids divide-by-zero when oldest.ts === now", () => {
    const s: RateSample[] = [
      { ts: 1000, bytes: 0 },
      { ts: 1000, bytes: 1024 },
    ];
    // Δt floored to 1 ms → 1024 bytes / 0.001 s = 1.024 GiB/s — high
    // but finite, so the UI still renders something sane.
    expect(Number.isFinite(computeRate(s, 1000))).toBe(true);
  });
});

describe("averageRate", () => {
  it("returns bytes / seconds", () => {
    expect(averageRate(1_000_000, 2000)).toBe(500_000);
  });

  it("returns 0 for non-positive elapsed", () => {
    expect(averageRate(1024, 0)).toBe(0);
    expect(averageRate(1024, -100)).toBe(0);
  });

  it("returns 0 for non-positive bytes", () => {
    expect(averageRate(0, 1000)).toBe(0);
    expect(averageRate(-100, 1000)).toBe(0);
  });

  it("returns 0 for non-finite inputs", () => {
    expect(averageRate(Infinity, 1000)).toBe(0);
    expect(averageRate(1024, NaN)).toBe(0);
  });
});
