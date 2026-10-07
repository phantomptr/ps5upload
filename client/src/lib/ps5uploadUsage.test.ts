import { describe, expect, it } from "vitest";

import { keptFolders, measureFolder, ps5uploadUsage, type DirEntry } from "./ps5uploadUsage";

/** A fake console filesystem: path -> entries. A missing path throws, like the helper. */
function fs(tree: Record<string, DirEntry[]>) {
  const calls: string[] = [];
  return {
    calls,
    list: async (path: string): Promise<DirEntry[]> => {
      calls.push(path);
      if (!(path in tree)) throw new Error("fs_list_dir_opendir_errno_2");
      return tree[path];
    },
  };
}
const file = (name: string, size: number): DirEntry => ({ name, kind: "file", size });
const dir = (name: string): DirEntry => ({ name, kind: "dir", size: 0 });

describe("measuring a folder on the console", () => {
  it("adds up every file under it, however deep", async () => {
    const f = fs({
      "/data/ps5upload/pkg_temp": [file("a.pkg", 100), dir("sub")],
      "/data/ps5upload/pkg_temp/sub": [file("b.pkg", 50), file("c.bin", 7)],
    });
    expect(await measureFolder(f.list, "/data/ps5upload/pkg_temp")).toEqual({
      exists: true,
      bytes: 157,
      files: 3,
      truncated: false,
    });
  });

  it("says a missing folder does not exist, and costs nothing", async () => {
    const f = fs({});
    expect(await measureFolder(f.list, "/data/ps5upload/tests")).toEqual({
      exists: false,
      bytes: 0,
      files: 0,
      truncated: false,
    });
  });

  it("stops at its limit and says the figure is a floor", async () => {
    const f = fs({ "/x": [file("1", 1), file("2", 1), file("3", 1), dir("d")], "/x/d": [file("4", 1)] });
    const m = await measureFolder(f.list, "/x", 3);
    expect(m.truncated).toBe(true);
    expect(m.files).toBe(3);
  });
});

describe("what ps5upload keeps on a console", () => {
  it("knows which folders it owns on each drive, and which are safe to empty", () => {
    const k = keptFolders(["/data", "/mnt/ext0"]);
    expect(k.find((x) => x.key === "pkg_temp" && x.drive === "/data")).toMatchObject({
      path: "/user/data/ps5upload/pkg_temp",
      cleanable: true,
    });
    // The package library holds the user's packages: shown, never offered for clean-up.
    expect(k.find((x) => x.key === "pkg_library" && x.drive === "/mnt/ext0")).toMatchObject({
      path: "/mnt/ext0/ps5upload/pkg_library",
      cleanable: false,
    });
    expect(k.find((x) => x.key === "backups")?.cleanable).toBe(false);
    expect(k.find((x) => x.key === "tests")?.cleanable).toBe(true);
    // Console-wide folders are listed once, under internal storage.
    expect(k.filter((x) => x.key === "backups")).toHaveLength(1);
  });

  it("reports only the folders that exist and hold something", async () => {
    const f = fs({
      "/user/data/ps5upload/pkg_temp": [file("half.pkg", 4096)],
      "/user/data/ps5upload/pkg_library": [],
      "/mnt/ext0/ps5upload/pkg_library": [file("game.pkg", 9000)],
    });
    const rows = await ps5uploadUsage(["/data", "/mnt/ext0"], f.list);
    expect(rows.map((r) => `${r.key}@${r.drive}=${r.bytes}`)).toEqual([
      "pkg_temp@/data=4096",
      "pkg_library@/mnt/ext0=9000",
    ]);
  });
});
