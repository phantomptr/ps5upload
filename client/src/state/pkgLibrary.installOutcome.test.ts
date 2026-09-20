import { describe, expect, it } from "vitest";
import { installOutcomeKind } from "./pkgLibrary";

/* An install the PS5 accepted but whose completion we could not confirm is NOT
 * a failure. PS5 installs are async and the app DB lags the filesystem both
 * ways, so historically every "install failed" of this shape was a successful
 * install that simply had not registered yet. Rendering it red taught users to
 * distrust real failures. */
describe("installOutcomeKind", () => {
  it("treats a confirmed install as done", () => {
    expect(installOutcomeKind({ installed: true })).toBe("done");
  });

  it("treats accepted-but-unverified as its own kind, not a failure", () => {
    expect(
      installOutcomeKind({ installed: false, acceptedUnverified: true }),
    ).toBe("unverified");
  });

  it("still treats a stall as a stall", () => {
    expect(installOutcomeKind({ installed: false, stalled: true })).toBe(
      "stalled",
    );
  });

  it("treats everything else as a failure", () => {
    expect(installOutcomeKind({ installed: false })).toBe("failed");
  });

  it("prefers done over unverified when both are set", () => {
    expect(
      installOutcomeKind({ installed: true, acceptedUnverified: true }),
    ).toBe("done");
  });
});
