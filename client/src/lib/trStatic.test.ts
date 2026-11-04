import { describe, expect, it } from "vitest";

import { trStatic } from "./trStatic";

describe("trStatic", () => {
  it("uses the catalogue text when the key exists", () => {
    expect(trStatic("copy_failed", "something else")).toBe("Copy failed");
  });
  it("falls back to the text at the call site", () => {
    expect(trStatic("no_such_key_for_this_test", "Fallback")).toBe("Fallback");
  });
  it("fills placeholders in the catalogue text and in the fallback", () => {
    expect(trStatic("fs_list_and_more", "x", { count: 3 })).toBe("…and 3 more");
    expect(trStatic("no_such_key_for_this_test", "{n} left", { n: 2 })).toBe("2 left");
  });
});
