import { describe, expect, it } from "vitest";
import { backupTimestamp } from "./backupTimestamp";

describe("backupTimestamp", () => {
  it("formats as YYYY-MM-DD_HHMMSS in local time", () => {
    const d = new Date(2026, 5, 17, 14, 25, 30); // June 17 2026, 14:25:30 local
    expect(backupTimestamp(d)).toBe("2026-06-17_142530");
  });

  it("zero-pads single-digit month/day/hour/minute/second", () => {
    const d = new Date(2026, 0, 2, 3, 4, 5); // Jan 2 2026, 03:04:05 local
    expect(backupTimestamp(d)).toBe("2026-01-02_030405");
  });

  it("defaults to the current time when no argument is given", () => {
    const before = new Date();
    const ts = backupTimestamp();
    const after = new Date();
    // Sanity check the shape and that it falls within [before, after].
    expect(ts).toMatch(/^\d{4}-\d{2}-\d{2}_\d{6}$/);
    expect(backupTimestamp(before) <= ts).toBe(true);
    expect(ts <= backupTimestamp(after)).toBe(true);
  });
});
