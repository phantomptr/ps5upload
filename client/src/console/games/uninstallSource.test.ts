import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import type { CallFn } from "./model";
import { deletable, deleteSource, planSource, resolveSource } from "./uninstallSource";

const app = (titleId: string, source: string) => ({ titleId, source });

describe("what an uninstall may delete (the engine's rules)", () => {
  it("a folder game: its folder; an image game: the image and its mount", () => {
    expect(resolveSource("ppsa00001", [app("PPSA00001", "/data/homebrew/Game/")], [])).toEqual({
      kind: "folder",
      path: "/data/homebrew/Game",
      mountPoint: null,
    });
    const vols = [{ path: "/mnt/ps5upload/Game", source_image: "/mnt/ext1/games/Game.ffpkg" }];
    expect(resolveSource("PPSA00002", [app("PPSA00002", "/mnt/ps5upload/Game/x")], vols)).toEqual({
      kind: "image",
      path: "/mnt/ext1/games/Game.ffpkg",
      mountPoint: "/mnt/ps5upload/Game",
    });
  });

  it("nothing for a package install, a ShadowMount+ title, an unknown image or a shared source", () => {
    expect(resolveSource("PPSA00003", [], [])).toEqual({ reason: "not_registered" });
    expect(resolveSource("PPSA00003", [app("PPSA00003", "/mnt/shadowmnt/G_1/x")], [])).toEqual({ reason: "shadowmount" });
    expect(resolveSource("PPSA00003", [app("PPSA00003", "/mnt/ps5upload/G/x")], [])).toEqual({ reason: "image_not_mounted" });
    const shared = [app("PPSA00004", "/mnt/usb0/Games"), app("PPSA00005", "/mnt/usb0/Games/B")];
    expect(resolveSource("PPSA00004", shared, [])).toEqual({ reason: "shared_folder" });
  });

  it("never a drive root, a top folder of /data, or the console's own folders", () => {
    for (const p of ["/data", "/data/homebrew", "/mnt/usb0", "/mnt/ext1/user/app/X", "/data/ps5upload/x", "/user/app/X", "/data/a/../b"])
      expect(deletable(p), p).toBe(false);
    expect(deletable("/mnt/usb0/Game")).toBe(true);
    expect(deletable("/data/homebrew/Game")).toBe(true);
  });
});

/** A console with one folder game at /mnt/usb0/Game and `tree` below it. */
function consoleWith(tree: Record<string, { name: string; kind: string; size?: number }[]>, extra: Record<string, unknown> = {}) {
  const calls: [string, unknown][] = [];
  const fn = vi.fn(async (route: string, body?: object) => {
    calls.push([route, body]);
    if (route in extra) return extra[route];
    if (route === "/api/apps/registered") return { reply: { apps: [{ title_id: "PPSA00001", src: "/mnt/usb0/Game" }] } };
    if (route === "/api/volumes") return { volumes: [] };
    if (route === "/api/fs/stat") {
      const p = (body as { path: string }).path;
      if (p === "/mnt/usb0/Game") return { kind: "dir", size: 0 };
      if (p === "/mnt/usb0/Game/sce_sys/param.json") return { kind: "file", size: 10 };
      return { missing: true };
    }
    throw new ApiError(404, "unknown_route");
  }) as unknown as CallFn;
  const list = async (path: string) => ({ entries: (tree[path] ?? []).map((e) => ({ size: 0, mtime: null, ...e })) as never });
  return { fn, calls, walk: { list, now: () => 0 } };
}

describe("planSource", () => {
  it("a folder game: its path and the bytes below it", async () => {
    const c = consoleWith({
      "/mnt/usb0/Game": [
        { name: "sce_sys", kind: "dir" },
        { name: "eboot.bin", kind: "file", size: 1000 },
      ],
      "/mnt/usb0/Game/sce_sys": [{ name: "param.json", kind: "file", size: 24 }],
    });
    expect(await planSource("PPSA00001", c.fn, c.walk)).toEqual({
      kind: "folder",
      path: "/mnt/usb0/Game",
      mountPoint: null,
      bytes: 1024,
      partial: false,
    });
  });

  it("a folder holding another game, or a folder already gone: nothing to ask", async () => {
    const nested = consoleWith({
      "/mnt/usb0/Game": [
        { name: "sce_sys", kind: "dir" },
        { name: "Other", kind: "dir" },
      ],
      "/mnt/usb0/Game/Other": [{ name: "sce_sys", kind: "dir" }],
    });
    expect(await planSource("PPSA00001", nested.fn, nested.walk)).toBeNull();
    const gone = consoleWith({}, { "/api/fs/stat": { missing: true } });
    expect(await planSource("PPSA00001", gone.fn, gone.walk)).toBeNull();
  });
});

describe("deleteSource", () => {
  it("unmounts an image before deleting it, and waits for the delete", async () => {
    const calls: string[] = [];
    const fn = vi.fn(async (route: string) => {
      calls.push(route);
      return route === "/api/fs/delete" ? { job: "j1" } : {};
    }) as unknown as CallFn;
    const wait = vi.fn(async () => undefined);
    await deleteSource({ kind: "image", path: "/data/homebrew/G.exfat", mountPoint: "/mnt/ps5upload/G", bytes: 1, partial: false }, fn, wait);
    expect(calls).toEqual(["/api/volumes/unmount", "/api/fs/delete"]);
    expect(wait).toHaveBeenCalledWith("j1");
  });

  it("a failed unmount deletes nothing", async () => {
    const calls: string[] = [];
    const fn = vi.fn(async (route: string) => {
      calls.push(route);
      if (route === "/api/volumes/unmount") throw new ApiError(409, "game_running");
      return { job: "j1" };
    }) as unknown as CallFn;
    await expect(
      deleteSource({ kind: "image", path: "/data/homebrew/G.exfat", mountPoint: "/mnt/ps5upload/G", bytes: 1, partial: false }, fn, async () => undefined),
    ).rejects.toThrow();
    expect(calls).toEqual(["/api/volumes/unmount"]);
  });
});
