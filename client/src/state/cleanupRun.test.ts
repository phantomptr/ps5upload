import { beforeEach, describe, expect, it, vi } from "vitest";

const status = vi.fn();
const scan = vi.fn();
const cancel = vi.fn();
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
    cleanupCancel: (...a: unknown[]) => cancel(...a),
  };
});

import { CleanupError } from "../api/cleanup";
import {
  attachRunningApply,
  cleanupRunOf,
  dismissCleanupResult,
  followApply,
  HELPER_SILENT,
  MAX_SILENT_MS,
  STOP_ANSWERING_GRACE_MS,
  STOP_UNCONFIRMED,
  stopCleanupApply,
  useCleanupRuns,
} from "./cleanupRun";

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
    cancel.mockReset();
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

  // #714: the helper died while the PS5 removed an update. "Waiting for the PS5…" stayed up and
  // Stop did nothing, since Stop only asked the helper that was gone.
  it("gives up with its own reason once the helper stays silent, keeping what was known", async () => {
    let t = 0;
    status.mockResolvedValueOnce(run({ phase: "waiting", done: [1] })).mockRejectedValue(new Error("HTTP 502"));
    await followApply("10.0.0.2", 7, [{ id: 1 } as never], null, async () => void (t += 1000), () => t);
    expect(now().applying).toBeNull();
    expect((now().error as { reason?: string }).reason).toBe(HELPER_SILENT);
    expect(now().summary?.removed).toHaveLength(1);
    expect(t).toBeGreaterThanOrEqual(MAX_SILENT_MS);
    expect(t).toBeLessThan(MAX_SILENT_MS + 2000);
  });

  it("ends on Stop even when the helper never confirms it", async () => {
    let t = 0;
    status.mockResolvedValue(run({ phase: "waiting" }));
    cancel.mockRejectedValue(new Error("helper_not_ava1"));
    let stopping: boolean | undefined;
    await followApply("10.0.0.2", 7, [], null, async () => {
      t += 1000;
      if (t === 3000) {
        stopCleanupApply("10.0.0.2", () => t);
        stopping = now().stopping;
      }
      if (t > 120_000) throw new Error("still following");
    }, () => t);
    expect(stopping).toBe(true);
    expect(cancel).toHaveBeenCalledWith("10.0.0.2", 7);
    expect(now().applying).toBeNull();
    expect(now().stopping).toBe(false);
    expect((now().error as { reason?: string }).reason).toBe(STOP_UNCONFIRMED);
    // The helper still answers "running": followed for the longer grace, then ended.
    expect(t).toBeGreaterThanOrEqual(3000 + STOP_ANSWERING_GRACE_MS);
    expect(t).toBeLessThanOrEqual(3000 + STOP_ANSWERING_GRACE_MS + 1000);
  });

  it("takes the helper's own word when it stops in time", async () => {
    let t = 0;
    status
      .mockResolvedValueOnce(run())
      .mockResolvedValueOnce(run())
      .mockResolvedValue(run({ state: "cancelled", skipped: [{ id: 1, reason: "cancelled" }] }));
    cancel.mockResolvedValue({ ok: true });
    const following = followApply("10.0.0.2", 7, [{ id: 1 } as never], null, async () => {
      t += 1000;
      if (t === 1000) stopCleanupApply("10.0.0.2", () => t);
    }, () => t);
    await following;
    expect(now().error).toBeNull();
    expect(now().summary?.skipped[0].reason).toBe("cancelled");
  });
});
