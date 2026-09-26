import { describe, expect, it, vi } from "vitest";
import { runBulkDelete, type BulkDeleteProgress } from "./bulkDelete";

const join = (dir: string, name: string) =>
  dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;

describe("runBulkDelete", () => {
  it("returns firstError=null and failureCount=0 when every delete succeeds", async () => {
    const deleter = vi.fn().mockResolvedValue(undefined);
    const onProgress = vi.fn();
    const result = await runBulkDelete({
      names: ["a.ffpkg", "b.ffpkg"],
      basePath: "/data/library",
      sizeByName: new Map([
        ["a.ffpkg", 1024],
        ["b.ffpkg", 2048],
      ]),
      joinPath: join,
      deleter,
      onProgress,
    });
    expect(result).toEqual({
      firstError: null,
      failureCount: 0,
      cancelled: false,
    });
    expect(deleter).toHaveBeenCalledTimes(2);
    expect(deleter).toHaveBeenNthCalledWith(1, "/data/library/a.ffpkg");
    expect(deleter).toHaveBeenNthCalledWith(2, "/data/library/b.ffpkg");
  });

  it("emits one progress event before each delete attempt with monotonic done", async () => {
    const deleter = vi.fn().mockResolvedValue(undefined);
    const onProgress = vi.fn();
    await runBulkDelete({
      names: ["one", "two", "three"],
      basePath: "/mnt/usb0",
      sizeByName: new Map([["one", 10]]),
      joinPath: join,
      deleter,
      onProgress,
    });
    expect(onProgress).toHaveBeenCalledTimes(3);
    const calls = onProgress.mock.calls.map(
      (c) => c[0] as BulkDeleteProgress,
    );
    expect(calls.map((p) => p.done)).toEqual([0, 1, 2]);
    expect(calls.map((p) => p.currentName)).toEqual(["one", "two", "three"]);
    expect(calls.map((p) => p.currentPath)).toEqual([
      "/mnt/usb0/one",
      "/mnt/usb0/two",
      "/mnt/usb0/three",
    ]);
    // Pre-fetched size present for "one", null for the others.
    expect(calls.map((p) => p.currentSize)).toEqual([10, null, null]);
  });

  it("continues after a per-item failure and reports the first error verbatim", async () => {
    const deleter = vi
      .fn()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("permission denied"))
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("io error"));
    const onProgress = vi.fn();
    const result = await runBulkDelete({
      names: ["ok1", "fail-perm", "ok2", "fail-io"],
      basePath: "/data",
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress,
    });
    expect(deleter).toHaveBeenCalledTimes(4);
    expect(result.failureCount).toBe(2);
    // First *error* should win even if a later error has a different
    // message — that's the contract: the user sees the earliest
    // failure as the headline.
    expect(result.firstError).toBe("fail-perm: permission denied");
  });

  it("stringifies non-Error throws so JSON-RPC payloads still surface", async () => {
    const deleter = vi
      .fn()
      .mockRejectedValue({ code: -32602, message: "ENOTDIR" });
    const onProgress = vi.fn();
    const result = await runBulkDelete({
      names: ["weird"],
      basePath: "/data",
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress,
    });
    // Object thrown -> String(obj) yields "[object Object]". Worth
    // pinning so a future "improve error formatting" change can't
    // silently drop information without updating this test too.
    expect(result.firstError).toBe("weird: [object Object]");
    expect(result.failureCount).toBe(1);
  });

  it("returns firstError=null and runs zero ops when names is empty", async () => {
    const deleter = vi.fn();
    const onProgress = vi.fn();
    const result = await runBulkDelete({
      names: [],
      basePath: "/data",
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress,
    });
    expect(result).toEqual({
      firstError: null,
      failureCount: 0,
      cancelled: false,
    });
    expect(deleter).not.toHaveBeenCalled();
    expect(onProgress).not.toHaveBeenCalled();
  });

  it("preserves names with spaces, parens, and unicode in joined paths", async () => {
    // PS5 game names commonly contain " (Update 1.05) [BLES01234]"
    // and Japanese/Korean titles. Regression-pin that we don't try to
    // sanitize them — the payload's protocol is byte-transparent.
    const deleter = vi.fn().mockResolvedValue(undefined);
    const onProgress = vi.fn();
    const tricky = [
      "Game (Update 1.05) [v2].ffpkg",
      "怪物獵人.ffpkg",
      "name with  double  space.ffpkg",
    ];
    await runBulkDelete({
      names: tricky,
      basePath: "/data/library",
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress,
    });
    expect(deleter.mock.calls.map((c) => c[0])).toEqual([
      "/data/library/Game (Update 1.05) [v2].ffpkg",
      "/data/library/怪物獵人.ffpkg",
      "/data/library/name with  double  space.ffpkg",
    ]);
  });

  it("uses the supplied joinPath so basePath ending in '/' doesn't double-slash", async () => {
    const deleter = vi.fn().mockResolvedValue(undefined);
    const onProgress = vi.fn();
    await runBulkDelete({
      names: ["a"],
      basePath: "/", // root case — historically a foot-gun
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress,
    });
    expect(deleter).toHaveBeenCalledWith("/a");
  });

  it("bails between items when shouldCancel returns true; cancelled flag set", async () => {
    const deleter = vi.fn().mockResolvedValue(undefined);
    let calls = 0;
    const result = await runBulkDelete({
      names: ["a", "b", "c", "d"],
      basePath: "/data",
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress: () => undefined,
      // Cancel right before the third item.
      shouldCancel: () => ++calls > 2,
    });
    expect(deleter).toHaveBeenCalledTimes(2);
    expect(result.cancelled).toBe(true);
    expect(result.failureCount).toBe(0);
    expect(result.firstError).toBeNull();
  });

  it("cancelled flag is false on natural completion", async () => {
    const result = await runBulkDelete({
      names: ["only"],
      basePath: "/data",
      sizeByName: new Map(),
      joinPath: join,
      deleter: vi.fn().mockResolvedValue(undefined),
      onProgress: () => undefined,
      shouldCancel: () => false,
    });
    expect(result.cancelled).toBe(false);
  });

  it("processes items strictly sequentially even when deleter is slow", async () => {
    // Without sequential ordering, the on-PS5 single-conn payload
    // could see overlapping fs_delete frames and mis-route responses.
    // The loop must `await` each call.
    const order: string[] = [];
    const deleter = vi.fn(async (p: string) => {
      order.push(`start:${p}`);
      await new Promise((r) => setTimeout(r, 5));
      order.push(`end:${p}`);
    });
    await runBulkDelete({
      names: ["x", "y"],
      basePath: "/data",
      sizeByName: new Map(),
      joinPath: join,
      deleter,
      onProgress: () => undefined,
    });
    expect(order).toEqual([
      "start:/data/x",
      "end:/data/x",
      "start:/data/y",
      "end:/data/y",
    ]);
  });
});
