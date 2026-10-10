import { describe, expect, it } from "vitest";

import { formatDate } from "./formatDate";

// A fixed instant; assertions avoid the hour so the machine's time zone
// doesn't matter.
const when = Date.UTC(2026, 9, 9, 12, 0, 0);

describe("formatDate", () => {
  it("formats in the given app language, not the system one", () => {
    expect(formatDate(when, "date", "en")).toContain("Oct");
    expect(formatDate(when, "date", "de")).toContain("Okt");
    expect(formatDate(when, "date", "fr")).toMatch(/oct/i);
    expect(formatDate(when, "date", "ja")).toContain("2026年");
  });

  it("accepts a Date or epoch ms and gives the same text", () => {
    expect(formatDate(new Date(when), "datetime", "en")).toBe(formatDate(when, "datetime", "en"));
  });

  it("time includes seconds, time-short does not", () => {
    const t = formatDate(new Date(2026, 0, 1, 13, 4, 5), "time", "en");
    expect(t).toMatch(/:04:05/);
    expect(formatDate(new Date(2026, 0, 1, 13, 4, 5), "time-short", "en")).not.toMatch(/:05/);
  });

  it("is empty for an invalid date and survives an unknown language", () => {
    expect(formatDate(Number.NaN, "date", "en")).toBe("");
    expect(formatDate(when, "date", "xx-invalid-zz")).not.toBe("");
  });
});
