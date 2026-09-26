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

  it("strips the .zip extension for an archive source", () => {
    // A .zip is decompressed host-side; only its contents reach the PS5, so
    // the destination folder is named after the archive minus `.zip` — not
    // `.../MyGame.zip` (which would extract into a folder literally named
    // "MyGame.zip").
    const { dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/Users/me/MyGame.ZIP",
      true,
    );
    expect(dest).toBe("/data/homebrew/MyGame");
  });

  it("wraps an archive in a zip-named subfolder when archiveIntoSubfolder is true", () => {
    // Explicit default: contents land under <destRoot>/<zipname>.
    const { destRoot, dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/Users/me/MyGame.zip",
      true,
      true,
    );
    expect(destRoot).toBe("/data/homebrew");
    expect(dest).toBe("/data/homebrew/MyGame");
  });

  it("extracts an archive flat into destRoot when archiveIntoSubfolder is false", () => {
    // Flat mode: the zip's contents drop straight into the destination,
    // no wrapper folder — dest IS destRoot. For zips that already carry
    // the game's own top-level folder.
    const { destRoot, dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/Users/me/MyGame.zip",
      true,
      false,
    );
    expect(destRoot).toBe("/data/homebrew");
    expect(dest).toBe("/data/homebrew");
  });

  it("ignores archiveIntoSubfolder for non-archive sources", () => {
    // The flat flag only applies to archives; a folder always suffixes
    // its basename regardless.
    const { dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/Users/me/my-folder",
      false,
      false,
    );
    expect(dest).toBe("/data/homebrew/my-folder");
  });

  it("keeps a .zip name verbatim when the source is NOT an archive", () => {
    // Defensive: the same path uploaded as a plain file (isArchive omitted)
    // keeps its extension.
    const { dest } = resolveUploadDest(
      "/data",
      "homebrew",
      "/Users/me/save.zip",
    );
    expect(dest).toBe("/data/homebrew/save.zip");
  });

  it("normalizes subpaths — leading/trailing slashes don't leak", () => {
    const { dest } = resolveUploadDest(
      "/data",
      "/homebrew/games/",
      "/Users/me/FOO",
    );
    expect(dest).toBe("/data/homebrew/games/FOO");
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

describe("multi-part archives land in ONE folder", () => {
  // A game published as Game.part01.zip … Game.part06.zip is six SEPARATE
  // self-contained archives, each holding a different slice of the game's
  // files. Naming each destination after its own archive scattered the game
  // across six sibling folders and it could not launch.
  it("strips a .partNN marker from .zip parts so every part shares a folder", () => {
    const parts = [
      "C:/dl/[SITE]- 01.021 PPSA23226.part01.zip",
      "C:/dl/[SITE]- 01.021 PPSA23226.part02.zip",
      "C:/dl/[SITE]- 01.021 PPSA23226.part06.zip",
    ];
    const dests = parts.map(
      (p) => resolveUploadDest("/data", "homebrew", p, true, true).dest,
    );
    expect(new Set(dests).size).toBe(1);
    expect(dests[0]).toBe("/data/homebrew/[SITE]- 01.021 PPSA23226");
  });

  it("does the same for .7z and .rar parts", () => {
    expect(
      resolveUploadDest("/data", "homebrew", "/d/G.part1.7z", true, true).dest,
    ).toBe("/data/homebrew/G");
    expect(
      resolveUploadDest("/data", "homebrew", "/d/G.part1.rar", true, true).dest,
    ).toBe("/data/homebrew/G");
  });

  it("leaves a normal archive name alone", () => {
    expect(
      resolveUploadDest("/data", "homebrew", "/d/MyGame.zip", true, true).dest,
    ).toBe("/data/homebrew/MyGame");
    // "part" without digits is not a multi-part marker.
    expect(
      resolveUploadDest("/data", "homebrew", "/d/Counterpart.zip", true, true)
        .dest,
    ).toBe("/data/homebrew/Counterpart");
  });

  it("still honours flat extract (no wrapper folder at all)", () => {
    expect(
      resolveUploadDest("/data", "homebrew", "/d/G.part01.zip", true, false)
        .dest,
    ).toBe("/data/homebrew");
  });
});

// The whole point of the "Add all N parts to the queue" button: every part
// of a set has to resolve to ONE destination, or the parts land in six
// sibling folders and the game is unusable. Verified end-to-end against a
// real PS5, but pinned here so a future change to the `.partN` peel can't
// silently reintroduce the split.
describe("multi-part sets share one destination", () => {
  const dir = "C:\\Users\\Majid\\Downloads\\Black Myth Wukong";
  const parts = [
    `${dir}\\[DLPSGAME.COM]- 01.021 PPSA23226.part01.zip`,
    `${dir}\\[DLPSGAME.COM]- 01.021 PPSA23226.part02.zip`,
    `${dir}\\[DLPSGAME.COM]- 01.021 PPSA23226.part06.zip`,
  ];

  it("resolves every part to the same folder", () => {
    const dests = parts.map(
      (p) => resolveUploadDest("/data", "homebrew", p, true, true).dest,
    );
    expect(new Set(dests).size).toBe(1);
    expect(dests[0]).toBe("/data/homebrew/[DLPSGAME.COM]- 01.021 PPSA23226");
  });

  it("peels .partN for 7z and rar too", () => {
    for (const ext of ["zip", "7z", "rar"]) {
      const { dest } = resolveUploadDest(
        "/data",
        "homebrew",
        `/dl/Game.part03.${ext}`,
        true,
        true,
      );
      expect(dest).toBe("/data/homebrew/Game");
    }
  });
});
