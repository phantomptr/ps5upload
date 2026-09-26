import { describe, expect, it } from "vitest";

import { fmtSize, parentOf } from "./pathBrowser";

describe("parentOf", () => {
  it("walks up one level", () => {
    expect(parentOf("/storage/emulated/0/Download")).toBe(
      "/storage/emulated/0",
    );
    expect(parentOf("/storage/emulated/0")).toBe("/storage/emulated");
  });

  it("stops at the filesystem root", () => {
    expect(parentOf("/storage")).toBe("/");
    expect(parentOf("/")).toBeNull();
    expect(parentOf("")).toBeNull();
  });

  it("tolerates trailing slashes", () => {
    expect(parentOf("/storage/emulated/0/")).toBe("/storage/emulated");
  });

  it("handles a folder name with spaces (the field-report path)", () => {
    expect(parentOf("/storage/emulated/0/Download/ADM/Juegos ps5")).toBe(
      "/storage/emulated/0/Download/ADM",
    );
  });
});

describe("fmtSize", () => {
  it("formats bytes through terabytes (KB rendered as a whole number)", () => {
    expect(fmtSize(0)).toBe("0 B");
    expect(fmtSize(512)).toBe("512 B");
    expect(fmtSize(1024)).toBe("1 KB");
    expect(fmtSize(1536)).toBe("2 KB"); // KB tier rounds to integer
    expect(fmtSize(1024 * 1024)).toBe("1.0 MB");
    expect(fmtSize(2.5 * 1024 * 1024)).toBe("2.5 MB");
  });

  it("drops the decimal once it reaches double digits", () => {
    expect(fmtSize(85.29 * 1024 * 1024 * 1024)).toBe("85 GB");
  });
});
