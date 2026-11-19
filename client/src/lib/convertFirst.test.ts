import { describe, expect, it } from "vitest";

import { convertFirstState, takeHandOver } from "./convertFirst";

describe("convertFirstState", () => {
  it("hands one game over as `source`", () => {
    expect(convertFirstState([{ path: "/g/Game", isDir: true, kind: "game-folder" }])).toEqual({
      source: "/g/Game",
    });
  });

  it("hands several over as `sources`, leaving out packages, archives and plain files", () => {
    expect(
      convertFirstState([
        { path: "/g/A", isDir: true, kind: "folder" },
        { path: "/g/b.exfat", isDir: false, kind: "image" },
        { path: "/g/c.pkg", isDir: false, kind: "pkg" },
        { path: "/g/d.zip", isDir: false, kind: "archive" },
        { path: "/g/e.txt", isDir: false, kind: "file" },
        { path: "/g/f", isDir: true, kind: null },
      ]),
    ).toEqual({
      sources: [
        { path: "/g/A", isDir: true },
        { path: "/g/b.exfat", isDir: false },
      ],
    });
  });

  it("offers nothing when nothing is convertible", () => {
    expect(convertFirstState([{ path: "/g/c.pkg", isDir: false, kind: "pkg" }])).toBeNull();
    expect(convertFirstState([])).toBeNull();
  });
});

describe("takeHandOver", () => {
  it("lets one navigation's hand-over be taken once, however often the screen is shown again", () => {
    expect(takeHandOver("nav-a:many")).toBe(true);
    expect(takeHandOver("nav-a:many")).toBe(false);
    expect(takeHandOver("nav-a:many")).toBe(false);
    // Handing the same games over again is a new navigation.
    expect(takeHandOver("nav-b:many")).toBe(true);
  });
});
