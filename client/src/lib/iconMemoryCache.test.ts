import { beforeEach, describe, expect, it } from "vitest";
import {
  clearIconMemoryCache,
  getCachedIcon,
  iconMemoryCacheStats,
  setCachedIcon,
} from "./iconMemoryCache";

/** A data URL of roughly `bytes` length. */
const url = (bytes: number) => `data:image/png;base64,${"A".repeat(bytes)}`;

describe("iconMemoryCache", () => {
  beforeEach(clearIconMemoryCache);

  it("returns what was stored", () => {
    setCachedIcon("app|10.0.0.1:9114|CUSA00900", url(10));
    expect(getCachedIcon("app|10.0.0.1:9114|CUSA00900")).toBe(url(10));
  });

  it("misses on an unknown key", () => {
    expect(getCachedIcon("nope")).toBeUndefined();
  });

  it("keeps consoles apart", () => {
    // The key carries the console, so the same title on a different PS5
    // must not resolve — one console's artwork under another's name is the
    // failure this app guards against everywhere else.
    setCachedIcon("app|10.0.0.1:9114|CUSA00900", url(10));
    expect(getCachedIcon("app|10.0.0.2:9114|CUSA00900")).toBeUndefined();
  });

  it("does not double-count when a key is replaced", () => {
    setCachedIcon("k", url(1000));
    const first = iconMemoryCacheStats().bytes;
    setCachedIcon("k", url(1000));
    expect(iconMemoryCacheStats().bytes).toBe(first);
    expect(iconMemoryCacheStats().count).toBe(1);
  });

  it("evicts oldest first once over budget", () => {
    // 24 MB budget; 5 MB entries, so the sixth pushes the first out.
    const five = 5 * 1024 * 1024;
    for (let i = 0; i < 6; i++) setCachedIcon(`k${i}`, url(five));
    expect(getCachedIcon("k0")).toBeUndefined();
    expect(getCachedIcon("k5")).toBeDefined();
    expect(iconMemoryCacheStats().bytes).toBeLessThanOrEqual(24 * 1024 * 1024);
  });

  it("skips an image larger than the whole budget rather than thrashing", () => {
    setCachedIcon("small", url(100));
    setCachedIcon("huge", url(25 * 1024 * 1024));
    expect(getCachedIcon("huge")).toBeUndefined();
    // and it did not evict everything else on the way
    expect(getCachedIcon("small")).toBeDefined();
  });

  it("ignores an empty value", () => {
    setCachedIcon("empty", "");
    expect(getCachedIcon("empty")).toBeUndefined();
    expect(iconMemoryCacheStats().count).toBe(0);
  });

  it("clears everything", () => {
    setCachedIcon("a", url(10));
    clearIconMemoryCache();
    expect(iconMemoryCacheStats()).toEqual({ count: 0, bytes: 0 });
  });
});
