import { describe, expect, it } from "vitest";

import { gameFormatLabel, gameFormatTag } from "./gameFormatTag";

describe("the Files list's format tag", () => {
  it("names each game image and package by its extension", () => {
    expect(gameFormatTag("Astro.exfat", false)).toBe("exfat");
    expect(gameFormatTag("Astro.EXFAT", false)).toBe("exfat");
    expect(gameFormatTag("Astro.ffpfsc", false)).toBe("ffpfsc");
    expect(gameFormatTag("save.ffpfs", false)).toBe("ffpfs");
    expect(gameFormatTag("Astro.ffpkg", false)).toBe("ufs2");
    expect(gameFormatTag("EP0001-PPSA01342_00-X.pkg", false)).toBe("pkg");
    expect(gameFormatTag("notes.txt", false)).toBeNull();
    expect(gameFormatTag("game.pkg.part0", false)).toBeNull();
  });

  it("tags a folder only when its name carries a title id", () => {
    expect(gameFormatTag("PPSA01342", true)).toBe("folder");
    expect(gameFormatTag("CUSA12345-app", true)).toBe("folder");
    expect(gameFormatTag("Astro Bot [PPSA01342]", true)).toBe("folder");
    expect(gameFormatTag("homebrew", true)).toBeNull();
    // A file named like a folder is still a file without a game extension.
    expect(gameFormatTag("PPSA01342", false)).toBeNull();
  });

  it("writes format names as they are known and translates only the folder word", () => {
    expect(gameFormatLabel("exfat", "Folder")).toBe("exFAT");
    expect(gameFormatLabel("ufs2", "Folder")).toBe("UFS2");
    expect(gameFormatLabel("folder", "Dossier")).toBe("Dossier");
  });
});
