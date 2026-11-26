import { describe, expect, it, vi } from "vitest";

// A queue saved while it was making a game, as a page closed mid-build leaves it (#954).
vi.hoisted(() => {
  const mem = new Map<string, string>([
    [
      "ps5upload.convert_queue.v1",
      JSON.stringify([
        { id: "a", source: "/games/A", compression: "balanced", then: "keep", host: null, status: "running" },
        { id: "b", source: "/games/B", compression: "balanced", then: "keep", host: null, status: "pending" },
      ]),
    ],
  ]);
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (k: string) => (mem.has(k) ? (mem.get(k) as string) : null),
      setItem: (k: string, v: string) => void mem.set(k, String(v)),
      removeItem: (k: string) => void mem.delete(k),
      clear: () => mem.clear(),
    },
    addEventListener: () => {},
    removeEventListener: () => {},
  };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { setConvertRunner, useConvertQueue } from "./convertQueue";

describe("a Convert queue whose page was closed mid-build (#954)", () => {
  it("waits for Start, says so, and runs from the game it was on when started", async () => {
    const q = useConvertQueue.getState();
    expect(q.running).toBe(false);
    expect(q.stoppedByClose).toBe(true);
    expect(q.items.map((i) => i.status)).toEqual(["pending", "pending"]);
    const built: string[] = [];
    setConvertRunner({
      build: async (item) => {
        built.push(item.source);
        return { ok: true, packagePath: `/out/${item.id}.pkg` };
      },
      handOff: async () => ({ ok: true }),
    });
    await q.start();
    expect(useConvertQueue.getState().stoppedByClose).toBe(false);
    expect(built).toEqual(["/games/A", "/games/B"]);
    expect(useConvertQueue.getState().items.map((i) => i.status)).toEqual(["done", "done"]);
  });
});
