import { describe, expect, it } from "vitest";

import { resumableTxId, stagedTreeGone, type ListDir } from "./resumeGuard";

const ADDR = "192.168.1.10:9113";
const consoleWith =
  (folders: Record<string, string[]>): ListDir =>
  async (_addr, path) => {
    const names = folders[path];
    if (!names) throw new Error("fs_list_dir_not_found");
    return names.map((name) => ({ name }));
  };

describe("stagedTreeGone", () => {
  it("is false while the staging tree is there", async () => {
    const list = consoleWith({ "/data/homebrew": ["G", "G.ava-part"], "/data/homebrew/G": [] });
    expect(await stagedTreeGone(ADDR, "/data/homebrew/G", list)).toBe(false);
  });
  it("is true when the tree was deleted and only the empty folder holding its place is left", async () => {
    const list = consoleWith({ "/data/homebrew": ["G"], "/data/homebrew/G": [] });
    expect(await stagedTreeGone(ADDR, "/data/homebrew/G", list)).toBe(true);
  });
  it("is true when both are gone", async () => {
    expect(await stagedTreeGone(ADDR, "/data/homebrew/G", consoleWith({ "/data/homebrew": [] }))).toBe(true);
  });
  it("is false when the destination holds files: the tree landed, or this is a merge", async () => {
    const list = consoleWith({ "/data/homebrew": ["G"], "/data/homebrew/G": ["eboot.bin"] });
    expect(await stagedTreeGone(ADDR, "/data/homebrew/G", list)).toBe(false);
  });
  it("is false when the console does not answer: that is no reason to start over", async () => {
    expect(await stagedTreeGone(ADDR, "/data/homebrew/G", consoleWith({}))).toBe(false);
    const parentOnly = consoleWith({ "/data/homebrew": ["G"] });
    expect(await stagedTreeGone(ADDR, "/data/homebrew/G", parentOnly)).toBe(false);
  });
});

describe("resumableTxId", () => {
  it("keeps the id while the job's work is there, and mints a new one when it is gone", async () => {
    const fresh = () => "new";
    const here = consoleWith({ "/data": ["G.ava-part"] });
    expect(await resumableTxId(ADDR, "/data/G", "old", fresh, here)).toBe("old");
    expect(await resumableTxId(ADDR, "/data/G", "old", fresh, consoleWith({ "/data": [] }))).toBe("new");
  });
});
