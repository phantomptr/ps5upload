import { describe, it, expect } from "vitest";
import {
  psTimeToDate,
  formatUtcCompact,
  formatDrift,
  type PsTimeJson,
} from "./sysTimeFormat";

describe("psTimeToDate", () => {
  it("returns null when the wire response is not ok", () => {
    const bad: PsTimeJson = { ok: false, err_code: 0x80a23001 };
    expect(psTimeToDate(bad)).toBeNull();
  });

  it("returns null for null input", () => {
    expect(psTimeToDate(null)).toBeNull();
  });

  it("returns null when year is out of range", () => {
    const old: PsTimeJson = {
      ok: true,
      err_code: 0,
      year: 1969,
      month: 12,
      day: 31,
    };
    expect(psTimeToDate(old)).toBeNull();
  });

  it("returns null when month is invalid", () => {
    const bad: PsTimeJson = {
      ok: true,
      err_code: 0,
      year: 2026,
      month: 13,
      day: 1,
    };
    expect(psTimeToDate(bad)).toBeNull();
  });

  it("parses a normal date into a UTC JS Date", () => {
    const t: PsTimeJson = {
      ok: true,
      err_code: 0,
      year: 2026,
      month: 5,
      day: 15,
      hour: 23,
      min: 30,
      sec: 0,
    };
    const d = psTimeToDate(t);
    expect(d).not.toBeNull();
    // 2026-05-15 23:30:00 UTC -> 1778887800 unix
    expect(d!.getTime()).toBe(1_778_887_800 * 1000);
  });

  it("defaults missing time fields to zero", () => {
    const t: PsTimeJson = {
      ok: true,
      err_code: 0,
      year: 2024,
      month: 1,
      day: 1,
    };
    const d = psTimeToDate(t);
    expect(d).not.toBeNull();
    expect(d!.getUTCHours()).toBe(0);
    expect(d!.getUTCMinutes()).toBe(0);
    expect(d!.getUTCSeconds()).toBe(0);
  });
});

describe("formatUtcCompact", () => {
  it("returns the em-dash placeholder for null", () => {
    expect(formatUtcCompact(null)).toBe("—");
  });

  it("zero-pads single-digit fields", () => {
    const d = new Date(Date.UTC(2026, 0, 5, 9, 8, 7)); // 2026-01-05 09:08:07
    expect(formatUtcCompact(d)).toBe("2026-01-05 09:08:07 UTC");
  });

  it("renders a typical timestamp", () => {
    const d = new Date(Date.UTC(2026, 4, 15, 23, 30, 0));
    expect(formatUtcCompact(d)).toBe("2026-05-15 23:30:00 UTC");
  });
});

describe("formatDrift", () => {
  const PC_BASE_MS = 1_778_887_800_000; // 2026-05-15 23:30:00 UTC

  it("returns em-dash when ps5Date is null", () => {
    expect(formatDrift(null, PC_BASE_MS)).toBe("—");
  });

  it("renders +0s when in sync", () => {
    const d = new Date(PC_BASE_MS);
    expect(formatDrift(d, PC_BASE_MS)).toBe("+0s");
  });

  it("renders seconds with a real Unicode minus sign (PS5 behind)", () => {
    // U+2212 MINUS SIGN, not U+002D HYPHEN-MINUS. Typographically
    // correct + easier to distinguish at a glance from the "+" prefix.
    const d = new Date(PC_BASE_MS - 30_000);
    expect(formatDrift(d, PC_BASE_MS)).toBe("−30s");
  });

  it("renders seconds when drift < 60s (PS5 ahead)", () => {
    const d = new Date(PC_BASE_MS + 42_000);
    expect(formatDrift(d, PC_BASE_MS)).toBe("+42s");
  });

  it("renders minutes+seconds when drift < 1h", () => {
    const d = new Date(PC_BASE_MS + (5 * 60 + 17) * 1000);
    expect(formatDrift(d, PC_BASE_MS)).toBe("+5m 17s");
  });

  it("renders hours+minutes when drift >= 1h", () => {
    const d = new Date(PC_BASE_MS + (3 * 3600 + 25 * 60) * 1000);
    expect(formatDrift(d, PC_BASE_MS)).toBe("+3h 25m");
  });

  it("handles drift exactly at 60s boundary", () => {
    const d = new Date(PC_BASE_MS + 60_000);
    expect(formatDrift(d, PC_BASE_MS)).toBe("+1m 0s");
  });

  it("handles drift exactly at 1h boundary", () => {
    const d = new Date(PC_BASE_MS + 3600_000);
    expect(formatDrift(d, PC_BASE_MS)).toBe("+1h 0m");
  });
});
