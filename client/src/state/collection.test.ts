import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  scan: vi.fn(),
  scanStatus: vi.fn(),
  library: vi.fn(),
}));
vi.mock("../api/collection", () => ({ collection: api }));

import { startCollectionScan, useCollectionStore } from "./collection";

const RUNNING = {
  running: true,
  deep: false,
  found: 12,
  done: 3,
  started_ms: 1,
  finished_ms: 0,
  error: null,
  games: 0,
  locations: 0,
};

describe("startCollectionScan", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    api.scan.mockReset();
    api.scanStatus.mockReset();
    useCollectionStore.setState({ error: null, scan: null });
  });

  it("keeps following the scan after a failed read, so it does not stay on running", async () => {
    api.scan.mockResolvedValue(RUNNING);
    api.library.mockResolvedValue({ games: [] });
    await startCollectionScan(false);
    api.scanStatus.mockRejectedValueOnce(new Error("engine restarting"));
    await vi.advanceTimersByTimeAsync(600);
    expect(useCollectionStore.getState().error).toContain("restarting");
    expect(useCollectionStore.getState().scan?.running).toBe(true);
    api.scanStatus.mockResolvedValueOnce({ ...RUNNING, running: false });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(useCollectionStore.getState().scan?.running).toBe(false);
    expect(useCollectionStore.getState().error).toBeNull();
    vi.useRealTimers();
  });

  it("joins a scan already running instead of reporting an error", async () => {
    api.scan.mockRejectedValue(new Error("a scan is already running"));
    api.scanStatus.mockResolvedValue(RUNNING);
    await startCollectionScan(false);
    const s = useCollectionStore.getState();
    expect(s.error).toBeNull();
    expect(s.scan?.running).toBe(true);
    vi.useRealTimers();
  });

  it("still reports any other failure", async () => {
    api.scan.mockRejectedValue(new Error("add a folder to the Collection first"));
    await startCollectionScan(false);
    expect(useCollectionStore.getState().error).toContain("add a folder");
    vi.useRealTimers();
  });
});
