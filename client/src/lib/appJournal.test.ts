import { describe, expect, it, vi } from "vitest";
import { createJournal } from "./appJournal";

describe("app journal buffer", () => {
  it("writes collapsed records once the window closes, and on flush", async () => {
    const written: string[] = [];
    let now = 1_000;
    const j = createJournal({
      append: async (lines) => {
        written.push(...lines);
      },
      read: async () => [],
      now: () => now,
    });
    j.record({ cat: "app", level: "warn", code: "engine_down", msg: "engine unreachable" });
    now += 5_000;
    j.record({ cat: "app", level: "warn", code: "engine_down", msg: "engine unreachable" });
    now += 70_000;
    await j.tick();
    expect(written).toHaveLength(1);
    expect(JSON.parse(written[0])).toMatchObject({ count: 2, src: "app" });
    j.record({ cat: "system", level: "info", code: "app_start", msg: "started" });
    await j.flush();
    expect(written).toHaveLength(2);
  });

  it("never throws when the backend fails, and counts what it dropped", async () => {
    const j = createJournal({
      append: vi.fn().mockRejectedValue(new Error("disk")),
      read: async () => [],
      now: () => 1,
    });
    j.record({ cat: "app", level: "error", msg: "x" });
    await expect(j.flush()).resolves.toBeUndefined();
    expect(j.dropped()).toBe(1);
  });

  it("record returns the event's time, for linking a report to it", () => {
    const j = createJournal({ append: async () => {}, read: async () => [], now: () => 42 });
    expect(j.record({ cat: "app", level: "error", msg: "x" })).toBe(42);
  });

  it("reads back what was written, flushing first", async () => {
    const store: string[] = [];
    const j = createJournal({
      append: async (l) => void store.push(...l),
      read: async () => store,
      now: () => 5,
    });
    j.record({ cat: "app", level: "info", msg: "hello" });
    const got = await j.read(0);
    expect(got).toHaveLength(1);
    expect(got[0]).toMatchObject({ msg: "hello", src: "app", ts: 5 });
  });
});
