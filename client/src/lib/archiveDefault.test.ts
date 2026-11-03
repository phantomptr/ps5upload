import { describe, expect, it } from "vitest";

import { archiveDefault } from "./archiveDefault";

const listing = (names: string[], complete = true, gameRoot: string | null = null) => ({
  game_root: gameRoot,
  entry_names: names,
  entry_names_complete: complete,
});

describe("archiveDefault (#645)", () => {
  it("sends a Y2JB-like jailbreak update as it is", () => {
    expect(archiveDefault(listing(["y2jb/", "y2jb/payload.js", "y2jb/main.elf"]))).toBe("as-is");
  });

  it("sends payloads and saves as they are", () => {
    expect(archiveDefault(listing(["kstuff.elf", "etaHEN.bin"]))).toBe("as-is");
    expect(archiveDefault(listing(["savedata/PPSA01234/sdimg_SAVEDATA00", "savedata/PPSA01234/icon0.png"]))).toBe("as-is");
  });

  it("unpacks a game the engine found (param.json game root)", () => {
    expect(archiveDefault(listing([], true, "MyGame"))).toBe("unpack");
    expect(archiveDefault(listing([], true, ""))).toBe("unpack");
  });

  it("unpacks a game by its sce_sys folder, at the top or one folder down", () => {
    expect(archiveDefault(listing(["sce_sys/icon0.png"]))).toBe("unpack");
    expect(archiveDefault(listing(["PPSA01234-app/sce_sys/icon0.png"]))).toBe("unpack");
    expect(archiveDefault(listing(["Game\\sce_sys\\icon0.png"]))).toBe("unpack");
    expect(archiveDefault(listing(["Game/sce_sys/"]))).toBe("unpack");
  });

  it("unpacks a game by eboot.bin or param.sfo/param.json, at the top or one folder down", () => {
    expect(archiveDefault(listing(["eboot.bin", "data.bin"]))).toBe("unpack");
    expect(archiveDefault(listing(["Game/EBOOT.BIN"]))).toBe("unpack");
    expect(archiveDefault(listing(["Game/param.sfo"]))).toBe("unpack");
    expect(archiveDefault(listing(["param.json"]))).toBe("unpack");
  });

  it("does not count game files deeper than one folder down", () => {
    expect(archiveDefault(listing(["a/b/eboot.bin", "a/b/c/sce_sys/icon0.png"]))).toBe("as-is");
  });

  it("does not count a file named sce_sys as the folder", () => {
    expect(archiveDefault(listing(["sce_sys"]))).toBe("as-is");
  });

  it("unpacks packages and disk images, at any depth", () => {
    for (const name of ["Game.pkg", "a/b/c/Game.ffpkg", "Game.exfat", "x/Game.ffpfs", "Game.FFPFSC"]) {
      expect(archiveDefault(listing([name]))).toBe("unpack");
    }
  });

  it("leaves an unknown listing unchosen", () => {
    expect(archiveDefault(null)).toBeNull();
    expect(archiveDefault(undefined)).toBeNull();
    // An older engine sends no listing.
    expect(archiveDefault({ game_root: null })).toBeNull();
    expect(archiveDefault({ game_root: null, entry_names: null, entry_names_complete: false })).toBeNull();
  });

  it("leaves a cut-off listing without a game unchosen, but unpacks one that shows a game", () => {
    expect(archiveDefault(listing(["a.txt", "b.txt"], false))).toBeNull();
    expect(archiveDefault(listing(["a.txt", "Game.pkg"], false))).toBe("unpack");
  });

  it("sends an empty archive as it is", () => {
    expect(archiveDefault(listing([]))).toBe("as-is");
  });
});
