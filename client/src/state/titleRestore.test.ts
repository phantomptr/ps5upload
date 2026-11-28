import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  restoreStart: vi.fn(),
  restoreStatus: vi.fn(),
}));
const notes = vi.hoisted(() => ({ push: vi.fn() }));

vi.mock("../api/titleRestore", () => api);
vi.mock("./notifications", () => ({ pushNotification: notes.push }));
vi.mock("./roster", () => ({ withConsolePrefix: (_h: string, s: string) => s }));

import { restoreKey, useTitleRestoreStore } from "./titleRestore";

const HOST = "192.168.1.50";
const items = [
  { titleId: "PPSA00001", name: "One" },
  { titleId: "PPSA00002", name: "Two" },
  { titleId: "PPSA00003", name: "Three" },
];
const phaseOf = (id: string) => useTitleRestoreStore.getState().entries[restoreKey(HOST, id)]?.run?.phase;

describe("following a Restore all (#961)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTitleRestoreStore.setState({ entries: {} });
    api.restoreStart.mockReset().mockResolvedValue({ ok: true, job: "j1", prepared: { ok: true, state: "prepared" } });
    api.restoreStatus.mockReset();
    notes.push.mockReset();
  });
  afterEach(() => vi.useRealTimers());

  it("asks only about the game being restored, tells each ending once, and goes on after a failure", async () => {
    // What the engine says, per title, poll after poll.
    const script: Record<string, unknown[]> = {
      PPSA00001: [{ phase: "installing" }, { phase: "rolled_back", cleared: true, install_hint: "stalled." }],
      PPSA00002: [{ phase: "installing" }, { phase: "restored" }],
      PPSA00003: [{ phase: "nonsense-from-a-newer-engine" }],
    };
    const asked: string[] = [];
    api.restoreStatus.mockImplementation(async (_h: string, id: string) => {
      asked.push(id);
      const q = script[id];
      return q.length > 1 ? q.shift() : q[0];
    });
    await useTitleRestoreStore.getState().start(HOST, items);
    expect(phaseOf("PPSA00001")).toBe("installing");
    expect(phaseOf("PPSA00002")).toBe("queued");

    await vi.advanceTimersByTimeAsync(3_000);
    // The first is still installing: the queued ones are not asked about.
    expect(asked).toEqual(["PPSA00001"]);
    await vi.advanceTimersByTimeAsync(3_000);
    // The first failed (its unfinished copy removed): told, and the next one is followed.
    expect(phaseOf("PPSA00001")).toBe("rolled_back");
    expect(notes.push.mock.calls[0][0]).toBe("error");
    expect(notes.push.mock.calls[0][2].body).toContain("unfinished copy was removed");
    expect(asked).toEqual(["PPSA00001", "PPSA00001", "PPSA00002"]);
    await vi.advanceTimersByTimeAsync(3_000);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(phaseOf("PPSA00002")).toBe("restored");
    // A phase this app does not know ends the run instead of leaving the page waiting forever.
    expect(phaseOf("PPSA00003")).toBe("failed");
    expect(notes.push).toHaveBeenCalledTimes(3);
    const before = api.restoreStatus.mock.calls.length;
    await vi.advanceTimersByTimeAsync(30_000);
    expect(api.restoreStatus.mock.calls.length).toBe(before);
  });

  it("an engine that stops answering leaves the games pending, not restoring for ever", async () => {
    api.restoreStatus.mockRejectedValue(new Error("engine unreachable"));
    await useTitleRestoreStore.getState().start(HOST, items.slice(0, 2));
    await vi.advanceTimersByTimeAsync(3_000 * 45);
    expect(phaseOf("PPSA00001")).toBe("pending");
    expect(phaseOf("PPSA00002")).toBe("pending");
  });

  it("an odd status body never throws out of the poll", async () => {
    api.restoreStatus.mockResolvedValueOnce("not json").mockResolvedValue({ phase: "restored" });
    await useTitleRestoreStore.getState().start(HOST, items.slice(0, 2));
    await vi.advanceTimersByTimeAsync(3_000);
    expect(phaseOf("PPSA00001")).toBe("failed");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(phaseOf("PPSA00002")).toBe("restored");
  });
});
