import { describe, expect, it } from "vitest";
import { compareVersions } from "./semver";

describe("compareVersions", () => {
  it("returns 0 for identical versions", () => {
    expect(compareVersions("2.2.3", "2.2.3")).toBe(0);
  });

  it("orders by major, minor, patch in that order", () => {
    expect(compareVersions("2.2.0", "2.2.3")).toBe(-1);
    expect(compareVersions("2.2.3", "2.2.0")).toBe(1);
    expect(compareVersions("2.1.9", "2.2.0")).toBe(-1);
    expect(compareVersions("3.0.0", "2.99.99")).toBe(1);
  });

  it("treats pre-release as less than the release", () => {
    expect(compareVersions("2.2.3-rc1", "2.2.3")).toBe(-1);
    expect(compareVersions("2.2.3", "2.2.3-rc1")).toBe(1);
  });

  it("orders pre-release strings lexicographically when both pre", () => {
    expect(compareVersions("2.2.3-rc1", "2.2.3-rc2")).toBe(-1);
    expect(compareVersions("2.2.3-rc2", "2.2.3-rc1")).toBe(1);
    expect(compareVersions("2.2.3-rc1", "2.2.3-rc1")).toBe(0);
  });

  it("accepts a leading 'v' on either side", () => {
    expect(compareVersions("v2.2.3", "2.2.3")).toBe(0);
    expect(compareVersions("v2.2.0", "v2.2.3")).toBe(-1);
  });

  it("returns null on unparseable input rather than guessing", () => {
    expect(compareVersions("two", "2.2.3")).toBeNull();
    expect(compareVersions("2.2", "2.2.3")).toBeNull();
    expect(compareVersions("", "2.2.3")).toBeNull();
  });
});
