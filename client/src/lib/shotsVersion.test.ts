import { describe, expect, it } from "vitest";

import { nextPatch, shotsVersion } from "../../site-shots/shotsVersion";
// The file the spec reads (the repo's version).
import repo from "../../../VERSION?raw";

// The website's screenshots (site-shots/site-shots.spec.ts) show the release they are taken for.
describe("the version the website's screenshots show", () => {
  it("is the repo's version plus one patch level by default", () => {
    expect(nextPatch("12.6.31\n")).toBe("12.6.32");
    expect(nextPatch("v9.0.9")).toBe("9.0.10");
    expect(nextPatch("12.7.0-beta.1")).toBe("12.7.1");
    expect(shotsVersion(undefined, "12.6.31\n")).toBe("12.6.32");
    expect(shotsVersion("  ", "12.6.31")).toBe("12.6.32");
    // The repo's own VERSION file.
    expect(shotsVersion(undefined, repo)).toBe(nextPatch(repo));
  });

  it("is SHOTS_VERSION when it is given", () => {
    expect(shotsVersion("12.7.0", "12.6.31")).toBe("12.7.0");
    expect(shotsVersion("v13.0.0", "12.6.31")).toBe("13.0.0");
  });

  it("refuses what is not a version, rather than show it", () => {
    expect(() => shotsVersion("next", "12.6.31")).toThrow(/SHOTS_VERSION/);
    expect(() => nextPatch("")).toThrow();
  });
});
