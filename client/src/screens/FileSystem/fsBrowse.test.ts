import { describe, expect, it } from "vitest";

import { fsKeyAction, nextSort, normalizeTypedPath, sortEntries } from "./fsBrowse";

const key = (k: string, mods: Partial<{ ctrl: boolean; meta: boolean; alt: boolean }> = {}) => ({
  key: k,
  ctrlKey: !!mods.ctrl,
  metaKey: !!mods.meta,
  altKey: !!mods.alt,
  shiftKey: false,
});

describe("sortEntries", () => {
  const list = [
    { name: "b.pkg", kind: "file", size: 30, mtime: 3 },
    { name: "Game10", kind: "dir", size: 0, mtime: 1 },
    { name: "a.bin", kind: "file", size: 10, mtime: 9 },
    { name: "Game2", kind: "dir", size: 0, mtime: 5 },
  ];

  it("puts folders first and orders names naturally", () => {
    expect(sortEntries(list, { key: "name", desc: false }).map((e) => e.name)).toEqual([
      "Game2",
      "Game10",
      "a.bin",
      "b.pkg",
    ]);
  });

  it("sorts by size and by date, either way, folders still first", () => {
    expect(sortEntries(list, { key: "size", desc: true }).map((e) => e.name)).toEqual([
      "Game2",
      "Game10",
      "b.pkg",
      "a.bin",
    ]);
    expect(sortEntries(list, { key: "mtime", desc: false }).map((e) => e.name)).toEqual([
      "Game10",
      "Game2",
      "b.pkg",
      "a.bin",
    ]);
  });

  it("flips the order on the same column, starts ascending on a new one", () => {
    expect(nextSort({ key: "name", desc: false }, "name")).toEqual({ key: "name", desc: true });
    expect(nextSort({ key: "name", desc: true }, "size")).toEqual({ key: "size", desc: false });
  });
});

describe("fsKeyAction", () => {
  it("maps the clipboard and selection shortcuts with Ctrl or Cmd", () => {
    expect(fsKeyAction(key("c", { ctrl: true }))).toBe("copy");
    expect(fsKeyAction(key("X", { meta: true }))).toBe("cut");
    expect(fsKeyAction(key("v", { ctrl: true }))).toBe("paste");
    expect(fsKeyAction(key("a", { meta: true }))).toBe("select-all");
    expect(fsKeyAction(key("Backspace", { meta: true }))).toBe("delete");
  });

  it("maps the plain keys FileZilla users expect", () => {
    expect(fsKeyAction(key("Delete"))).toBe("delete");
    expect(fsKeyAction(key("F2"))).toBe("rename");
    expect(fsKeyAction(key("F5"))).toBe("refresh");
    expect(fsKeyAction(key("Backspace"))).toBe("up");
    expect(fsKeyAction(key("Enter"))).toBe("open");
    expect(fsKeyAction(key("Escape"))).toBe("clear");
    expect(fsKeyAction(key("q"))).toBeNull();
    expect(fsKeyAction(key("c", { alt: true }))).toBeNull();
  });
});

describe("normalizeTypedPath", () => {
  it("accepts absolute paths and tidies slashes", () => {
    expect(normalizeTypedPath(" /data//homebrew/ ")).toBe("/data/homebrew");
    expect(normalizeTypedPath("/")).toBe("/");
    expect(normalizeTypedPath("data")).toBeNull();
  });
});
