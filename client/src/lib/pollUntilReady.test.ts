import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { pollUntilReady } from "./pollUntilReady";

// We use vitest's fake timers because our poller mixes setTimeout with
// awaited Promises — rolling our own clock means also rolling our own
// microtask queue, which is fragile. `vi.advanceTimersByTimeAsync`
// flushes both timers and microtasks in lockstep.

describe("pollUntilReady", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("resolves ok on the first successful probe", async () => {
    const probe = vi.fn(async () => "ok" as const);
    const onResolved = vi.fn();
    const onAttempt = vi.fn();
    pollUntilReady({
      probe,
      initialDelayMs: 1000,
      intervalMs: 500,
      maxAttempts: 5,
      onAttempt,
      onResolved,
    });

    expect(probe).not.toHaveBeenCalled();
    expect(onResolved).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1000);
    expect(probe).toHaveBeenCalledTimes(1);
    expect(onAttempt).toHaveBeenCalledWith(1, "ok");
    expect(onResolved).toHaveBeenCalledWith("ok");
  });

  it("retries on fail then resolves ok when the probe comes up", async () => {
    let attempts = 0;
    const probe = vi.fn(async (): Promise<"ok" | "fail"> => {
      attempts += 1;
      return attempts >= 3 ? "ok" : "fail";
    });
    const onResolved = vi.fn();
    const onAttempt = vi.fn();
    pollUntilReady({
      probe,
      initialDelayMs: 1000,
      intervalMs: 500,
      maxAttempts: 10,
      onAttempt,
      onResolved,
    });

    await vi.advanceTimersByTimeAsync(1000); // initial → attempt 1 (fail)
    expect(probe).toHaveBeenCalledTimes(1);
    expect(onResolved).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(500);  // retry → attempt 2 (fail)
    expect(probe).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(500);  // retry → attempt 3 (ok)
    expect(probe).toHaveBeenCalledTimes(3);
    expect(onResolved).toHaveBeenCalledWith("ok");
    expect(onAttempt).toHaveBeenNthCalledWith(1, 1, "fail");
    expect(onAttempt).toHaveBeenNthCalledWith(2, 2, "fail");
    expect(onAttempt).toHaveBeenNthCalledWith(3, 3, "ok");
  });

  it("resolves fail once the attempt budget is exhausted", async () => {
    const probe = vi.fn(async () => "fail" as const);
    const onResolved = vi.fn();
    pollUntilReady({
      probe,
      initialDelayMs: 100,
      intervalMs: 200,
      maxAttempts: 3,
      onResolved,
    });

    await vi.advanceTimersByTimeAsync(100);  // attempt 1
    await vi.advanceTimersByTimeAsync(200);  // attempt 2
    await vi.advanceTimersByTimeAsync(200);  // attempt 3 (last)
    expect(probe).toHaveBeenCalledTimes(3);
    expect(onResolved).toHaveBeenCalledWith("fail");
    expect(onResolved).toHaveBeenCalledTimes(1);
  });

  it("cancel() prevents further probes and suppresses onResolved", async () => {
    const probe = vi.fn(async () => "fail" as const);
    const onResolved = vi.fn();
    const handle = pollUntilReady({
      probe,
      initialDelayMs: 100,
      intervalMs: 200,
      maxAttempts: 5,
      onResolved,
    });

    await vi.advanceTimersByTimeAsync(100); // attempt 1 (fail), retry scheduled
    expect(probe).toHaveBeenCalledTimes(1);
    handle.cancel();
    await vi.advanceTimersByTimeAsync(1000);
    expect(probe).toHaveBeenCalledTimes(1);
    expect(onResolved).not.toHaveBeenCalled();
  });

  it("cancel() before the first probe prevents any probe", async () => {
    const probe = vi.fn(async () => "ok" as const);
    const onResolved = vi.fn();
    const handle = pollUntilReady({
      probe,
      initialDelayMs: 100,
      intervalMs: 200,
      maxAttempts: 5,
      onResolved,
    });
    handle.cancel();
    await vi.advanceTimersByTimeAsync(1000);
    expect(probe).not.toHaveBeenCalled();
    expect(onResolved).not.toHaveBeenCalled();
  });

  it("treats probe exceptions as fail and keeps polling", async () => {
    let n = 0;
    const probe = vi.fn(async (): Promise<"ok" | "fail"> => {
      n += 1;
      if (n === 1) throw new Error("boom");
      if (n === 2) throw new Error("boom again");
      return "ok";
    });
    const onResolved = vi.fn();
    const onAttempt = vi.fn();
    pollUntilReady({
      probe,
      initialDelayMs: 10,
      intervalMs: 10,
      maxAttempts: 5,
      onAttempt,
      onResolved,
    });
    await vi.advanceTimersByTimeAsync(10); // throws → fail
    await vi.advanceTimersByTimeAsync(10); // throws → fail
    await vi.advanceTimersByTimeAsync(10); // ok
    expect(probe).toHaveBeenCalledTimes(3);
    expect(onAttempt).toHaveBeenNthCalledWith(1, 1, "fail");
    expect(onAttempt).toHaveBeenNthCalledWith(2, 2, "fail");
    expect(onAttempt).toHaveBeenNthCalledWith(3, 3, "ok");
    expect(onResolved).toHaveBeenCalledWith("ok");
  });

  it("does not double-schedule after first ok", async () => {
    // Regression guard: the previous inline implementation used
    // setStatusProbeState's updater function to schedule the next
    // attempt, which React may invoke twice in strict/concurrent mode
    // — producing parallel timeout chains. This test asserts that
    // each attempt produces exactly one probe call even when results
    // are raced.
    const probe = vi.fn(async () => "ok" as const);
    pollUntilReady({
      probe,
      initialDelayMs: 100,
      intervalMs: 100,
      maxAttempts: 10,
    });
    await vi.advanceTimersByTimeAsync(100);
    await vi.advanceTimersByTimeAsync(1000);
    expect(probe).toHaveBeenCalledTimes(1); // ok on first attempt; no more scheduled
  });

  it("throws if maxAttempts < 1", () => {
    expect(() =>
      pollUntilReady({
        probe: async () => "ok",
        initialDelayMs: 0,
        intervalMs: 0,
        maxAttempts: 0,
      })
    ).toThrow();
  });

  it("full boot scenario: 3s initial + 10 retries at 2s; boot ready on attempt 4 (~9s)", async () => {
    // Mirrors the real post-send use case. If this passes, the live
    // desktop code should behave correctly too. We check: pill is
    // amber for the whole probe window, green on first success, no
    // red flashes.
    let calls = 0;
    const probe = vi.fn(async (): Promise<"ok" | "fail"> => {
      calls += 1;
      return calls >= 4 ? "ok" : "fail";
    });
    const resolutions: Array<"ok" | "fail"> = [];
    pollUntilReady({
      probe,
      initialDelayMs: 3000,
      intervalMs: 2000,
      maxAttempts: 10,
      onResolved: (r) => resolutions.push(r),
    });

    await vi.advanceTimersByTimeAsync(3000); // attempt 1 (fail)
    expect(resolutions).toEqual([]);
    await vi.advanceTimersByTimeAsync(2000); // attempt 2 (fail)
    expect(resolutions).toEqual([]);
    await vi.advanceTimersByTimeAsync(2000); // attempt 3 (fail)
    expect(resolutions).toEqual([]);
    await vi.advanceTimersByTimeAsync(2000); // attempt 4 (ok)
    expect(resolutions).toEqual(["ok"]);
    expect(probe).toHaveBeenCalledTimes(4);

    // No further scheduling after success.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(probe).toHaveBeenCalledTimes(4);
  });
});
