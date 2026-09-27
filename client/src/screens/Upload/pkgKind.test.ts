import { describe, expect, it } from "vitest";

import { pkgKindLabel } from "./pkgKind";

// The fallback is what renders without a translation, so it is what the test reads.
const tr = (_k: string, fallback: string) => fallback;

describe("pkgKindLabel (the Upload screen's Detected: line)", () => {
  it("names a PS4 package as PS4, not PS5", () => {
    expect(pkgKindLabel("ps4", tr)).toBe("PS4 package (.pkg)");
  });

  it("names a PS5 package as PS5", () => {
    expect(pkgKindLabel("ps5", tr)).toBe("PS5 package (.pkg)");
  });

  it("does not guess a console when the header gave none", () => {
    expect(pkgKindLabel("", tr)).toBe("Package (.pkg)");
    expect(pkgKindLabel(undefined, tr)).toBe("Package (.pkg)");
  });
});
