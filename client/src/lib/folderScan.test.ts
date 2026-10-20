import { describe, expect, it, vi } from "vitest";

const listDir = vi.fn();
const remoteList = vi.fn();
vi.mock("../api/localFs", () => ({ localFs: { listDir: (p: string) => listDir(p) } }));
vi.mock("../api/remote", () => ({ remoteApi: { listDir: (p: string, c?: string) => remoteList(p, c) } }));

import { gameFoldersAmong, gamesInFolder, isGameFolder, scanChildren } from "./folderScan";

describe("Add games from a folder", () => {
  it("lists the folder's children, checking the playable ones", async () => {
    listDir.mockResolvedValue([
      { name: "Game A", path: "/g/Game A", is_dir: true, size: 0 },
      { name: "b.pkg", path: "/g/b.pkg", is_dir: false, size: 10 },
      { name: "notes.txt", path: "/g/notes.txt", is_dir: false, size: 1 },
    ]);
    expect(await scanChildren("/g")).toEqual([
      { path: "/g/Game A", isDir: true, size: null, include: true },
      { path: "/g/b.pkg", isDir: false, size: 10, include: true },
      { path: "/g/notes.txt", isDir: false, size: 1, include: false },
    ]);
  });

  it("reads a saved server's folder page by page", async () => {
    remoteList
      .mockResolvedValueOnce({ entries: [{ name: "x.7z", is_dir: false, size: 5 }], next_cursor: "c" })
      .mockResolvedValueOnce({ entries: [{ name: "G", is_dir: true, size: 0 }], next_cursor: null });
    const got = await scanChildren("remote://nas/dl");
    expect(got.map((e) => e.path)).toEqual(["remote://nas/dl/x.7z", "remote://nas/dl/G"]);
    expect(remoteList).toHaveBeenLastCalledWith("remote://nas/dl", "c");
  });
});

describe("Which picked folders are games", () => {
  /** A fake disk: folder → its entries. */
  const disk = (tree: Record<string, { name: string; is_dir: boolean }[]>) =>
    listDir.mockImplementation(async (p: string) => {
      const entries = tree[p];
      if (!entries) throw new Error(`no such folder: ${p}`);
      return entries.map((e) => ({ ...e, path: `${p}/${e.name}`, size: 1 }));
    });

  it("is a game when sce_sys holds param.json or param.sfo", async () => {
    disk({
      "/g/PS5": [{ name: "sce_sys", is_dir: true }],
      "/g/PS5/sce_sys": [{ name: "param.json", is_dir: false }],
      "/g/PS4": [{ name: "sce_sys", is_dir: true }],
      "/g/PS4/sce_sys": [{ name: "param.sfo", is_dir: false }],
      "/g/Empty": [{ name: "sce_sys", is_dir: true }],
      "/g/Empty/sce_sys": [{ name: "icon0.png", is_dir: false }],
      "/g/Dir": [{ name: "sce_sys", is_dir: true }],
      "/g/Dir/sce_sys": [{ name: "param.json", is_dir: true }],
    });
    expect(await isGameFolder("/g/PS5")).toBe(true);
    expect(await isGameFolder("/g/PS4")).toBe(true);
    expect(await isGameFolder("/g/Empty")).toBe(false);
    expect(await isGameFolder("/g/Dir")).toBe(false);
    expect(await isGameFolder("/g/Missing")).toBe(false);
  });

  it("reads a Windows folder with its own separator", async () => {
    listDir.mockImplementation(async (p: string) =>
      p === "C:\\games\\G\\sce_sys" ? [{ name: "param.json", is_dir: false, path: "x", size: 1 }] : [],
    );
    expect(await isGameFolder("C:\\games\\G")).toBe(true);
  });

  it("reads a saved server's sce_sys", async () => {
    remoteList.mockImplementation(async (p: string) =>
      p === "remote://nas/dl/G/sce_sys"
        ? { entries: [{ name: "param.json", is_dir: false, size: 1 }], next_cursor: null }
        : { entries: [], next_cursor: null },
    );
    expect(await isGameFolder("remote://nas/dl/G")).toBe(true);
    expect(await isGameFolder("remote://nas/dl/H")).toBe(false);
  });

  it("keeps the games among picked folders and counts the rest", async () => {
    disk({
      "/a/G1/sce_sys": [{ name: "param.json", is_dir: false }],
      "/a/sce_module": [],
      "/a/G2/sce_sys": [{ name: "param.sfo", is_dir: false }],
    });
    expect(await gameFoldersAmong(["/a/G1", "/a/sce_module", "/a/G2"])).toEqual({
      games: ["/a/G1", "/a/G2"],
      skipped: 1,
    });
  });
});

describe("Games in a picked folder", () => {
  const disk = (tree: Record<string, { name: string; is_dir: boolean }[]>) =>
    listDir.mockImplementation(async (p: string) => {
      const entries = tree[p];
      if (!entries) throw new Error(`no such folder: ${p}`);
      return entries.map((e) => ({ ...e, path: `${p}/${e.name}`, size: 1 }));
    });

  it("queues the picked folder itself when it is a game, not its sce_sys or sce_module", async () => {
    disk({
      "/g/Minecraft": [
        { name: "sce_sys", is_dir: true },
        { name: "sce_module", is_dir: true },
        { name: "eboot.bin", is_dir: false },
      ],
      "/g/Minecraft/sce_sys": [{ name: "param.json", is_dir: false }],
    });
    expect(await gamesInFolder("/g/Minecraft")).toEqual({
      sources: [{ path: "/g/Minecraft", isDir: true }],
      skipped: 0,
    });
  });

  it("otherwise takes the child games, images and archives, and skips other folders", async () => {
    disk({
      "/g": [
        { name: "Game A", is_dir: true },
        { name: "Backups", is_dir: true },
        { name: ".hidden", is_dir: true },
        { name: "b.ffpkg", is_dir: false },
        { name: "c.7z", is_dir: false },
        { name: "d.pkg", is_dir: false },
        { name: "notes.txt", is_dir: false },
      ],
      "/g/Game A": [{ name: "sce_sys", is_dir: true }],
      "/g/Game A/sce_sys": [{ name: "param.json", is_dir: false }],
      "/g/Backups": [{ name: "old", is_dir: true }],
    });
    expect(await gamesInFolder("/g")).toEqual({
      sources: [
        { path: "/g/Game A", isDir: true },
        { path: "/g/b.ffpkg", isDir: false },
        { path: "/g/c.7z", isDir: false },
      ],
      skipped: 1,
    });
  });
});


describe("Nested package conversion intake", () => {
  it("finds packages inside each game wrapper, without entering actual game folders", async () => {
    const tree: Record<string, string[]> = {
      "/games": ["007/", "Saros/", "Dump/"],
      "/games/007": ["game.pkg", "notes.txt"],
      "/games/Saros": ["game.pkg"],
      "/games/Dump/sce_sys": ["param.json"],
    };
    listDir.mockImplementation(async (p: string) => {
      if (!(p in tree)) throw new Error("not found");
      return tree[p].map((n) => ({ name: n.replace(/\/$/, ""), path: `${p}/${n.replace(/\/$/, "")}`, is_dir: n.endsWith("/"), size: 1 }));
    });
    expect(await gamesInFolder("/games")).toEqual({
      sources: [
        { path: "/games/007/game.pkg", isDir: false },
        { path: "/games/Saros/game.pkg", isDir: false },
        { path: "/games/Dump", isDir: true },
      ],
      skipped: 0,
    });
  });
});
