import { beforeEach, describe, expect, it, vi } from "vitest";
import { canAdvance, clearDraft, defaultDraft, loadDraft, rangeStart, RANGE_MS, saveDraft } from "./draft";

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

  it("starts from Everything", () => {
    const d = defaultDraft("6.5.2", "Android");
    expect(d.everything).toBe(true);
    expect(d.sources.length).toBeGreaterThan(5);
    expect(d.rangeKey).toBe("24h");
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

describe("canAdvance", () => {
  it("step 1 needs 20 characters and an Other description", () => {
    const d = defaultDraft("1", "Android");
    d.form.whatHappened = "x".repeat(19);
    expect(canAdvance(1, d)).toBe(false);
    d.form.whatHappened = "x".repeat(20);
    expect(canAdvance(1, d)).toBe(true);
    d.form.doing = "other";
    expect(canAdvance(1, d)).toBe(false);
    d.form.doingOther = "Cheats";
    expect(canAdvance(1, d)).toBe(true);
  });

  it("step 3 needs something to include", () => {
    const d = defaultDraft("1", "Android");
    expect(canAdvance(3, d)).toBe(true);
    d.sources = [];
    expect(canAdvance(3, d)).toBe(false);
  });

  it("step 2 is always fine", () => {
    expect(canAdvance(2, defaultDraft("1", "Android"))).toBe(true);
  });
});
