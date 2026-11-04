import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import { IntoItselfError } from "./move";
import { freeName, keepBothName, pasteItems, type Conflict, type Decision } from "./paste";
import {
  askConflict,
  clearClipboard,
  currentConflict,
  dropFromClipboard,
  getClipboard,
  setClipboard,
} from "./clipboard";

const mockCall = vi.mocked(call);
const VOLS = [{ path: "/data" }, { path: "/mnt/usb0" }];

/** A fake console: `fs` maps existing paths to their kind; jobs run at once. */
function fakeConsole(fs: Record<string, "file" | "dir">) {
  let n = 0;
  mockCall.mockImplementation(async (route: string, body?: unknown) => {
    const b = body as Record<string, string>;
    switch (route) {
      case "/api/fs/stat":
        if (fs[b.path]) return { kind: fs[b.path] } as never;
        throw new ApiError(404, "not_found");
      case "/api/fs/delete":
        delete fs[b.path];
        return { job: `del${++n}` } as never;
      case "/api/fs/rename":
        if (b.from.startsWith("/mnt/") !== b.to.startsWith("/mnt/")) throw new ApiError(409, "cross_mount");
        if (fs[b.to]) throw new ApiError(409, "exists");
        fs[b.to] = fs[b.from];
        delete fs[b.from];
        return {} as never;
      case "/api/fs/copy":
        if (fs[b.dest] && !(body as { overwrite?: boolean }).overwrite) throw new ApiError(409, "exists");
        fs[b.dest] = fs[b.src];
        return { job: `job${++n}` } as never;
    }
    throw new Error(`unexpected ${route}`);
  });
  return fs;
}

function routes(): string[] {
  return mockCall.mock.calls.filter(([r]) => r !== "/api/fs/stat").map(([r, b]) => `${r} ${JSON.stringify(b)}`);
}

/** A decide() that answers from a list and records what it was asked. */
function answers(...ds: Decision[]) {
  const asked: Conflict[] = [];
  return {
    asked,
    decide: async (c: Conflict) => {
      asked.push(c);
      const d = ds.shift();
      if (!d) throw new Error("asked too often");
      return d;
    },
  };
}

beforeEach(() => {
  mockCall.mockReset();
  clearClipboard();
});

describe("keep both names", () => {
  it("adds (n) before the extension, not for folders or dot files", () => {
    expect(keepBothName("a.txt", 2, false)).toBe("a (2).txt");
    expect(keepBothName("archive.tar.gz", 2, false)).toBe("archive.tar (2).gz");
    expect(keepBothName("README", 3, false)).toBe("README (3)");
    expect(keepBothName(".bashrc", 2, false)).toBe(".bashrc (2)");
    expect(keepBothName("v1.2", 2, true)).toBe("v1.2 (2)");
    expect(keepBothName("a (2).txt", 3, false)).toBe("a (3).txt");
  });

  it("takes the first free number, past an existing (2)", async () => {
    const taken = new Set(["/d/a.txt", "/d/a (2).txt"]);
    const has = async (p: string) => taken.has(p);
    expect(await freeName("/d", "a.txt", false, has)).toBe("a (3).txt");
    expect(await freeName("/d", "b", false, has)).toBe("b (2)");
    // A name already ending in (2) counts on from there instead of growing "(2) (2)".
    taken.add("/d/a (3).txt");
    expect(await freeName("/d", "a (2).txt", false, has)).toBe("a (4).txt");
  });
});

describe("paste conflicts", () => {
  it("no conflict: a copy job per item, no question", async () => {
    fakeConsole({ "/data/a/x.txt": "file" });
    const a = answers();
    const r = await pasteItems([{ path: "/data/a/x.txt", dir: false }], "/data/b", "copy", { decide: a.decide });
    expect(a.asked).toEqual([]);
    expect(routes()).toEqual(['/api/fs/copy {"src":"/data/a/x.txt","dest":"/data/b/x.txt","move":false}']);
    expect(r.done).toEqual(["/data/a/x.txt"]);
  });

  it("skip leaves both alone", async () => {
    const fs = fakeConsole({ "/data/a/x.txt": "file", "/data/b/x.txt": "file" });
    const a = answers({ choice: "skip", all: false });
    const r = await pasteItems([{ path: "/data/a/x.txt", dir: false }], "/data/b", "copy", { decide: a.decide });
    expect(routes()).toEqual([]);
    expect(r.declined).toEqual(["/data/a/x.txt"]);
    expect(r.done).toEqual([]);
    expect(fs["/data/a/x.txt"]).toBe("file");
    expect(a.asked[0]).toMatchObject({ dest: "/data/b/x.txt", canReplace: true, destDir: false, left: 0 });
  });

  it("keep both copies to the first free name", async () => {
    fakeConsole({ "/data/a/x.txt": "file", "/data/b/x.txt": "file", "/data/b/x (2).txt": "file" });
    const a = answers({ choice: "keep", all: false });
    await pasteItems([{ path: "/data/a/x.txt", dir: false }], "/data/b", "copy", { decide: a.decide });
    expect(routes()).toEqual(['/api/fs/copy {"src":"/data/a/x.txt","dest":"/data/b/x (3).txt","move":false}']);
  });

  it("replace on a file: one copy job with overwrite, no delete", async () => {
    fakeConsole({ "/data/a/x.txt": "file", "/data/b/x.txt": "file" });
    const a = answers({ choice: "replace", all: false });
    await pasteItems([{ path: "/data/a/x.txt", dir: false }], "/data/b", "copy", { decide: a.decide });
    expect(routes()).toEqual([
      '/api/fs/copy {"src":"/data/a/x.txt","dest":"/data/b/x.txt","move":false,"overwrite":true}',
    ]);
  });

  it("replace on a folder copies beside it first, swaps, then deletes the old one: overwrite would merge", async () => {
    fakeConsole({ "/data/a/G": "dir", "/data/b/G": "dir" });
    const a = answers({ choice: "replace", all: false });
    const waited: string[] = [];
    const r = await pasteItems([{ path: "/data/a/G", dir: true }], "/data/b", "copy", { decide: a.decide }, {
      waitJob: async (id) => {
        waited.push(id);
        return "done";
      },
    });
    expect(routes()).toEqual([
      '/api/fs/copy {"src":"/data/a/G","dest":"/data/b/G.ps5u-new","move":false}',
      '/api/fs/rename {"from":"/data/b/G","to":"/data/b/G.ps5u-old"}',
      '/api/fs/rename {"from":"/data/b/G.ps5u-new","to":"/data/b/G"}',
      '/api/fs/delete {"path":"/data/b/G.ps5u-old"}',
    ]);
    expect(waited).toEqual(["job1", "del2"]);
    expect(r.done).toEqual(["/data/a/G"]);
    expect(r.notes).toEqual([]);
  });

  it("a failed replace copy fails the item; the old folder is never touched", async () => {
    const fs = fakeConsole({ "/data/a/G": "dir", "/data/b/G": "dir" });
    const a = answers({ choice: "replace", all: false });
    const r = await pasteItems([{ path: "/data/a/G", dir: true }], "/data/b", "copy", { decide: a.decide }, {
      waitJob: async () => "failed",
    });
    expect(routes().filter((x) => x.includes('"/data/b/G"'))).toEqual([]);
    expect(fs["/data/b/G"]).toBe("dir");
    expect(r.failed.map((f) => f.item)).toEqual(["/data/a/G"]);
    expect(r.done).toEqual([]);
  });

  it("apply to all answers the later conflicts without asking", async () => {
    fakeConsole({
      "/data/a/1": "file",
      "/data/a/2": "file",
      "/data/a/3": "file",
      "/data/b/1": "file",
      "/data/b/3": "file",
    });
    const a = answers({ choice: "keep", all: true });
    const items = ["1", "2", "3"].map((n) => ({ path: `/data/a/${n}`, dir: false }));
    const r = await pasteItems(items, "/data/b", "copy", { decide: a.decide });
    expect(a.asked).toHaveLength(1);
    expect(a.asked[0].left).toBe(2);
    expect(routes()).toEqual([
      '/api/fs/copy {"src":"/data/a/1","dest":"/data/b/1 (2)","move":false}',
      '/api/fs/copy {"src":"/data/a/2","dest":"/data/b/2","move":false}',
      '/api/fs/copy {"src":"/data/a/3","dest":"/data/b/3 (2)","move":false}',
    ]);
    expect(r.done).toHaveLength(3);
  });

  it("skip for all, after a replace for one", async () => {
    fakeConsole({ "/data/a/1": "file", "/data/a/2": "file", "/data/a/3": "file", "/data/b/1": "file", "/data/b/2": "file", "/data/b/3": "file" });
    const a = answers({ choice: "replace", all: false }, { choice: "skip", all: true });
    const items = ["1", "2", "3"].map((n) => ({ path: `/data/a/${n}`, dir: false }));
    const r = await pasteItems(items, "/data/b", "copy", { decide: a.decide });
    expect(a.asked).toHaveLength(2);
    expect(routes()).toEqual(['/api/fs/copy {"src":"/data/a/1","dest":"/data/b/1","move":false,"overwrite":true}']);
    expect(r.declined).toEqual(["/data/a/2", "/data/a/3"]);
  });

  it("copy into its own folder: Replace is not offered (it would delete the item)", async () => {
    fakeConsole({ "/data/a/x.txt": "file" });
    const a = answers({ choice: "replace", all: false });
    const r = await pasteItems([{ path: "/data/a/x.txt", dir: false }], "/data/a", "copy", { decide: a.decide });
    expect(a.asked[0].canReplace).toBe(false);
    expect(routes()).toEqual([]);
    expect(r.declined).toEqual(["/data/a/x.txt"]);
  });
});

describe("cut", () => {
  it("same drive: a rename", async () => {
    fakeConsole({ "/data/a/x": "dir" });
    const r = await pasteItems([{ path: "/data/a/x", dir: true }], "/data/b", "cut", { decide: answers().decide, volumes: VOLS });
    expect(routes()).toEqual(['/api/fs/rename {"from":"/data/a/x","to":"/data/b/x"}']);
    expect(r.moved).toEqual(["/data/a/x"]);
    expect(r.done).toEqual(["/data/a/x"]);
  });

  it("across drives: a copy job with the move flag", async () => {
    fakeConsole({ "/mnt/usb0/G": "dir" });
    await pasteItems([{ path: "/mnt/usb0/G", dir: true }], "/data/b", "cut", { decide: answers().decide, volumes: VOLS });
    expect(routes()).toEqual([
      '/api/fs/rename {"from":"/mnt/usb0/G","to":"/data/b/G"}',
      '/api/fs/copy {"src":"/mnt/usb0/G","dest":"/data/b/G","move":true}',
    ]);
  });

  it("replace on the same drive: rename beside it, swap, then delete the old one (rename never overwrites)", async () => {
    fakeConsole({ "/data/a/x.txt": "file", "/data/b/x.txt": "file" });
    const r = await pasteItems([{ path: "/data/a/x.txt", dir: false }], "/data/b", "cut", {
      decide: answers({ choice: "replace", all: false }).decide,
      volumes: VOLS,
    });
    expect(routes()).toEqual([
      '/api/fs/rename {"from":"/data/a/x.txt","to":"/data/b/x.txt.ps5u-new"}',
      '/api/fs/rename {"from":"/data/b/x.txt","to":"/data/b/x.txt.ps5u-old"}',
      '/api/fs/rename {"from":"/data/b/x.txt.ps5u-new","to":"/data/b/x.txt"}',
      '/api/fs/delete {"path":"/data/b/x.txt.ps5u-old"}',
    ]);
    expect(r.moved).toEqual(["/data/a/x.txt"]);
    expect(r.done).toEqual(["/data/a/x.txt"]);
  });

  it("replace of a file across drives: move job with overwrite", async () => {
    fakeConsole({ "/mnt/usb0/x.txt": "file", "/data/b/x.txt": "file" });
    await pasteItems([{ path: "/mnt/usb0/x.txt", dir: false }], "/data/b", "cut", {
      decide: answers({ choice: "replace", all: false }).decide,
      volumes: VOLS,
    });
    expect(routes()).toEqual([
      '/api/fs/copy {"src":"/mnt/usb0/x.txt","dest":"/data/b/x.txt","move":true,"overwrite":true}',
    ]);
  });

  it("a cut item pasted into its own folder stays put", async () => {
    fakeConsole({ "/data/a/x": "file" });
    const r = await pasteItems([{ path: "/data/a/x", dir: false }], "/data/a/", "cut", { decide: answers().decide });
    expect(mockCall).not.toHaveBeenCalled();
    expect(r.here).toEqual(["/data/a/x"]);
    expect(r.done).toEqual([]);
  });

  it("a folder is refused into itself, the rest still go", async () => {
    fakeConsole({ "/data/a": "dir", "/data/z.txt": "file" });
    const r = await pasteItems(
      [
        { path: "/data/a", dir: true },
        { path: "/data/z.txt", dir: false },
      ],
      "/data/a/sub",
      "cut",
      { decide: answers().decide, volumes: VOLS },
    );
    expect(r.failed[0].error).toBeInstanceOf(IntoItselfError);
    expect(routes()).toEqual(['/api/fs/rename {"from":"/data/z.txt","to":"/data/a/sub/z.txt"}']);
  });
});

describe("stop", () => {
  it("stop mid-batch: the rest are not sent", async () => {
    fakeConsole({ "/data/a/1": "file", "/data/a/2": "file", "/data/a/3": "file" });
    let stop = false;
    const r = await pasteItems(
      ["1", "2", "3"].map((n) => ({ path: `/data/a/${n}`, dir: false })),
      "/data/b",
      "copy",
      { decide: answers().decide },
      {
        stopped: () => stop,
        waitJob: async () => {
          stop = true;
          return "done";
        },
      },
    );
    expect(routes()).toHaveLength(1);
    expect(r.skipped).toEqual(["/data/a/2", "/data/a/3"]);
    expect(r.done).toEqual(["/data/a/1"]);
  });

  it("stop while a replace copies: that item is not pasted, the old one is as it was", async () => {
    const fs = fakeConsole({ "/data/a/G": "dir", "/data/b/G": "dir" });
    const r = await pasteItems([{ path: "/data/a/G", dir: true }], "/data/b", "copy", {
      decide: answers({ choice: "replace", all: false }).decide,
    }, { waitJob: async (_, unstoppable) => (unstoppable ? "done" : "cancelled") });
    expect(routes()).toEqual([
      '/api/fs/copy {"src":"/data/a/G","dest":"/data/b/G.ps5u-new","move":false}',
      '/api/fs/delete {"path":"/data/b/G.ps5u-new"}',
    ]);
    expect(r.skipped).toEqual(["/data/a/G"]);
    expect(r.done).toEqual([]);
    expect(fs["/data/b/G"]).toBe("dir");
    expect(fs["/data/b/G.ps5u-new"]).toBeUndefined();
  });
});

describe("clipboard store", () => {
  it("holds items across calls until cleared; cut items that moved drop out", () => {
    setClipboard("cut", [
      { path: "/data/a", dir: true },
      { path: "/data/b", dir: false },
    ]);
    expect(getClipboard()?.items).toHaveLength(2);
    dropFromClipboard(["/data/a"]);
    expect(getClipboard()).toEqual({ mode: "cut", items: [{ path: "/data/b", dir: false }] });
    dropFromClipboard(["/data/b"]);
    expect(getClipboard()).toBeNull();
  });

  it("a conflict waits for its answer", async () => {
    const c: Conflict = { item: "/a", dest: "/b/a", destDir: false, canReplace: true, left: 0 };
    const p = askConflict(c);
    expect(currentConflict()?.conflict).toBe(c);
    currentConflict()?.answer({ choice: "skip", all: false });
    expect(await p).toEqual({ choice: "skip", all: false });
    expect(currentConflict()).toBeNull();
  });
});

describe("paste as the page's batch", () => {
  it("Stop mid-batch through batch.ts: the rest are not sent, a cut keeps them in the clipboard", async () => {
    const { startBatch, stopBatch, currentBatch } = await import("./batch");
    fakeConsole({ "/data/a/1": "file", "/data/a/2": "file", "/data/a/3": "file" });
    const items = ["1", "2", "3"].map((n) => ({ path: `/data/a/${n}`, dir: false }));
    setClipboard("cut", items);
    const run = startBatch("move", 3, (ctl) =>
      pasteItems(items, "/data/b", "cut", { decide: answers().decide, volumes: VOLS }, {
        onProgress: (at, total) => {
          ctl.onProgress(at, total);
          if (at === 1) stopBatch();
        },
        stopped: ctl.stopped,
      }),
    )!;
    expect(currentBatch()?.op).toBe("move");
    const r = await run;
    dropFromClipboard(r.done.concat(r.declined));
    expect(r.moved).toEqual(["/data/a/1"]);
    expect(r.skipped).toEqual(["/data/a/2", "/data/a/3"]);
    expect(getClipboard()?.items.map((i) => i.path)).toEqual(["/data/a/2", "/data/a/3"]);
    expect(currentBatch()).toBeNull();
  });
});

describe("the item itself, by identity (exFAT is case-insensitive)", () => {
  /** /mnt/usb0 answers any letter case with the same entry (dev + ino), like exFAT. */
  function exfat(fs: Record<string, "file" | "dir">, ids = true) {
    const key = (p: string) => (p.startsWith("/mnt/usb0") ? p.toLowerCase() : p);
    const norm: Record<string, "file" | "dir"> = {};
    for (const [p, k] of Object.entries(fs)) norm[key(p)] = k;
    let n = 0;
    mockCall.mockImplementation(async (route: string, body?: unknown) => {
      const b = body as Record<string, string>;
      if (route === "/api/fs/stat") {
        const k = norm[key(b.path)];
        if (!k) throw new ApiError(404, "not_found");
        return (ids ? { kind: k, dev: "7", ino: key(b.path) } : { kind: k }) as never;
      }
      if (route === "/api/fs/delete") return { job: `del${++n}` } as never;
      if (route === "/api/fs/copy") return { job: `job${++n}` } as never;
      return {} as never;
    });
  }

  it("a cut into the same folder spelled differently is 'already here', nothing is sent", async () => {
    exfat({ "/mnt/usb0/Games": "dir", "/mnt/usb0/Games/x": "file" });
    const a = answers();
    const r = await pasteItems([{ path: "/mnt/usb0/Games/x", dir: false }], "/mnt/usb0/games", "cut", {
      decide: a.decide,
      volumes: VOLS,
    });
    expect(a.asked).toEqual([]);
    expect(routes()).toEqual([]);
    expect(r.here).toEqual(["/mnt/usb0/Games/x"]);
  });

  it("the destination found is the item itself: Replace is never offered, Apply-to-all Replace skips it", async () => {
    exfat({ "/mnt/usb0/Games": "dir", "/mnt/usb0/Games/x": "file", "/mnt/usb0/Games/y": "file" });
    const a = answers({ choice: "replace", all: true });
    const r = await pasteItems(
      [
        { path: "/mnt/usb0/Games/x", dir: false },
        { path: "/mnt/usb0/Games/y", dir: false },
      ],
      "/mnt/usb0/games",
      "copy",
      { decide: a.decide, volumes: VOLS },
    );
    expect(a.asked[0].canReplace).toBe(false);
    expect(routes()).toEqual([]); // no delete, no copy
    expect(r.declined).toEqual(["/mnt/usb0/Games/x", "/mnt/usb0/Games/y"]);
  });

  it("a cut whose folder ids differ but the destination is the item itself: no-op, never deleted", async () => {
    exfat({ "/mnt/usb0/Games/x": "file" });
    // The folders' stats fail (no ids to compare): the item's own stat still catches it.
    const base = mockCall.getMockImplementation()!;
    mockCall.mockImplementation(async (route: string, body?: unknown) => {
      const p = (body as { path?: string })?.path ?? "";
      if (route === "/api/fs/stat" && !p.endsWith("/x")) throw new ApiError(500, "io");
      return base(route, body as never);
    });
    const r = await pasteItems([{ path: "/mnt/usb0/Games/x", dir: false }], "/mnt/usb0/GAMES", "cut", {
      decide: answers({ choice: "replace", all: false }).decide,
      volumes: VOLS,
    });
    expect(routes()).toEqual([]);
    expect(r.here).toEqual(["/mnt/usb0/Games/x"]);
  });

  it("an older helper without ids falls back to the path compare", async () => {
    exfat({ "/data/a/x": "file", "/data/b/x": "file" }, false);
    const a = answers({ choice: "replace", all: false });
    await pasteItems([{ path: "/data/a/x", dir: false }], "/data/b", "copy", { decide: a.decide });
    expect(a.asked[0].canReplace).toBe(true);
  });
});

describe("Stop leaves a job running", () => {
  it("a cut whose job Stop left running is not counted done (it stays in the clipboard)", async () => {
    fakeConsole({ "/mnt/usb0/G": "dir" });
    const r = await pasteItems([{ path: "/mnt/usb0/G", dir: true }], "/data/b", "cut", { decide: answers().decide, volumes: VOLS }, {
      waitJob: async () => "left",
    });
    expect(r.left).toEqual(["/mnt/usb0/G"]);
    expect(r.done).toEqual([]);
  });

  it("Stop during a replace's copy is reported as halted", async () => {
    fakeConsole({ "/data/a/G": "dir", "/data/b/G": "dir" });
    const r = await pasteItems([{ path: "/data/a/G", dir: true }], "/data/b", "copy", {
      decide: answers({ choice: "replace", all: false }).decide,
    }, { waitJob: async () => "cancelled" });
    expect(r.halted).toEqual(["/data/a/G"]);
    expect(r.left).toEqual([]);
  });

  it("a plain copy Stop cancelled is not done (a cut stays in the clipboard) and says so", async () => {
    fakeConsole({ "/mnt/usb0/G": "dir", "/mnt/usb0/H": "dir" });
    let stop = false;
    const r = await pasteItems(
      [
        { path: "/mnt/usb0/G", dir: true },
        { path: "/mnt/usb0/H", dir: true },
      ],
      "/data/b",
      "cut",
      { decide: answers().decide, volumes: VOLS },
      {
        stopped: () => stop,
        waitJob: async () => {
          stop = true;
          return "cancelled";
        },
      },
    );
    expect(r.cancelled).toEqual(["/mnt/usb0/G"]);
    expect(r.skipped).toEqual(["/mnt/usb0/H"]);
    expect(r.done).toEqual([]);
  });

  it("Stop / Files closing answers a waiting conflict with Skip for the rest", async () => {
    const { skipPendingConflict } = await import("./clipboard");
    fakeConsole({ "/data/a/1": "file", "/data/a/2": "file", "/data/b/1": "file", "/data/b/2": "file" });
    const run = pasteItems(
      ["1", "2"].map((n) => ({ path: `/data/a/${n}`, dir: false })),
      "/data/b",
      "copy",
      { decide: askConflict },
    );
    for (let i = 0; i < 20 && !currentConflict(); i++) await Promise.resolve();
    expect(currentConflict()).not.toBeNull();
    skipPendingConflict();
    const r = await run;
    expect(r.declined).toEqual(["/data/a/1", "/data/a/2"]);
    expect(currentConflict()).toBeNull();
    expect(routes()).toEqual([]);
  });
});
