import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { holdConsoleFromEngineQueue } from "./engineQueue";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine" }));

function answer(status: number, body: unknown = {}): Response {
  return new Response(JSON.stringify(body), { status });
}

const running = () => answer(409, { code: "queue_running" });

function calls(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls.map((c) => String(c[0]).replace("http://engine", ""));
}

describe("holdConsoleFromEngineQueue", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  const turn = (live = () => true) => {
    const waits: boolean[] = [];
    return { waits, turn: { isLive: live, onWaiting: (w: boolean) => waits.push(w) } };
  };

  it("holds at once when the engine's queue is not running, and gives it back", async () => {
    fetchMock.mockImplementation(async () => answer(200, { held: true }));
    const { waits, turn: t } = turn();
    const release = await holdConsoleFromEngineQueue("192.168.1.5", t);
    expect(waits).toEqual([]);
    const body = JSON.parse(fetchMock.mock.calls[0][1].body);
    expect(body.console).toBe("192.168.1.5");
    expect(calls(fetchMock)).toEqual(["/api/queue/hold"]);

    // Kept going while the app works.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(calls(fetchMock)).toEqual(["/api/queue/hold", "/api/queue/hold"]);
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).holder).toBe(body.holder);

    release();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(calls(fetchMock)).toEqual(["/api/queue/hold", "/api/queue/hold", "/api/queue/release"]);
  });

  it("waits while an item of the engine's queue runs, and says so", async () => {
    fetchMock.mockResolvedValueOnce(running()).mockResolvedValueOnce(running());
    fetchMock.mockImplementation(async () => answer(200, { held: true }));
    const { waits, turn: t } = turn();
    const held = holdConsoleFromEngineQueue("h", t);
    await vi.advanceTimersByTimeAsync(0);
    expect(waits).toEqual([true]);
    await vi.advanceTimersByTimeAsync(4_000);
    const release = await held;
    expect(waits).toEqual([true, false]);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    release();
  });

  it("stops waiting when the run is stopped", async () => {
    fetchMock.mockImplementation(async () => running());
    let live = true;
    const { waits, turn: t } = turn(() => live);
    const held = holdConsoleFromEngineQueue("h", t);
    await vi.advanceTimersByTimeAsync(0);
    live = false;
    await vi.advanceTimersByTimeAsync(2_000);
    const release = await held;
    expect(waits).toEqual([true, false]);
    release();
    await vi.advanceTimersByTimeAsync(30_000);
    // It never held: nothing to keep going or give back.
    expect(calls(fetchMock).filter((c) => c === "/api/queue/release")).toEqual([]);
  });

  it("goes on when the engine cannot say", async () => {
    for (const reply of [
      () => Promise.reject(new TypeError("network")),
      async () => answer(404, { code: "not_found" }),
      async () => answer(409, { code: "something_else" }),
      async () => answer(500),
    ]) {
      fetchMock.mockReset();
      fetchMock.mockImplementation(reply);
      const { waits, turn: t } = turn();
      const release = await holdConsoleFromEngineQueue("h", t);
      expect(waits).toEqual([]);
      release();
      await vi.advanceTimersByTimeAsync(0);
    }
  });
});
