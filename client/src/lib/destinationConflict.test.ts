import { describe, expect, it, vi } from "vitest";

import {
  clearForReplace,
  destInFolder,
  destinationConflictOf,
  offersConflictActions,
  type ReplaceDeps,
} from "./destinationConflict";

describe("which failures offer Replace / Upload somewhere else", () => {
  it("maps the two destination-taken reasons, nothing else", () => {
    expect(destinationConflictOf("ava1_exists")).toBe("before");
    expect(destinationConflictOf("ava1_commit_exists")).toBe("after");
    expect(destinationConflictOf("ava1_commit_cross_device")).toBeNull();
    expect(destinationConflictOf("ava1_no_space")).toBeNull();
    expect(destinationConflictOf(null)).toBeNull();
  });

  it("only on a failed upload row, never an install or a console-to-console copy", () => {
    const base = { status: "failed", sourceKind: "folder", errorReason: "ava1_commit_exists" } as const;
    expect(offersConflictActions(base)).toBe(true);
    expect(offersConflictActions({ ...base, sourceKind: "pkg", errorReason: "ava1_exists" })).toBe(true);
    expect(offersConflictActions({ ...base, status: "pending" })).toBe(false);
    expect(offersConflictActions({ ...base, sourceKind: "install" })).toBe(false);
    expect(offersConflictActions({ ...base, sourceKind: "ps5" })).toBe(false);
    expect(offersConflictActions({ ...base, errorReason: "ava1_unreachable" })).toBe(false);
  });

  it("keeps the name when it goes to another folder", () => {
    expect(destInFolder("/data/homebrew/Game", "/mnt/ext1/homebrew")).toBe("/mnt/ext1/homebrew/Game");
    expect(destInFolder("/data/pkg/a.pkg", "/data/other/")).toBe("/data/other/a.pkg");
    expect(destInFolder("/data/Game", "/")).toBe("/Game");
  });

  it("is the folder itself for an archive extracted straight into it", () => {
    expect(destInFolder("/data/homebrew/Game", "/data/homebrew", true)).toBe("/data/homebrew");
    expect(destInFolder("/data/homebrew/Game", "/mnt/ext1/games/", true)).toBe("/mnt/ext1/games");
    expect(destInFolder("/data/homebrew", "/", true)).toBe("/");
  });
});

/** A console holding `paths` (folders end in `/`). */
function console_(paths: string[]) {
  const have = new Set(paths);
  const calls: string[] = [];
  const deps: ReplaceDeps = {
    listDir: vi.fn(async (_addr: string, path: string) => {
      if (!have.has(`${path}/`) && path !== "/data/homebrew") throw new Error("fs_list_not_dir");
      const pre = path === "/" ? "/" : `${path}/`;
      return [...have]
        .filter((p) => p.startsWith(pre) && p !== pre)
        .map((p) => p.slice(pre.length).replace(/\/$/, ""))
        .filter((n) => n && !n.includes("/"))
        .map((name) => ({ name }));
    }),
    remove: vi.fn(async (_addr: string, path: string) => {
      calls.push(`rm ${path}`);
      if (![...have].some((p) => p === path || p === `${path}/`)) throw new Error("fs_delete_not_found");
      for (const p of [...have]) if (p === path || p.startsWith(`${path}/`)) have.delete(p);
    }),
    move: vi.fn(async (_addr: string, from: string, to: string) => {
      calls.push(`mv ${from} ${to}`);
    }),
    forgetResumeId: vi.fn(async () => {
      calls.push("forget");
    }),
  };
  return { deps, calls };
}

const ADDR = "192.168.1.10:9113";

describe("Replace clears the way", () => {
  it("a folder that all arrived: deletes what is there and moves the finished tree in", async () => {
    const { deps, calls } = console_([
      "/data/homebrew/",
      "/data/homebrew/Game/",
      "/data/homebrew/Game/old.bin",
      "/data/homebrew/Game.ava-part/",
      "/data/homebrew/Game.ava-part/eboot.bin",
    ]);
    const r = await clearForReplace(
      { addr: ADDR, sourceKind: "game-folder", sourcePath: "/src/Game", resolvedDest: "/data/homebrew/Game", errorReason: "ava1_commit_exists" },
      deps,
    );
    expect(r).toEqual({ placed: true });
    expect(calls).toEqual(["rm /data/homebrew/Game", "mv /data/homebrew/Game.ava-part /data/homebrew/Game"]);
  });

  it("refused before sending: deletes what is there, and the row is sent again", async () => {
    const { deps, calls } = console_(["/data/homebrew/", "/data/homebrew/Game/"]);
    const r = await clearForReplace(
      { addr: ADDR, sourceKind: "folder", sourcePath: "/src/Game", resolvedDest: "/data/homebrew/Game", errorReason: "ava1_exists" },
      deps,
    );
    expect(r).toEqual({ placed: false });
    expect(calls[0]).toBe("rm /data/homebrew/Game");
    expect(deps.move).not.toHaveBeenCalled();
    expect(deps.forgetResumeId).not.toHaveBeenCalled();
  });

  it("a single file: deletes it and its part, and forgets the remembered job", async () => {
    const { deps, calls } = console_([
      "/data/homebrew/",
      "/data/homebrew/a.pkg/",
      "/data/homebrew/a.pkg.ava-part",
    ]);
    const r = await clearForReplace(
      { addr: ADDR, sourceKind: "pkg", sourcePath: "/src/a.pkg", resolvedDest: "/data/homebrew/a.pkg", errorReason: "ava1_commit_exists" },
      deps,
    );
    expect(r).toEqual({ placed: false });
    expect(calls).toEqual(["rm /data/homebrew/a.pkg", "rm /data/homebrew/a.pkg.ava-part", "forget"]);
  });

  it("a move that fails sends it again, with the leftover removed", async () => {
    const { deps, calls } = console_([
      "/data/homebrew/",
      "/data/homebrew/Game/",
      "/data/homebrew/Game.ava-part/",
    ]);
    vi.mocked(deps.move).mockRejectedValueOnce(new Error("fs_move_failed"));
    const r = await clearForReplace(
      { addr: ADDR, sourceKind: "folder", sourcePath: "/src/Game", resolvedDest: "/data/homebrew/Game", errorReason: "ava1_commit_exists" },
      deps,
    );
    expect(r).toEqual({ placed: false });
    expect(calls).toContain("rm /data/homebrew/Game.ava-part");
  });

  it("a delete the console refuses throws, with nothing else touched", async () => {
    const { deps } = console_(["/data/homebrew/", "/data/homebrew/Game/", "/data/homebrew/Game.ava-part/"]);
    vi.mocked(deps.remove).mockRejectedValueOnce(new Error("fs_delete_failed"));
    await expect(
      clearForReplace(
        { addr: ADDR, sourceKind: "folder", sourcePath: "/src/Game", resolvedDest: "/data/homebrew/Game", errorReason: "ava1_commit_exists" },
        deps,
      ),
    ).rejects.toThrow("fs_delete_failed");
    expect(deps.move).not.toHaveBeenCalled();
  });
});
