import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./engine", () => ({ getEngineUrl: () => "http://engine" }));

import { asQueue, folderSizesApi, queueActive } from "../api/folderSizes";
import {
  POLL_MS,
  resetFolderSizes,
  useFolderSizesStore,
  watchFolderForTest,
} from "./folderSizes";

const ADDR = "10.0.0.5";

function answer(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

const queue = (over: Record<string, unknown> = {}) => ({
  rev: 1,
  running: null,
  queued: [],
  paused: null,
  keep_fresh: false,
  ...over,
});

const busy = (rev: number) =>
  queue({ rev, running: { addr: ADDR, path: "/data/games/A", started_at_ms: 1, files: 0, bytes: 0 } });

const sizes = (rev: number, q = queue({ rev })) => ({
  dir: "/data/games",
  self: null,
  children: rev > 1 ? { A: { bytes: 99, files: 3, measured_at_ms: 5, source: "measured", stored: true, partial: false } } : {},
  queue: q,
});

function paths(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls.map((c) => `${(c[1] as RequestInit | undefined)?.method ?? "GET"} ${String(c[0]).replace("http://engine", "")}`);
}

/** Lets the fetch promises and their follow-ups run. */
const settle = async () => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

describe("folderSizesApi", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    fetchMock = vi.fn(async () => answer(queue()));
    vi.stubGlobal("fetch", fetchMock);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("asks the engine's routes with the console and the folder", async () => {
    fetchMock.mockImplementationOnce(async () => answer(sizes(1)));
    const r = await folderSizesApi.get(ADDR, "/data/my games");
    expect(r.children).toEqual({});
    await folderSizesApi.measure(ADDR, ["/data/a", "/data/b"]);
    await folderSizesApi.measureAllIn(ADDR, "/data");
    await folderSizesApi.cancel(ADDR);
    await folderSizesApi.cancel(ADDR, ["/data/a"]);
    await folderSizesApi.queue(ADDR);
    expect(paths(fetchMock)).toEqual([
      "GET /api/ps5/folder-sizes?addr=10.0.0.5&dir=%2Fdata%2Fmy%20games",
      "POST /api/ps5/folder-sizes/measure",
      "POST /api/ps5/folder-sizes/measure",
      "POST /api/ps5/folder-sizes/cancel",
      "POST /api/ps5/folder-sizes/cancel",
      "GET /api/ps5/folder-sizes/queue?addr=10.0.0.5",
    ]);
    const bodies = fetchMock.mock.calls.slice(1, 5).map((c) => JSON.parse(String((c[1] as RequestInit).body)));
    expect(bodies).toEqual([
      { addr: ADDR, paths: ["/data/a", "/data/b"] },
      { addr: ADDR, dir: "/data" },
      { addr: ADDR },
      { addr: ADDR, paths: ["/data/a"] },
    ]);
  });

  it("a refusal is an error with the engine's words", async () => {
    fetchMock.mockImplementationOnce(async () => answer({ error: "path must be absolute" }, 400));
    await expect(folderSizesApi.measure(ADDR, ["x"])).rejects.toThrow("path must be absolute");
  });

  it("fills in a queue an older engine left short", () => {
    expect(asQueue(null)).toEqual({ rev: 0, running: null, queued: [], paused: null, keep_fresh: false });
    expect(queueActive(asQueue({ queued: [{ addr: ADDR, path: "/a" }] }))).toBe(true);
    expect(queueActive(asQueue({}))).toBe(false);
  });
});

describe("the folder sizes store", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  let unwatch: () => void;
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    resetFolderSizes();
    unwatch = watchFolderForTest(ADDR, "/data/games");
  });
  afterEach(() => {
    unwatch();
    resetFolderSizes();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("an idle queue is fetched once with the folder and never polled", async () => {
    fetchMock.mockImplementation(async () => answer(sizes(1)));
    await useFolderSizesStore.getState().load(ADDR, "/data/games");
    await vi.advanceTimersByTimeAsync(POLL_MS * 10);
    expect(paths(fetchMock)).toEqual(["GET /api/ps5/folder-sizes?addr=10.0.0.5&dir=%2Fdata%2Fgames"]);
  });

  it("polls while measuring, refetches the folder when a size changed, then stops", async () => {
    const replies = [
      answer(busy(1)), // the measure
      answer(busy(1)), // first poll: still the same rev
      answer(queue({ rev: 2 })), // second poll: done, a size changed
      answer(sizes(2)), // the folder again
    ];
    fetchMock.mockImplementation(async () => replies.shift() ?? answer(queue({ rev: 2 })));
    await useFolderSizesStore.getState().measure(ADDR, ["/data/games/A"]);
    expect(useFolderSizesStore.getState().queues[ADDR]?.running?.path).toBe("/data/games/A");
    await vi.advanceTimersByTimeAsync(POLL_MS);
    await settle();
    await vi.advanceTimersByTimeAsync(POLL_MS);
    await settle();
    const dir = useFolderSizesStore.getState().dirs[`${ADDR}\u0000/data/games`];
    expect(dir?.children.A?.bytes).toBe(99);
    const seen = paths(fetchMock);
    await vi.advanceTimersByTimeAsync(POLL_MS * 10);
    expect(paths(fetchMock)).toEqual(seen);
    expect(seen).toEqual([
      "POST /api/ps5/folder-sizes/measure",
      "GET /api/ps5/folder-sizes/queue?addr=10.0.0.5",
      "GET /api/ps5/folder-sizes/queue?addr=10.0.0.5",
      "GET /api/ps5/folder-sizes?addr=10.0.0.5&dir=%2Fdata%2Fgames",
    ]);
  });

  it("stops polling once no screen shows a folder of that console", async () => {
    fetchMock.mockImplementation(async () => answer(busy(1)));
    await useFolderSizesStore.getState().measure(ADDR, ["/data/games/A"]);
    await vi.advanceTimersByTimeAsync(POLL_MS);
    await settle();
    const n = fetchMock.mock.calls.length;
    unwatch();
    unwatch = () => {};
    await vi.advanceTimersByTimeAsync(POLL_MS * 10);
    expect(fetchMock.mock.calls.length).toBe(n);
  });

  it("an engine without the route leaves sizes unknown and polls nothing", async () => {
    fetchMock.mockImplementation(async () => answer({ error: "not found" }, 404));
    await useFolderSizesStore.getState().load(ADDR, "/data/games");
    await vi.advanceTimersByTimeAsync(POLL_MS * 5);
    expect(useFolderSizesStore.getState().dirs).toEqual({});
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("Stop measuring empties the queue and the polling ends", async () => {
    const replies = [answer(busy(1)), answer(queue({ rev: 1 }))];
    fetchMock.mockImplementation(async () => replies.shift() ?? answer(queue({ rev: 1 })));
    await useFolderSizesStore.getState().measure(ADDR, ["/data/games/A"]);
    await useFolderSizesStore.getState().cancel(ADDR);
    expect(queueActive(useFolderSizesStore.getState().queues[ADDR])).toBe(false);
    await vi.advanceTimersByTimeAsync(POLL_MS * 5);
    expect(paths(fetchMock)).toEqual([
      "POST /api/ps5/folder-sizes/measure",
      "POST /api/ps5/folder-sizes/cancel",
    ]);
  });
});
