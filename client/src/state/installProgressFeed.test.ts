import { describe, expect, it, vi } from "vitest";
import { installSampleFeed, type InstallSample } from "./pkgLibrary";
import { moveItemToWithinGroup } from "../lib/queueOps";
import { moveGroupOf, type QueueItem } from "./uploadQueue";

const sample = (over: Partial<InstallSample>): InstallSample => ({
  phase: "install",
  installedBytes: 0,
  transferBytes: 0,
  total: 1000,
  servedRequests: 0,
  stalled: false,
  acceptedUnverified: false,
  ...over,
});

/* The queue row shows an install's bytes, phase and rate itself, so the feed
 * has to hand over the numbers — not a pre-formatted sentence. */
describe("installSampleFeed", () => {
  it("reports the phase and bytes behind the percentage", () => {
    const onProgress = vi.fn();
    const feed = installSampleFeed({ onProgress, onStatus: vi.fn() });
    feed(sample({ phase: "download", transferBytes: 250, originRateBps: 9 }));
    expect(onProgress).toHaveBeenCalledWith(
      25,
      expect.objectContaining({
        phase: "transfer",
        current: 250,
        total: 1000,
        originBytesPerSec: 9,
      }),
    );
    feed(sample({ installedBytes: 500 }));
    expect(onProgress).toHaveBeenLastCalledWith(
      50,
      expect.objectContaining({ phase: "install", current: 500 }),
    );
  });

  it("never reports 100% before the install is confirmed", () => {
    const onProgress = vi.fn();
    installSampleFeed({ onProgress, onStatus: vi.fn() })(
      sample({ installedBytes: 1000 }),
    );
    expect(onProgress.mock.calls[0][0]).toBe(99);
  });

  it("sends a note, and the wait before the PS5 starts, as status", () => {
    const onProgress = vi.fn();
    const onStatus = vi.fn();
    const feed = installSampleFeed({ onProgress, onStatus });
    feed(sample({ phase: "queued" }));
    feed(sample({ installedBytes: 10, note: "The engine went away" }));
    expect(onStatus.mock.calls.map((c) => c[0])).toEqual([
      "Waiting for the PS5 to start…",
      "The engine went away",
    ]);
    expect(onProgress).not.toHaveBeenCalled();
  });
});

const q = (over: Partial<QueueItem>) =>
  ({ id: "x", addr: "10.0.0.2:9113", status: "pending", sourceKind: "install", resolvedDest: "", ...over }) as QueueItem;

/* Waiting rows are shown in run order, so a move must swap with the waiting
 * neighbour of the same tier — never a finished row or another console. */
describe("moveGroupOf", () => {
  it("groups waiting rows by console and install tier", () => {
    expect(moveGroupOf(q({ id: "a" }))).toBe(moveGroupOf(q({ id: "b" })));
    expect(moveGroupOf(q({ category: "gp" }))).not.toBe(moveGroupOf(q({ category: "gd" })));
    expect(moveGroupOf(q({ addr: "10.0.0.3:9113" }))).not.toBe(moveGroupOf(q({})));
  });

  it("keeps a finished row out of every group", () => {
    expect(moveGroupOf(q({ id: "a", status: "done" }))).not.toBe(moveGroupOf(q({ id: "b" })));
  });

  it("a drag never moves a row onto a running row, another tier or another console", () => {
    const items = [
      q({ id: "run", status: "running" }),
      q({ id: "a" }),
      q({ id: "b" }),
      q({ id: "upd", category: "gp" }),
      q({ id: "other", addr: "10.0.0.3:9113" }),
    ];
    expect(moveItemToWithinGroup(items, "b", "run", moveGroupOf)).toBe(items);
    expect(moveItemToWithinGroup(items, "run", "a", moveGroupOf)).toBe(items);
    expect(moveItemToWithinGroup(items, "a", "upd", moveGroupOf)).toBe(items);
    expect(moveItemToWithinGroup(items, "a", "other", moveGroupOf)).toBe(items);
    expect(moveItemToWithinGroup(items, "b", "a", moveGroupOf).map((i) => i.id)).toEqual([
      "run",
      "b",
      "a",
      "upd",
      "other",
    ]);
  });
});
