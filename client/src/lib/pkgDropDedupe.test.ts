import { describe, expect, it } from "vitest";

import { acceptPkgDrop, PKG_DROP_DEDUPE_MS } from "./pkgDropDedupe";

describe("acceptPkgDrop", () => {
  it("accepts one physical drop only once during the route hand-off window", () => {
    const recent = new Map<string, number>();
    expect(acceptPkgDrop(recent, "/tmp/patch.pkg", 1_000)).toBe(true);
    expect(acceptPkgDrop(recent, "/tmp/patch.pkg", 1_100)).toBe(false);
  });

  it("allows an intentional later retry and independent package paths", () => {
    const recent = new Map<string, number>();
    expect(acceptPkgDrop(recent, "/tmp/a.pkg", 0)).toBe(true);
    expect(acceptPkgDrop(recent, "/tmp/b.pkg", 1)).toBe(true);
    expect(acceptPkgDrop(recent, "/tmp/a.pkg", PKG_DROP_DEDUPE_MS)).toBe(true);
  });
});
