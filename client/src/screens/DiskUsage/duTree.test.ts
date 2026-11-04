import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
let replies: Array<unknown> = [];
let busyStarts = 0;
vi.mock("../../lib/invokeLogged", () => ({
  invoke: vi.fn(async (cmd: string, args: Record<string, unknown>) => {
    calls.push({ cmd, args });
    if (cmd === "ps5_disk_usage_cancel") return {};
    if (cmd === "ps5_disk_usage_start" && busyStarts > 0) {
      busyStarts--;
      throw new Error("engine HTTP 409 Conflict: disk_usage_busy");
    }
    return replies.shift();
  }),
}));

import {
  blocksOf,
  cachedFolder,
  clearWalkCache,
  findSubtree,
  forgetWalks,
  rememberWalk,
  isWalkBusy,
  runWalk,
  sleep,
  POLL_MS,
  type DuDir,
  type DuStatus,
} from "./duTree";

const dir = (name: string, bytes: number, over: Partial<DuDir> = {}): DuDir => ({
  name,
  bytes,
  files: 0,
  dirs: 0,
  skipped: 0,
  children: [],
  top_files: [],
  more_files: 0,
  more_bytes: 0,
  ...over,
});

// /data: games (a: 110, b: 1000), locked (unreadable), top.bin 5.
const tree = dir("data", 1115, {
  children: [
    dir("games", 1110, {
      children: [dir("b", 1000), dir("a", 110, { children: [dir("sce_sys", 10)] })],
    }),
    dir("locked", 0, { note: "unreadable", skipped: 1 }),
  ],
  top_files: [{ name: "top.bin", size: 5 }],
});

const status = (over: Partial<DuStatus> = {}): DuStatus => ({
  id: "w1",
  path: "/data",
  state: "done",
  dirs_scanned: 5,
  files: 4,
  bytes: 1115,
  skipped: 1,
  elapsed_ms: 10,
  tree,
  ...over,
});

beforeEach(() => {
  clearWalkCache();
  calls.length = 0;
  replies = [];
  busyStarts = 0;
});

describe("disk usage tree", () => {
  it("finds a folder anywhere below the walked root, but not a skipped one", () => {
    expect(findSubtree(tree, "/data", "/data")).toBe(tree);
    expect(findSubtree(tree, "/data/", "/data/games/a/sce_sys")?.bytes).toBe(10);
    expect(findSubtree(tree, "/data", "/data/locked")).toBeNull();
    expect(findSubtree(tree, "/data", "/data/nope")).toBeNull();
    // A sibling whose name starts the same is not inside.
    expect(findSubtree(tree, "/data", "/database")).toBeNull();
    expect(findSubtree(dir("", 1, { children: [dir("user", 1)] }), "/", "/user")?.name).toBe("user");
  });

  it("lists a folder's folders and files largest first, with the other files summed last", () => {
    const d = dir("x", 0, {
      children: [dir("small", 3), dir("big", 300)],
      top_files: [{ name: "f", size: 50 }],
      more_files: 7,
      more_bytes: 9,
    });
    expect(blocksOf(d).map((b) => [b.name, b.totalSize, b.isDir])).toEqual([
      ["big", 300, true],
      ["f", 50, false],
      ["small", 3, true],
      ["", 9, false],
    ]);
    expect(blocksOf(d)[3].count).toBe(7);
  });

  it("drills into a folder from the parent's walk, per console", () => {
    rememberWalk("10.0.0.5", status());
    expect(cachedFolder("10.0.0.5", "/data/games")?.dir.bytes).toBe(1110);
    expect(cachedFolder("10.0.0.6", "/data/games")).toBeNull();
    // Above the walk, or a folder it could not read: not known.
    expect(cachedFolder("10.0.0.5", "/")).toBeNull();
    expect(cachedFolder("10.0.0.5", "/data/locked")).toBeNull();
  });

  it("never keeps a cancelled or failed walk", () => {
    rememberWalk("h", status({ state: "cancelled", partial: "cancelled" }));
    rememberWalk("h", status({ state: "failed", tree: undefined }));
    expect(cachedFolder("h", "/data")).toBeNull();
  });

  it("refresh forgets the walks above and below the folder, and keeps the others", () => {
    rememberWalk("h", status());
    rememberWalk("h", status({ path: "/data/games", tree: tree.children[0] }));
    rememberWalk("h", status({ path: "/user", tree: dir("user", 1) }));
    forgetWalks("h", "/data/games");
    expect(cachedFolder("h", "/data")).toBeNull();
    expect(cachedFolder("h", "/data/games")).toBeNull();
    expect(cachedFolder("h", "/user")?.dir.bytes).toBe(1);
  });
});

describe("running a walk", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("polls until the walk ends, reporting progress", async () => {
    replies = [
      status({ state: "running", tree: undefined, dirs_scanned: 1 }),
      status({ state: "running", tree: undefined, dirs_scanned: 3 }),
      status(),
    ];
    const seen: number[] = [];
    const p = runWalk("10.0.0.5:9", "/data", (s) => seen.push(s.dirs_scanned), new AbortController().signal);
    await vi.advanceTimersByTimeAsync(POLL_MS * 3);
    const done = await p;
    expect(done?.state).toBe("done");
    expect(seen).toEqual([1, 3]);
    expect(calls.map((c) => c.cmd)).toEqual([
      "ps5_disk_usage_start",
      "ps5_disk_usage_status",
      "ps5_disk_usage_status",
    ]);
    expect(calls[0].args).toEqual({ addr: "10.0.0.5:9", path: "/data" });
  });

  it("waits out the walk before it when another folder was picked", async () => {
    busyStarts = 2;
    replies = [status()];
    const p = runWalk("a", "/data", () => {}, new AbortController().signal);
    await vi.advanceTimersByTimeAsync(POLL_MS * 3);
    expect((await p)?.state).toBe("done");
    expect(calls.filter((c) => c.cmd === "ps5_disk_usage_start")).toHaveLength(3);
  });

  it("gives up when another folder keeps the console busy", async () => {
    busyStarts = 100;
    const p = runWalk("a", "/data", () => {}, new AbortController().signal);
    const failed = expect(p).rejects.toThrow("disk_usage_busy");
    await vi.advanceTimersByTimeAsync(POLL_MS * 20);
    await failed;
    expect(isWalkBusy("engine HTTP 409: disk_usage_busy")).toBe(true);
  });

  it("cancels the engine's walk when aborted", async () => {
    replies = [status({ state: "running", tree: undefined })];
    const ac = new AbortController();
    const p = runWalk("a", "/data", () => {}, ac.signal);
    await vi.advanceTimersByTimeAsync(POLL_MS / 2);
    ac.abort();
    expect(await p).toBeNull();
    expect(calls[calls.length - 1]).toEqual({ cmd: "ps5_disk_usage_cancel", args: { id: "w1" } });
  });
});

describe("sleep", () => {
  it("takes its abort listener off again when the wait ends", async () => {
    const controller = new AbortController();
    const add = vi.spyOn(controller.signal, "addEventListener");
    const remove = vi.spyOn(controller.signal, "removeEventListener");
    await sleep(0, controller.signal);
    expect(add).toHaveBeenCalledTimes(1);
    expect(remove).toHaveBeenCalledTimes(1);
    expect(remove.mock.calls[0][1]).toBe(add.mock.calls[0][1]);
  });
  it("ends at once on abort", async () => {
    const controller = new AbortController();
    const waited = sleep(60_000, controller.signal);
    controller.abort();
    await expect(waited).resolves.toBeUndefined();
  });
});
