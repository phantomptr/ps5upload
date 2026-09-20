import { describe, expect, it } from "vitest";
import { installReverifyDelaysMs } from "./pkgLibrary";

/* The re-verify must be cheap and must not give up early. A 100 GiB install on
 * a slow internal drive is the case this exists for: the engine's grace window
 * ends in minutes, the console keeps writing for far longer. */
describe("installReverifyDelaysMs", () => {
  it("starts quickly", () => {
    expect(installReverifyDelaysMs(0)).toBe(30_000);
  });

  it("backs off but stays bounded", () => {
    expect(installReverifyDelaysMs(1)).toBe(60_000);
    expect(installReverifyDelaysMs(2)).toBe(120_000);
    expect(installReverifyDelaysMs(3)).toBe(300_000);
  });

  it("caps at five minutes however many attempts have passed", () => {
    expect(installReverifyDelaysMs(4)).toBe(300_000);
    expect(installReverifyDelaysMs(99)).toBe(300_000);
  });

  it("never returns a non-positive delay", () => {
    for (let i = 0; i < 50; i++) {
      expect(installReverifyDelaysMs(i)).toBeGreaterThan(0);
    }
  });
});
