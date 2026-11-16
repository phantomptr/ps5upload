import { beforeEach, describe, expect, it, vi } from "vitest";

const taken = new Set<string>();
const added: { resolvedDest: string; strategy: string }[] = [];
let statFails: Error | null = null;
// What the queue reports for the item, in order.
let queueSteps: { status: string; waitingForWake?: boolean }[] = [{ status: "done" }];

vi.mock("../api/ps5", async (orig) => ({
  ...(await orig<typeof import("../api/ps5")>()),
  smpStatus: vi.fn(async () => ({ kraken: {} })),
  smpRescan: vi.fn(async () => ({})),
  listVolumes: vi.fn(async () => []),
  fsMkdir: vi.fn(async () => {}),
}));
vi.mock("../api/consoleFiles", () => ({
  fetchConsoleStat: vi.fn(async (_host: string, path: string) => {
    if (statFails) throw statFails;
    if (taken.has(path)) return { kind: "file", size: 1, mtime: 0, mode: 0, dev: 0 };
    throw new Error("fs_stat_errno_2");
  }),
}));
vi.mock("../lib/invokeLogged", () => ({
  invoke: vi.fn(async () => ({ parts: ["x"], head: { content_id: "UP0000-PPSA00000_00-X", title: "Game" } })),
}));
vi.mock("../lib/mounter", async (orig) => ({
  ...(await orig<typeof import("../lib/mounter")>()),
  krakenIsTheMounter: () => true,
}));
vi.mock("./consoleQueueBridge", () => ({
  pkgQueue: () => ({
    add: (item: { resolvedDest: string; strategy: string }) => {
      added.push(item);
      return "q1";
    },
    watch: (_id: string, cb: (q: { status: string }) => void) => {
      for (const q of queueSteps) cb(q);
      return () => {};
    },
  }),
}));

import { placePkgForMount } from "./pkgMount";

describe("placePkgForMount name clash", () => {
  beforeEach(() => {
    taken.clear();
    added.length = 0;
    statFails = null;
    queueSteps = [{ status: "done" }];
  });

  it("uploads straight in when the name is free", async () => {
    const r = await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data" });
    expect(r.ok).toBe(true);
    expect(added[0].resolvedDest).toBe("/data/homebrew/Game.pkg");
  });

  it("never overwrites unasked: without a question it picks a free name", async () => {
    taken.add("/data/homebrew/Game.pkg");
    taken.add("/data/homebrew/Game (2).pkg");
    const r = await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data" });
    expect(r.ok).toBe(true);
    expect(added[0].resolvedDest).toBe("/data/homebrew/Game (3).pkg");
  });

  it("asks, and Replace overwrites, Keep both renames, Cancel queues nothing", async () => {
    taken.add("/data/homebrew/Game.pkg");
    const ask = vi.fn(async () => "replace" as const);
    await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data", onExists: ask });
    expect(ask).toHaveBeenCalledWith("Game.pkg", false);
    expect(added[0].resolvedDest).toBe("/data/homebrew/Game.pkg");

    await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data", onExists: async () => "keep" });
    expect(added[1].resolvedDest).toBe("/data/homebrew/Game (2).pkg");

    const r = await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data", onExists: async () => null });
    expect(r.ok).toBe(false);
    expect(added).toHaveLength(2);
  });

  it("does not upload when the console can't say whether the name is taken", async () => {
    statFails = new Error("timed out");
    const r = await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data" });
    expect(r).toMatchObject({ ok: false, message: "timed out" });
    expect(added).toHaveLength(0);
  });

  it("ends when Pause or Stop puts the running upload back to waiting", async () => {
    queueSteps = [{ status: "pending" }, { status: "running" }, { status: "pending" }];
    const r = await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data" });
    expect(r.ok).toBe(false);
    expect(r.message).toMatch(/Paused or stopped/);
  });

  it("keeps waiting while the console rests, then finishes", async () => {
    queueSteps = [
      { status: "running" },
      { status: "pending", waitingForWake: true },
      { status: "running" },
      { status: "done" },
    ];
    const r = await placePkgForMount("/pc/Game.pkg", "10.0.0.2", { drive: "/data" });
    expect(r.ok).toBe(true);
  });
});
