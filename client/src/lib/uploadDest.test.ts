import { describe, expect, it } from "vitest";

import { basename, resolveUploadDest } from "./uploadDest";

describe("basename", () => {
  it("returns the final path segment for POSIX paths", () => {
    expect(basename("/Users/me/games/my-folder")).toBe("my-folder");
    expect(basename("/data/homebrew/file.pkg")).toBe("file.pkg");
  });

  it("tolerates Windows separators", () => {
    expect(basename("C:\\Users\\me\\my-folder")).toBe("my-folder");
    expect(basename("D:\\downloads\\game.exfat")).toBe("game.exfat");
  });

  it("strips trailing separators before extracting the name", () => {
    expect(basename("/Users/me/games/my-folder/")).toBe("my-folder");
    expect(basename("C:\\Users\\me\\thing\\")).toBe("thing");
  });

  it("handles a bare name with no separators", () => {
    expect(basename("solo")).toBe("solo");
  });

  it("handles root-only paths", () => {
    // A lone `/` trims to empty — no name to extract. Matches POSIX
    // `basename("/")` which returns "/" but for our purposes empty is
    // what `resolveUploadDest` wants (no trailing segment to append).
    expect(basename("/")).toBe("");
    expect(basename("///")).toBe("");
    expect(basename("")).toBe("");
  });

  it("handles Windows drive-root paths", () => {
    // `C:\\` (drive root) trims to `C:` — still a valid basename in
    // the rare case the user drags a whole drive. Not great UX but
    // not a crash.
    expect(basename("C:\\")).toBe("C:");
  });
});

describe("resolveUploadDest", () => {
  it("appends the source basename to a folder destination", () => {
    const { destRoot, dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/Users/me/my-folder",
    );
    expect(destRoot).toBe("/data/homebrew");
    expect(dest).toBe("/data/homebrew/my-folder");
  });

  it("appends the source basename for a single-file upload", () => {
    const { destRoot, dest } = resolveUploadDest(
      "/mnt/ext0",
      "exfat",
      "/Volumes/Media/map.exfat",
    );
    expect(destRoot).toBe("/mnt/ext0/exfat");
    expect(dest).toBe("/mnt/ext0/exfat/map.exfat");
  });

  it("normalizes subpaths — leading/trailing slashes don't leak", () => {
    const { dest } = resolveUploadDest(
      "/data",
      "/etaHEN/games/",
      "/Users/me/FOO",
    );
    expect(dest).toBe("/data/etaHEN/games/FOO");
  });

  it("falls back to /data when volume is null (preset 'auto')", () => {
    const { destRoot, dest } = resolveUploadDest(
      null,
      "homebrew",
      "/x/y/APP",
    );
    expect(destRoot).toBe("/data/homebrew");
    expect(dest).toBe("/data/homebrew/APP");
  });

  it("treats empty subpath as volume-root", () => {
    const { destRoot, dest } = resolveUploadDest(
      "/mnt/usb0",
      "",
      "/a/b/BAR",
    );
    expect(destRoot).toBe("/mnt/usb0");
    expect(dest).toBe("/mnt/usb0/BAR");
  });

  it("degrades to destRoot when the source has no basename", () => {
    // Defensive: pasted `/` as a source (unlikely via UI, but possible
    // via drag-drop of a root-level something) should not produce a
    // trailing-slash URL like `/data/homebrew/`.
    const { destRoot, dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/",
    );
    expect(destRoot).toBe("/data/homebrew");
    expect(dest).toBe("/data/homebrew");
  });

  it("strips many leading and trailing slashes in subpath", () => {
    const { dest } = resolveUploadDest(
      "/data",
      "///foo/bar///",
      "/x/y/z",
    );
    expect(dest).toBe("/data/foo/bar/z");
  });
});
