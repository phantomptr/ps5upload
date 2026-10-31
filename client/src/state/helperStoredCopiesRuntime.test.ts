import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { firstUpdateNotice } from "./helperStoredCopiesRuntime";

describe("firstUpdateNotice", () => {
  beforeEach(() => {
    const store = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
      },
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("says the helper was updated once per console and version", () => {
    expect(firstUpdateNotice("192.168.1.5", "10.0.4")).toBe(true);
    expect(firstUpdateNotice("192.168.1.5", "10.0.4")).toBe(false);
    expect(firstUpdateNotice("192.168.1.6", "10.0.4")).toBe(true);
    expect(firstUpdateNotice("192.168.1.5", "10.0.5")).toBe(true);
  });
});
