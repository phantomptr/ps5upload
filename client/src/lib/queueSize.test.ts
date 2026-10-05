import { describe, expect, it } from "vitest";

import type { QueueItem } from "../state/uploadQueue";
import {
  queueItemBytes,
  queueItemRemainingBytes,
  queueRemainingBytes,
} from "./queueSize";

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

describe("sizing a queue item", () => {
  it("prefers the size captured at add time over the pre-stat", () => {
    expect(queueItemBytes(item({ estimatedBytes: 5, totalBytes: 7 }))).toBe(5);
  });

  it("falls back to the pre-stat when the pick had no size", () => {
    expect(queueItemBytes(item({ totalBytes: 7 }))).toBe(7);
    expect(queueItemBytes(item({ estimatedBytes: 0, totalBytes: 7 }))).toBe(7);
  });

  it("counts unknown sources as zero so the chip under-reports, never over", () => {
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
  });

  it("counts an empty queue as zero bytes", () => {
    expect(queueRemainingBytes([])).toBe(0);
  });
});
