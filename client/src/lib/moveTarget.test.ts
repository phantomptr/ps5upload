import { describe, expect, it } from "vitest";
import {
  defaultMoveSubpath,
  detectSourceVolume,
  isInvalidName,
  isMoveNoop,
  resolveMoveDestination,
  sourceBasename,
} from "./moveTarget";

describe("detectSourceVolume", () => {
  it("matches the longest writable volume prefix", () => {
    const vols = ["/data", "/mnt/ext1", "/mnt/usb0"];
    expect(detectSourceVolume("/mnt/ext1/games/MyGame", vols)).toBe(
      "/mnt/ext1",
    );
    expect(detectSourceVolume("/data/homebrew/foo", vols)).toBe("/data");
  });

  it("trims trailing slash from volume entries before matching", () => {
    const vols = ["/data/", "/mnt/ext1/"];
    expect(detectSourceVolume("/mnt/ext1/x", vols)).toBe("/mnt/ext1");
  });

  it("falls back to first volume when nothing matches", () => {
    expect(detectSourceVolume("/somewhere/else", ["/data", "/mnt/ext1"])).toBe(
      "/data",
    );
  });

  it("returns null only when volumes list is empty", () => {
    expect(detectSourceVolume("/data/foo", [])).toBeNull();
  });
});

describe("defaultMoveSubpath", () => {
  it("returns the parent dir relative to the volume", () => {
    expect(defaultMoveSubpath("/mnt/ext1/games/MyGame", "/mnt/ext1")).toBe(
      "games",
    );
    expect(
      defaultMoveSubpath("/data/homebrew/etaHEN/games/foo", "/data"),
    ).toBe("homebrew/etaHEN/games");
  });

  it("returns empty string when entry is directly under volume root", () => {
    expect(defaultMoveSubpath("/mnt/ext1/lone-game", "/mnt/ext1")).toBe("");
  });

  it("returns empty string when entry path does not start with the volume", () => {
    expect(defaultMoveSubpath("/mnt/ext1/games/x", "/data")).toBe("");
  });
});

describe("resolveMoveDestination", () => {
  it("appends source basename to the resolved root", () => {
    expect(
      resolveMoveDestination("/mnt/ext1", "games", "/data/homebrew/MyGame"),
    ).toBe("/mnt/ext1/games/MyGame");
  });

  it("strips trailing slash from source so basename is the dir name", () => {
    expect(
      resolveMoveDestination(
        "/mnt/ext1",
        "games",
        "/data/homebrew/MyGame/",
      ),
    ).toBe("/mnt/ext1/games/MyGame");
  });

  it("trims leading and trailing slashes from subpath", () => {
    expect(
      resolveMoveDestination("/data", "/homebrew/", "/data/games/Foo"),
    ).toBe("/data/homebrew/Foo");
  });

  it("places under volume root when subpath is empty", () => {
    expect(resolveMoveDestination("/mnt/ext1", "", "/data/X.exfat")).toBe(
      "/mnt/ext1/X.exfat",
    );
  });
});

describe("resolveMoveDestination with custom name", () => {
  it("uses custom name when supplied", () => {
    expect(
      resolveMoveDestination(
        "/mnt/ext1",
        "games",
        "/data/homebrew/MyGame",
        "RenamedGame",
      ),
    ).toBe("/mnt/ext1/games/RenamedGame");
  });

  it("falls back to source basename when custom name is empty", () => {
    expect(
      resolveMoveDestination("/mnt/ext1", "games", "/data/X", ""),
    ).toBe("/mnt/ext1/games/X");
  });

  it("treats whitespace-only custom name as empty", () => {
    expect(
      resolveMoveDestination("/mnt/ext1", "games", "/data/X", "   "),
    ).toBe("/mnt/ext1/games/X");
  });

  it("trims surrounding slashes from custom name", () => {
    expect(
      resolveMoveDestination("/mnt/ext1", "games", "/data/X", "//Foo//"),
    ).toBe("/mnt/ext1/games/Foo");
  });
});

describe("sourceBasename", () => {
  it("returns the trailing component", () => {
    expect(sourceBasename("/mnt/ext1/games/MyGame")).toBe("MyGame");
  });
  it("ignores trailing slashes", () => {
    expect(sourceBasename("/mnt/ext1/games/MyGame/")).toBe("MyGame");
  });
});

describe("isInvalidName", () => {
  it("accepts a normal segment", () => {
    expect(isInvalidName("MyGame")).toBe(false);
    expect(isInvalidName("title with spaces")).toBe(false);
  });
  it("accepts empty (caller falls back to source basename)", () => {
    expect(isInvalidName("")).toBe(false);
  });
  it("rejects names with slashes", () => {
    expect(isInvalidName("foo/bar")).toBe(true);
    expect(isInvalidName("foo\\bar")).toBe(true);
  });
  it("rejects relative-path tokens", () => {
    expect(isInvalidName(".")).toBe(true);
    expect(isInvalidName("..")).toBe(true);
  });
});

describe("isMoveNoop", () => {
  it("detects identical source and destination", () => {
    expect(isMoveNoop("/data/homebrew/Foo", "/data/homebrew/Foo")).toBe(true);
  });

  it("ignores trailing slash differences", () => {
    expect(isMoveNoop("/data/homebrew/Foo/", "/data/homebrew/Foo")).toBe(true);
  });

  it("returns false for genuinely different paths", () => {
    expect(isMoveNoop("/data/homebrew/Foo", "/mnt/ext1/games/Foo")).toBe(false);
  });
});
