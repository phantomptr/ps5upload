import { describe, expect, it } from "vitest";
import { describeLinkDownload } from "./pkgLibrary";

/* "Download through this computer" showed nothing until the download finished. */
describe("describeLinkDownload", () => {
  it("shows percent, bytes, speed and time left", () => {
    const line = describeLinkDownload(512 * 1024 * 1024, 1024 * 1024 * 1024, 64 * 1024 * 1024);
    expect(line).toContain("Downloading to this computer — 50%");
    expect(line).toMatch(/at .*\/s/);
    expect(line).toContain("·");
  });

  it("copes with an unknown size and no rate yet", () => {
    expect(describeLinkDownload(1000, 0, 0)).toBe("Downloading to this computer — 1000 B");
  });
});
