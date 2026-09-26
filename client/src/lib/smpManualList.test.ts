import { describe, expect, it } from "vitest";

import { appendManualListLine, SMP_MANUAL_LIST_PATH } from "./smpManualList";

describe("appendManualListLine", () => {
  it("appends to an empty list", () => {
    expect(appendManualListLine("", "/data/homebrew/g.ffpkg")).toBe(
      "/data/homebrew/g.ffpkg\n",
    );
  });
  it("appends after existing content with a single trailing newline", () => {
    expect(appendManualListLine("/a\n/b\n", "/c")).toBe("/a\n/b\n/c\n");
    // collapses messy trailing whitespace
    expect(appendManualListLine("/a\n\n\n", "/c")).toBe("/a\n/c\n");
  });
  it("is idempotent — skips an exact path already listed (returns null)", () => {
    expect(appendManualListLine("/a\n/b\n", "/b")).toBeNull();
    expect(appendManualListLine("  /b  \n", "/b")).toBeNull(); // trimmed match
  });
  it("ignores comment + blank lines when deduping", () => {
    expect(appendManualListLine("# /b\n\n", "/b")).toBe("# /b\n/b\n");
  });
  it("rejects an empty path", () => {
    expect(appendManualListLine("/a\n", "   ")).toBeNull();
  });
  it("exposes the canonical list path", () => {
    expect(SMP_MANUAL_LIST_PATH).toBe("/data/shadowmount/manual.lst");
  });
});
