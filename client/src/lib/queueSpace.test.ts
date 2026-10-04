import { beforeEach, describe, expect, it, vi } from "vitest";

import type { QueueItem } from "../state/uploadQueue";
import {
  checkQueueSpace,
  queueCommittedBytes,
  queueItemBytes,
  queueItemRemainingBytes,
  queueRemainingBytes,
} from "./queueSpace";

vi.mock("../api/ps5", async (importOriginal) => ({
  ...(await importOriginal<object>()),
  fetchVolumes: vi.fn(),
}));

import { fetchVolumes } from "../api/ps5";

const mockFetchVolumes = vi.mocked(fetchVolumes);

/** A QueueItem with only the fields the space helpers read. */
function item(over: Partial<QueueItem>): QueueItem {
  return {
    id: over.id ?? over.sourcePath ?? "it",
    sourceKind: "file",
    sourcePath: "/src/game",
    displayName: "game",
    resolvedDest: "/data/game",
    addr: "192.168.1.2:9113",
    strategy: "overwrite",
    reconcileMode: "fast",
    excludes: [],
    mountAfterUpload: false,
    mountReadOnly: true,
    registerAfterUpload: false,
    txIdHex: "00",
    status: "pending",
    bytesSent: 0,
    totalBytes: 0,
    bytesPerSec: 0,
    filesFinalized: 0,
    filesFinalizingTotal: 0,
    mountedAt: null,
    registeredAs: null,
    mountWarnings: [],
    error: null,
    errorReason: null,
    errorDetail: null,
    addedAt: 0,
    startedAt: null,
    completedAt: null,
    ...over,
  };
}

const data = {
  path: "/data",
  fs_type: "ext4",
  total_bytes: 1_000_000_000_000,
  free_bytes: 100_000_000_000,
  allocatable_bytes: 90_000_000_000,
  writable: true,
};

const ext0 = {
  path: "/mnt/ext0",
  fs_type: "exfat",
  total_bytes: 500_000_000_000,
  free_bytes: 400_000_000_000,
  allocatable_bytes: 390_000_000_000,
  writable: true,
};

describe("sizing a queue item", () => {
  it("prefers the size captured at add time over the pre-stat", () => {
    expect(queueItemBytes(item({ estimatedBytes: 5, totalBytes: 7 }))).toBe(5);
  });

  it("falls back to the pre-stat when the pick had no size", () => {
    expect(queueItemBytes(item({ totalBytes: 7 }))).toBe(7);
    expect(queueItemBytes(item({ estimatedBytes: 0, totalBytes: 7 }))).toBe(7);
  });

  it("counts unknown sources as zero so the check under-reports, never over", () => {
    expect(queueItemBytes(item({}))).toBe(0);
  });

  it("sizes an install from its live progress only", () => {
    expect(
      queueItemBytes(item({ sourceKind: "install", install: { via: "library", path: "/data/p.pkg" } })),
    ).toBe(0);
    expect(
      queueItemBytes(
        item({
          sourceKind: "install",
          installProgress: { phase: "install", current: 20, total: 80, bytesPerSec: 0 },
        }),
      ),
    ).toBe(80);
  });

  it("needs everything while waiting, only the remainder while running, nothing when finished", () => {
    const pending = item({ estimatedBytes: 100 });
    const running = item({ estimatedBytes: 100, status: "running", bytesSent: 30 });
    const done = item({ estimatedBytes: 100, status: "done", bytesSent: 100 });
    const failed = item({ estimatedBytes: 100, status: "failed" });
    expect(queueItemRemainingBytes(pending)).toBe(100);
    expect(queueItemRemainingBytes(running)).toBe(70);
    expect(queueItemRemainingBytes(done)).toBe(0);
    expect(queueItemRemainingBytes(failed)).toBe(0);
  });

  it("clamps the running remainder at zero when bytesSent overshoots", () => {
    expect(
      queueItemRemainingBytes(item({ estimatedBytes: 10, status: "running", bytesSent: 20 })),
    ).toBe(0);
  });

  it("sums the live rows and skips finished ones", () => {
    const items = [
      item({ estimatedBytes: 100 }),
      item({ estimatedBytes: 100, status: "running", bytesSent: 30 }),
      item({ estimatedBytes: 100, status: "done", bytesSent: 100 }),
      item({ estimatedBytes: 100, status: "failed" }),
    ];
    expect(queueRemainingBytes(items)).toBe(170);
    expect(queueCommittedBytes(items)).toBe(200);
  });

  it("counts an empty queue as zero bytes", () => {
    expect(queueRemainingBytes([])).toBe(0);
    expect(queueCommittedBytes([])).toBe(0);
  });
});

describe("checking the queue against the console's drives", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("passes quietly when everything fits", async () => {
    mockFetchVolumes.mockResolvedValue([data, ext0]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 80_000_000_000, resolvedDest: "/data/game" }),
      item({ estimatedBytes: 300_000_000_000, resolvedDest: "/mnt/ext0/game" }),
    ]);
    expect(findings).toEqual([]);
  });

  it("reports a volume whose queue doesn't fit, grouped per volume", async () => {
    mockFetchVolumes.mockResolvedValue([data, ext0]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 80_000_000_000, resolvedDest: "/data/game1" }),
      item({ estimatedBytes: 20_000_000_000, resolvedDest: "/data/game2" }),
      item({ estimatedBytes: 300_000_000_000, resolvedDest: "/mnt/ext0/game" }),
    ]);
    expect(findings).toEqual([
      {
        volumePath: "/data",
        requiredBytes: 100_000_000_000,
        allocatableBytes: 90_000_000_000,
        overBy: 10_000_000_000,
      },
    ]);
  });

  it("compares against allocatable (reserve-aware) space, not raw free", async () => {
    mockFetchVolumes.mockResolvedValue([{ ...data, free_bytes: 200_000_000_000 }]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 150_000_000_000, resolvedDest: "/data/game" }),
    ]);
    // Raw free (200 GB) covers it; the reserve-aware allocatable (90 GB) doesn't.
    expect(findings).toHaveLength(1);
    expect(findings[0].allocatableBytes).toBe(90_000_000_000);
  });

  it("counts only the running item's remainder, not its full size", async () => {
    mockFetchVolumes.mockResolvedValue([data]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 100, status: "running", bytesSent: 60, resolvedDest: "/data/game" }),
    ]);
    expect(findings).toEqual([]);
  });

  it("ignores items on drives the payload doesn't report", async () => {
    mockFetchVolumes.mockResolvedValue([data]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 999_000_000_000, resolvedDest: "/mnt/ext9/game" }),
    ]);
    expect(findings).toEqual([]);
  });

  it("ignores read-only and placeholder volumes", async () => {
    mockFetchVolumes.mockResolvedValue([
      { ...data, writable: false },
      { ...ext0, is_placeholder: true },
    ]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 500_000_000_000, resolvedDest: "/data/game" }),
    ]);
    expect(findings).toEqual([]);
  });

  it("stays quiet when the payload can't be reached", async () => {
    mockFetchVolumes.mockRejectedValue(new Error("connection refused"));
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 999_000_000_000, resolvedDest: "/data/game" }),
    ]);
    expect(findings).toEqual([]);
  });

  it("passes quietly when nothing is queued", async () => {
    mockFetchVolumes.mockResolvedValue([data]);
    const findings = await checkQueueSpace("192.168.1.2:9113", [
      item({ estimatedBytes: 100, status: "done", bytesSent: 100 }),
    ]);
    expect(findings).toEqual([]);
  });
});
