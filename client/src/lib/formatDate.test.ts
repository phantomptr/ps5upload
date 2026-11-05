import { describe, expect, it } from "vitest";

import { formatAgo, formatDate, formatDuration, formatNumber, relativeAge } from "./formatDate";

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

describe("formatNumber", () => {
  it("groups digits the way the app language does", () => {
    expect(formatNumber(1234567, "en")).toBe("1,234,567");
    expect(formatNumber(1234567, "de")).toBe("1.234.567");
  });
  it("survives an unknown language tag", () => {
    expect(formatNumber(5, "not a tag!")).toBe("5");
  });
});

describe("formatDuration", () => {
  it("keeps the short letters in English", () => {
    expect(formatDuration(4, "en")).toBe("4s");
    expect(formatDuration(125, "en")).toBe("2m 5s");
    expect(formatDuration(3900, "en")).toBe("1h 5m");
  });
  it("uses the language's own units", () => {
    expect(formatDuration(3900, "ru")).toMatch(/^1\s*ч 5\s*м/);
  });
  it("gives a dash for nonsense", () => {
    expect(formatDuration(-1, "en")).toBe("—");
    expect(formatDuration(NaN, "en")).toBe("—");
  });
});

describe("formatAgo", () => {
  it("is empty for no time at all", () => {
    expect(formatAgo(0, "en")).toBe("");
  });
  it("says it in the app language, not in English letters", () => {
    expect(formatAgo(Date.now() - 5 * 60_000, "en")).toBe("5 minutes ago");
    expect(formatAgo(Date.now() - 5 * 60_000, "de")).toBe("vor 5 Minuten");
    expect(formatAgo(Date.now() - 1000, "en")).toBe("now");
  });
  it("gives the date once over a week old", () => {
    const old = Date.now() - 30 * 86_400_000;
    expect(formatAgo(old, "en")).toBe(formatDate(old, "date", "en"));
  });
});

describe("relativeAge", () => {
  it("picks the largest whole unit", () => {
    expect(relativeAge(3 * 3600, "en")).toBe("3 hours ago");
    expect(relativeAge(30, "en")).toBe("30 seconds ago");
  });
});
