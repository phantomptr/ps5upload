import { describe, expect, it } from "vitest";

import { fileSystemTargetFor } from "./searchLink";

const dirOf = (hit: Parameters<typeof fileSystemTargetFor>[0]) => fileSystemTargetFor(hit).dir;

describe("fileSystemTargetFor", () => {
  it("opens a folder hit at the folder itself", () => {
    expect(dirOf({ path: "/mnt/ext0/games/PPSA01234", kind: "dir" })).toBe(
      "/mnt/ext0/games/PPSA01234",
    );
  });

  it("opens a file hit at the folder that holds it", () => {
    expect(dirOf({ path: "/data/pkgs/Game Name.pkg", kind: "file" })).toBe(
      "/data/pkgs",
    );
  });

  it("opens a file at the root on /", () => {
    expect(dirOf({ path: "/eboot.bin", kind: "file" })).toBe("/");
  });
});

describe("fileSystemTargetFor", () => {
  it("selects a file hit inside its folder", () => {
    expect(fileSystemTargetFor({ path: "/data/pkgs/a.pkg", kind: "file" })).toEqual({
      dir: "/data/pkgs",
      select: "a.pkg",
    });
  });

  it("opens a folder hit itself, with nothing selected", () => {
    expect(fileSystemTargetFor({ path: "/data/pkgs/", kind: "dir" })).toEqual({ dir: "/data/pkgs" });
  });
});
