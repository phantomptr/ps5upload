import { describe, expect, it } from "vitest";

import type { ActivityEntry, ActivityKind } from "../../state/activityHistory";
import { computeStats, isNetworkTransfer } from "./stats";

const MiB = 1024 * 1024;
let n = 0;
function entry(kind: ActivityKind, bytes: number, seconds: number): ActivityEntry {
  const start = Date.UTC(2026, 9, 1, 12) + n++ * 1000;
  return {
    id: `e${n}`,
    kind,
    label: `${kind} ${n}`,
    startedAtMs: start,
    endedAtMs: start + seconds * 1000,
    outcome: "done",
    bytes,
  };
}

describe("computeStats", () => {
  it("counts the upload queue's bytes as uploaded", () => {
    const s = computeStats([
      entry("upload-queue", 300 * MiB, 10),
      entry("upload", 100 * MiB, 10),
      entry("download", 50 * MiB, 10),
      entry("library-install", 900 * MiB, 10),
    ]);
    expect(s.uploadedBytes).toBe(400 * MiB);
    expect(s.downloadedBytes).toBe(50 * MiB);
  });

  it("ranks only network transfers as fastest, not on-console copies or installs", () => {
    const s = computeStats([
      entry("fs-paste-copy", 4000 * MiB, 2),
      entry("library-move", 4000 * MiB, 2),
      entry("library-install", 4000 * MiB, 2),
      entry("upload-queue", 1000 * MiB, 10),
    ]);
    expect(s.fastestLabel).toMatch(/^upload-queue/);
    expect(s.fastestMbps).toBeCloseTo(100);
    expect(s.topTransfers).toHaveLength(1);
  });

  it("has no fastest when nothing crossed the network", () => {
    const s = computeStats([entry("fs-paste-move", 10 * MiB, 1)]);
    expect(s.fastestMbps).toBeNull();
    expect(s.fastestLabel).toBeNull();
  });
});

describe("isNetworkTransfer", () => {
  it("covers uploads and downloads only", () => {
    expect(isNetworkTransfer("upload-dir")).toBe(true);
    expect(isNetworkTransfer("library-download")).toBe(true);
    expect(isNetworkTransfer("fs-paste-copy")).toBe(false);
    expect(isNetworkTransfer("library-delete")).toBe(false);
  });
});
