import { beforeEach, describe, expect, it, vi } from "vitest";
import { clearDraft, defaultDraft, loadDraft, MIN_DESCRIPTION, platformFromHost, rangeStart, RANGE_MS, saveDraft, whatIsMissing } from "./draft";

const mem = new Map<string, string>();
beforeEach(() => {
  mem.clear();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => mem.get(k) ?? null,
    setItem: (k: string, v: string) => void mem.set(k, v),
    removeItem: (k: string) => void mem.delete(k),
  });
});

describe("bug report drafts", () => {
  it("round-trip per console", () => {
    const d = defaultDraft("6.5.2", "macOS (Apple Silicon)");
    d.form.whatHappened = "it broke";
    saveDraft("192.168.0.5", d);
    expect(loadDraft("192.168.0.5")?.form.whatHappened).toBe("it broke");
    expect(loadDraft("192.168.0.6")).toBeNull();
    clearDraft("192.168.0.5");
    expect(loadDraft("192.168.0.5")).toBeNull();
  });

  it("an unreadable store reads as no draft", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
      removeItem: () => {},
    });
    expect(loadDraft("x")).toBeNull();
    expect(() => saveDraft("x", defaultDraft("1", "Android"))).not.toThrow();
  });

  it("starts from the last 24 hours", () => {
    expect(defaultDraft("6.5.2", "Android").rangeKey).toBe("24h");
  });

  it("a draft saved by the old four-step wizard still loads, without its old fields", () => {
    const old = { ...defaultDraft("6.5.2", "Android"), step: 3, sources: ["app_log"], cats: [], everything: false, redact: false };
    old.form.whatHappened = "kept";
    mem.set("ps5upload.bugReportDraft.x", JSON.stringify(old));
    const d = loadDraft("x");
    expect(d?.form.whatHappened).toBe("kept");
    expect(Object.keys(d ?? {}).sort()).toEqual(["customStart", "form", "rangeKey"]);
  });
});

describe("rangeStart", () => {
  it("counts back from now, or uses the custom start", () => {
    const d = defaultDraft("1", "Android");
    d.rangeKey = "6h";
    expect(rangeStart(d, 100_000_000)).toBe(100_000_000 - RANGE_MS["6h"]);
    d.rangeKey = "custom";
    d.customStart = 42;
    expect(rangeStart(d, 100_000_000)).toBe(42);
    d.customStart = null;
    expect(rangeStart(d, 100_000_000)).toBe(100_000_000 - RANGE_MS["24h"]);
  });
});

describe("whatIsMissing", () => {
  it("names what still blocks the report, and nothing once it's there", () => {
    const d = defaultDraft("1", "Android");
    expect(whatIsMissing(d)).toBe("description");
    d.form.whatHappened = "x".repeat(MIN_DESCRIPTION - 1);
    expect(whatIsMissing(d)).toBe("description");
    d.form.whatHappened = "It disconnects";
    expect(whatIsMissing(d)).toBeNull();
    d.form.doing = "other";
    expect(whatIsMissing(d)).toBe("doing_other");
    d.form.doingOther = "Backporting";
    expect(whatIsMissing(d)).toBeNull();
  });

  it("a short but real sentence is enough", () => {
    const d = defaultDraft("1", "Android");
    d.form.whatHappened = "App froze";
    expect(whatIsMissing(d)).toBeNull();
  });
});

describe("platformFromHost", () => {
  it("names the issue form's option from the OS and CPU the desktop app reports", () => {
    expect(platformFromHost({ os: "macos", arch: "aarch64" })).toBe("macOS (Apple Silicon)");
    expect(platformFromHost({ os: "macos", arch: "x86_64" })).toBe("macOS (Intel)");
    expect(platformFromHost({ os: "windows", arch: "x86_64" })).toBe("Windows (x64)");
    expect(platformFromHost({ os: "windows", arch: "aarch64" })).toBe("Windows (ARM64)");
    expect(platformFromHost({ os: "linux", arch: "aarch64" })).toBe("Linux (ARM64)");
    expect(platformFromHost({ os: "android", arch: "aarch64" })).toBe("Android");
    expect(platformFromHost({ os: "freebsd", arch: "x86_64" })).toBeNull();
  });
});
