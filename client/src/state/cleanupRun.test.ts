import { beforeEach, describe, expect, it, vi } from "vitest";

const status = vi.fn();
const scan = vi.fn();
vi.mock("../api/cleanup", () => {
  class CleanupError extends Error {
    reason: string;
    constructor(message: string, reason: string) {
      super(message);
      this.reason = reason;
    }
  }
  return {
    CleanupError,
    cleanupApplyStatus: (...a: unknown[]) => status(...a),
    cleanupScanStatus: (...a: unknown[]) => scan(...a),
  };
});

import { CleanupError } from "../api/cleanup";
import { attachRunningApply, cleanupRunOf, dismissCleanupResult, followApply, useCleanupRuns } from "./cleanupRun";

const run = (over: object = {}) => ({
  ok: true,
  apply_id: 7,
  scan_id: 3,
  state: "running",
  current: 0,
  phase: "",
  total: 1,
  done: [],
  skipped: [],
  failed: [],
  freed_bytes: 0,
  ...over,
});
const now = () => cleanupRunOf(useCleanupRuns.getState(), "10.0.0.2");
const noWait = () => Promise.resolve();

describe("following a Clean-up Remove", () => {
  beforeEach(() => {
    useCleanupRuns.setState({ byHost: {} });
    status.mockReset();
    scan.mockReset();
  });

  it("keeps going past failed reads and ends with what became of each item", async () => {
    status
      .mockResolvedValueOnce(run())
      .mockRejectedValueOnce(new Error("timeout"))
      .mockResolvedValueOnce(run({ state: "done", failed: [{ id: 1, reason: "in_use" }] }));
    await followApply("10.0.0.2", 7, [{ id: 1 } as never], 500, noWait);
    expect(now().applying).toBeNull();
    expect(now().summary?.failed[0].reason).toBe("in_use");
    expect(now().freeBefore).toBe(500);
    expect(now().error).toBeNull();
  });

  it("stops when the helper has forgotten it, and says so", async () => {
    status
      .mockResolvedValueOnce(run())
      .mockRejectedValueOnce(new (CleanupError as unknown as new (m: string, r: string) => Error)("gone", "unknown_apply"));
    await followApply("10.0.0.2", 7, [], null, noWait);
    expect(now().applying).toBeNull();
    expect(now().error).toBeTruthy();
  });

  it("finds a running one it was not following, with its scan's items", async () => {
    status.mockResolvedValueOnce(run()).mockResolvedValue(run({ state: "done", done: [1] }));
    scan.mockResolvedValue({ items: [{ id: 1 }], free_bytes: 9 });
    expect(await attachRunningApply("10.0.0.2")).toBe(true);
    expect(status).toHaveBeenCalledWith("10.0.0.2", 0);
    expect(scan).toHaveBeenCalledWith("10.0.0.2", 3);
    await vi.waitFor(() => expect(now().summary?.removed).toHaveLength(1));
    expect(now().freeBefore).toBe(9);
  });

  it("reads an older helper's refusal as nothing running", async () => {
    status.mockRejectedValue(new Error("unknown_apply"));
    expect(await attachRunningApply("10.0.0.2")).toBe(false);
  });

  it("drops a result that was seen, never a run still going", async () => {
    status.mockResolvedValueOnce(run({ state: "done" }));
    await followApply("10.0.0.2", 7, [], null, noWait);
    expect(now().summary).not.toBeNull();
    dismissCleanupResult("10.0.0.2");
    expect(now().summary).toBeNull();
    useCleanupRuns.setState({ byHost: { "10.0.0.2": { ...now(), applying: run() as never } } });
    dismissCleanupResult("10.0.0.2");
    expect(now().applying).not.toBeNull();
  });
});
