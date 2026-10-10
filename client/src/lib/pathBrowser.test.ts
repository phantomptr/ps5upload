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
  it("is the app's IEC formatter, so sizes read the same on every screen", () => {
    expect(fmtSize(0)).toBe("0 B");
    expect(fmtSize(512)).toBe("512 B");
    expect(fmtSize(1536)).toBe("1.50 KiB");
    expect(fmtSize(2.5 * 1024 * 1024)).toBe("2.50 MiB");
    expect(fmtSize(85.29 * 1024 * 1024 * 1024)).toBe("85.3 GiB");
  });
});
