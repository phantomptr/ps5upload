import { describe, expect, it } from "vitest";

import { progressFigures, resumedShown, runBytes } from "./resumedProgress";

const GiB = 1024 ** 3;
const MiB = 1024 ** 2;

describe("resumed upload progress", () => {
  it("a resumed single file counts what the PS5 had, and times only what is left", () => {
    // The #report case: 155 GiB file, 63.1 GiB already on the PS5, 121 MiB sent this run,
    // 1.18 MiB/s. The engine's bytes_sent counts the resumed part.
    const total = 155 * GiB;
    const resumed = 63.1 * GiB;
    const sent = resumed + 121 * MiB;
    const rate = 1.18 * MiB;
    const f = progressFigures(sent, total, rate);
    expect(Math.round(f.pct)).toBe(41);
    expect(f.remainingBytes).toBeCloseTo(total - sent, 0);
    // About 22 h, not the 37 h the whole file would take.
    expect(f.etaSec! / 3600).toBeGreaterThan(21);
    expect(f.etaSec! / 3600).toBeLessThan(23);
    expect(runBytes(sent, resumed)).toBeCloseTo(121 * MiB, 0);
    expect(resumedShown(resumed, total)).toBe(resumed);
  });

  it("a multi-file resume: files in place count as done too", () => {
    const f = progressFigures(600, 1000, 50);
    expect(f).toEqual({ pct: 60, remainingBytes: 400, etaSec: 8 });
    expect(runBytes(600, 500)).toBe(100);
    expect(resumedShown(500, 1000)).toBe(500);
  });

  it("a fresh upload has no resumed line and all bytes are this run's", () => {
    expect(resumedShown(0, 1000)).toBeNull();
    expect(resumedShown(undefined, 1000)).toBeNull();
    expect(runBytes(250, undefined)).toBe(250);
    expect(progressFigures(250, 1000, 25)).toEqual({ pct: 25, remainingBytes: 750, etaSec: 30 });
  });

  it("no speed yet: no time left", () => {
    expect(progressFigures(600, 1000, 0)).toEqual({ pct: 60, remainingBytes: 400, etaSec: null });
  });

  it("done: 100%, nothing left, no time left", () => {
    expect(progressFigures(1000, 1000, 50)).toEqual({ pct: 100, remainingBytes: 0, etaSec: null });
    // A counter past the total (a resend) never shows more than 100%.
    expect(progressFigures(1200, 1000, 50).pct).toBe(100);
  });

  it("no total yet: 0% and no time left", () => {
    expect(progressFigures(10, 0, 5)).toEqual({ pct: 0, remainingBytes: 0, etaSec: null });
  });

  it("a snapshot whose resumed figure is ahead of its bytes never gives negative bytes", () => {
    expect(runBytes(100, 500)).toBe(0);
    expect(resumedShown(2000, 1000)).toBe(1000);
  });
});
