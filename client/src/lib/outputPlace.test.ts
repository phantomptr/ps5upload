import { describe, expect, it } from "vitest";

import { outputFor, parentDir, parseOutputPlace } from "./outputPlace";

describe("parentDir", () => {
  it("is the folder holding a game folder or a game image", () => {
    expect(parentDir("/games/My Game")).toBe("/games");
    expect(parentDir("/games/My Game/")).toBe("/games");
    expect(parentDir("/games/PPSA01234.exfat")).toBe("/games");
    expect(parentDir("/games/Game.ffpkg")).toBe("/games");
    expect(parentDir("~/Downloads/game.7z")).toBe("~/Downloads");
  });

  it("keeps Windows paths and roots", () => {
    expect(parentDir("D:\\Games\\Astro Bot")).toBe("D:\\Games");
    expect(parentDir("D:\\Games\\Astro Bot\\")).toBe("D:\\Games");
    expect(parentDir("D:\\Astro.exfat")).toBe("D:\\");
    expect(parentDir("D:/Astro.exfat")).toBe("D:/");
    expect(parentDir("/Game")).toBe("/");
  });

  it("has none for a bare name, a root, a server or the PS5", () => {
    expect(parentDir("Game")).toBeNull();
    expect(parentDir("/")).toBeNull();
    expect(parentDir("")).toBeNull();
    expect(parentDir("remote://nas/games/Game")).toBeNull();
    expect(parentDir("ps5://192.168.1.5/data/homebrew/Game")).toBeNull();
  });
});

describe("outputFor", () => {
  it("uses the Output folder by default", () => {
    expect(outputFor("/games/A", "folder", " /out ")).toEqual({ outputDir: "/out", keepBoth: false, beside: false });
    expect(outputFor("/games/A", "folder", "")).toEqual({ outputDir: undefined, keepBoth: false, beside: false });
  });

  it("puts each one next to its source, and keeps both when the name is taken", () => {
    // Two game folders in one place: both go beside them, never inside a game folder.
    expect(outputFor("/games/A", "beside", "/out")).toEqual({ outputDir: "/games", keepBoth: true, beside: true });
    expect(outputFor("/games/B/", "beside", "/out")).toEqual({ outputDir: "/games", keepBoth: true, beside: true });
    // A game image goes beside the image, in the folder that holds it.
    expect(outputFor("/images/B.exfat", "beside", "/out")).toEqual({
      outputDir: "/images",
      keepBoth: true,
      beside: true,
    });
  });

  it("falls back to the Output folder for a game with no folder here", () => {
    expect(outputFor("remote://nas/games/A", "beside", "/out")).toEqual({
      outputDir: "/out",
      keepBoth: false,
      beside: false,
    });
    expect(outputFor("ps5://10.0.0.2/data/A.exfat", "beside", "")).toEqual({
      outputDir: undefined,
      keepBoth: false,
      beside: false,
    });
  });

  it("parses the remembered choice", () => {
    expect(parseOutputPlace("beside")).toBe("beside");
    expect(parseOutputPlace("folder")).toBe("folder");
    expect(parseOutputPlace(null)).toBe("folder");
    expect(parseOutputPlace("junk")).toBe("folder");
  });
});
