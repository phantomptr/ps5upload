import { describe, expect, it } from "vitest";

import { ETA_AFTER_MS, RATE_WINDOW_MS, RateTrackers, STALL_MS, newRateTracker, trackRate } from "./transferRate";

const MiB = 1024 * 1024;

describe("trackRate", () => {
  it("steady: the speed and the time left of a steady copy", () => {
    const t = newRateTracker();
    const total = 1000 * MiB;
    let v = trackRate(t, 0, 0, total);
    expect(v).toMatchObject({ bytesPerSec: 0, etaSeconds: null, stalled: false, elapsedSeconds: 0 });
    // 10 MiB a second, polled once a second.
    for (let s = 1; s <= 2; s++) v = trackRate(t, s * 1000, s * 10 * MiB, total);
    expect(v.bytesPerSec).toBeCloseTo(10 * MiB);
    // Not yet: the window holds two seconds, under ETA_AFTER_MS.
    expect(v.etaSeconds).toBeNull();
    for (let s = 3; s <= 20; s++) v = trackRate(t, s * 1000, s * 10 * MiB, total);
    expect(v.bytesPerSec).toBeCloseTo(10 * MiB);
    expect(v.etaSeconds).toBeCloseTo(80);
    expect(v.elapsedSeconds).toBe(20);
    expect(v.stalled).toBe(false);
  });

  it("bursty: blocks landing every few seconds read as their average, not as spikes and zeros", () => {
    const t = newRateTracker();
    const total = 10_000 * MiB;
    let done = 0;
    const seen: number[] = [];
    for (let s = 0; s <= 60; s++) {
      // 32 MiB every 4 s = 8 MiB/s.
      if (s > 0 && s % 4 === 0) done += 32 * MiB;
      const v = trackRate(t, s * 1000, done, total);
      if (s >= 12) seen.push(v.bytesPerSec);
    }
    for (const r of seen) {
      expect(r).toBeGreaterThan(4 * MiB);
      expect(r).toBeLessThan(13 * MiB);
    }
  });

  it("stalled: nothing for STALL_MS with bytes still to come hides speed and time left", () => {
    const t = newRateTracker();
    const total = 100 * MiB;
    for (let s = 0; s <= 5; s++) trackRate(t, s * 1000, s * MiB, total);
    let v = trackRate(t, 5000 + STALL_MS - 1000, 5 * MiB, total);
    expect(v.stalled).toBe(false);
    // The speed falls as the window empties.
    expect(v.bytesPerSec).toBe(0);
    v = trackRate(t, 5000 + STALL_MS, 5 * MiB, total);
    expect(v).toMatchObject({ stalled: true, bytesPerSec: 0, etaSeconds: null });
    // Bytes move again: no longer stalled.
    v = trackRate(t, 5000 + STALL_MS + 1000, 6 * MiB, total);
    expect(v.stalled).toBe(false);
  });

  it("finished bytes (the job settling) are never stalled and have no time left", () => {
    const t = newRateTracker();
    trackRate(t, 0, 0, 10 * MiB);
    trackRate(t, 1000, 10 * MiB, 10 * MiB);
    const v = trackRate(t, 1000 + STALL_MS * 2, 10 * MiB, 10 * MiB);
    expect(v).toMatchObject({ stalled: false, etaSeconds: null });
  });

  it("unknown total: a speed but no time left; stalled only once it had moved", () => {
    const t = newRateTracker();
    // Not moving from the start (a share still being scanned): not stalled.
    expect(trackRate(t, 0, 0, 0).stalled).toBe(false);
    expect(trackRate(t, STALL_MS * 2, 0, 0).stalled).toBe(false);
    let v = trackRate(t, STALL_MS * 2 + 1000, 5 * MiB, 0);
    for (let s = 2; s <= 6; s++) v = trackRate(t, STALL_MS * 2 + s * 1000, 5 * MiB * s, 0);
    expect(v.bytesPerSec).toBeGreaterThan(0);
    expect(v.etaSeconds).toBeNull();
    v = trackRate(t, STALL_MS * 3 + 6000, 30 * MiB, 0);
    expect(v.stalled).toBe(true);
  });

  it("restart after a resume: bytes going down start the speed again, elapsed keeps counting", () => {
    const t = newRateTracker();
    const total = 1000 * MiB;
    for (let s = 0; s <= 10; s++) trackRate(t, s * 1000, s * 20 * MiB, total);
    // The job restarted and counts from 50 MiB.
    let v = trackRate(t, 11_000, 50 * MiB, total);
    expect(v.bytesPerSec).toBe(0);
    expect(v.etaSeconds).toBeNull();
    expect(v.elapsedSeconds).toBe(11);
    v = trackRate(t, 12_000, 55 * MiB, total);
    expect(v.bytesPerSec).toBeCloseTo(5 * MiB);
    // The time left waits for ETA_AFTER_MS of the new window.
    expect(v.etaSeconds).toBeNull();
    v = trackRate(t, 11_000 + ETA_AFTER_MS, 50 * MiB + (ETA_AFTER_MS / 1000) * 5 * MiB, total);
    expect(v.etaSeconds).not.toBeNull();
  });

  it("a page that opens mid-copy counts only what moved since", () => {
    const t = newRateTracker();
    trackRate(t, 0, 900 * MiB, 1000 * MiB);
    const v = trackRate(t, 1000, 901 * MiB, 1000 * MiB);
    expect(v.bytesPerSec).toBeCloseTo(MiB);
  });

  it("the window forgets what is older than RATE_WINDOW_MS", () => {
    const t = newRateTracker();
    const total = 10_000 * MiB;
    let done = 0;
    for (let s = 0; s <= 20; s++) trackRate(t, s * 1000, (done += 50 * MiB), total);
    // Slows to 1 MiB/s: after a full window the old speed is gone.
    let v = trackRate(t, 21_000, done, total);
    for (let s = 21; s <= 21 + RATE_WINDOW_MS / 1000; s++) v = trackRate(t, s * 1000, (done += MiB), total);
    expect(v.bytesPerSec).toBeLessThan(2 * MiB);
  });

  it("several polls in the same millisecond keep the latest count", () => {
    const t = newRateTracker();
    trackRate(t, 0, 0, 100 * MiB);
    trackRate(t, 1000, MiB, 100 * MiB);
    const v = trackRate(t, 1000, 2 * MiB, 100 * MiB);
    expect(v.bytesPerSec).toBeCloseTo(2 * MiB);
  });

  it("ignores bad numbers", () => {
    const t = newRateTracker();
    const v = trackRate(t, 0, Number.NaN, Number.POSITIVE_INFINITY);
    expect(v).toMatchObject({ bytesPerSec: 0, etaSeconds: null, stalled: false });
  });
});

describe("RateTrackers", () => {
  it("keeps one tracker per key and forgets pruned ones", () => {
    const r = new RateTrackers();
    r.track("a", 0, 0, 100 * MiB);
    r.track("b", 0, 0, 100 * MiB);
    expect(r.track("a", 1000, MiB, 100 * MiB).bytesPerSec).toBeCloseTo(MiB);
    r.prune(["b"]);
    // "a" starts over.
    expect(r.track("a", 2000, 2 * MiB, 100 * MiB).bytesPerSec).toBe(0);
    expect(r.track("b", 2000, 4 * MiB, 100 * MiB).bytesPerSec).toBeCloseTo(2 * MiB);
  });
});
