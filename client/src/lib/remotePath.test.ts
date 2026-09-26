import { describe, expect, it } from "vitest";

import { displayPath, isRemotePath, parseRemotePath, remotePath } from "./remotePath";

describe("remote paths", () => {
  it("recognises, parses and builds them", () => {
    expect(isRemotePath("remote://nas-1/games/a.pkg")).toBe(true);
    expect(isRemotePath("/Users/me/a.pkg")).toBe(false);
    expect(isRemotePath(null)).toBe(false);
    expect(parseRemotePath("remote://nas-1/games/a.pkg")).toEqual({
      connectionId: "nas-1",
      path: "/games/a.pkg",
    });
    expect(parseRemotePath("remote://nas-1")).toEqual({ connectionId: "nas-1", path: "/" });
    expect(parseRemotePath("C:\\games\\a.pkg")).toBeNull();
    expect(remotePath("nas-1", "/games/a b.pkg")).toBe("remote://nas-1/games/a b.pkg");
    expect(remotePath("nas-1", "games")).toBe("remote://nas-1/games");
  });

  it("shows a remote path by its server's name", () => {
    expect(displayPath("remote://nas-1/games/a.pkg", () => "NAS")).toBe("NAS › games/a.pkg");
    expect(displayPath("remote://nas-1/", () => "NAS")).toBe("NAS");
    expect(displayPath("remote://gone/x", () => undefined)).toBe("gone › x");
    expect(displayPath("/Users/me/a.pkg", () => "NAS")).toBe("/Users/me/a.pkg");
  });

  it("keeps a % in a file name intact", () => {
    // The engine percent-decodes paths, so a literal % must travel as %25.
    const p = remotePath("nas-1", "/games/Game [100%].pkg");
    expect(p).toBe("remote://nas-1/games/Game [100%25].pkg");
    expect(parseRemotePath(p)?.path).toBe("/games/Game [100%].pkg");
    expect(displayPath(p, () => "NAS")).toBe("NAS › games/Game [100%].pkg");
  });
});

