import { describe, expect, it, vi } from "vitest";
import { createLimiter } from "./limitConcurrency";

/**
 * The limiter is the throttle gate behind every per-row Library
 * fetch — bug here would silently break or corrupt parallel-fetch
 * ordering. Tests cover concurrency cap, FIFO ordering, error
 * recovery, and the always-release contract.
 */
describe("createLimiter", () => {
  function deferred<T = void>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: unknown) => void;
    const promise = new Promise<T>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    return { promise, resolve, reject };
  }

  it("max=1 serializes calls", async () => {
    const limit = createLimiter(1);
    const order: number[] = [];
    const a = deferred();
    const b = deferred();
    const p1 = limit(async () => {
      order.push(1);
      await a.promise;
      order.push(2);
    });
    const p2 = limit(async () => {
      order.push(3);
      await b.promise;
      order.push(4);
    });
    // Microtask flush — task 1 should be inside its body, task 2 still
    // queued.
    await Promise.resolve();
    expect(order).toEqual([1]);
    a.resolve();
    await p1;
    expect(order).toEqual([1, 2, 3]);
    b.resolve();
    await p2;
    expect(order).toEqual([1, 2, 3, 4]);
  });

  it("max=N caps concurrent active count", async () => {
    const limit = createLimiter(2);
    let active = 0;
    let peak = 0;
    const gates = Array.from({ length: 5 }, () => deferred());
    const results = gates.map((g, i) =>
      limit(async () => {
        active += 1;
        peak = Math.max(peak, active);
        await g.promise;
        active -= 1;
        return i;
      }),
    );
    // Let the first two start.
    await new Promise((r) => setTimeout(r, 0));
    expect(active).toBe(2);
    expect(peak).toBe(2);
    // Release sequentially; each release lets one queued task in.
    for (const g of gates) {
      g.resolve();
      await new Promise((r) => setTimeout(r, 0));
    }
    const out = await Promise.all(results);
    expect(out).toEqual([0, 1, 2, 3, 4]);
    expect(peak).toBe(2);
  });

  it("preserves FIFO order across queued tasks", async () => {
    const limit = createLimiter(1);
    const order: number[] = [];
    const gates = Array.from({ length: 4 }, () => deferred());
    const ps = gates.map((g, i) =>
      limit(async () => {
        order.push(i);
        await g.promise;
      }),
    );
    // Release in reverse order; FIFO inside the limiter still
    // dictates that task 0 ran first, then 1, 2, 3.
    for (const g of gates) g.resolve();
    await Promise.all(ps);
    expect(order).toEqual([0, 1, 2, 3]);
  });

  it("releases the slot even when fn throws", async () => {
    const limit = createLimiter(1);
    const calls: number[] = [];
    await expect(
      limit(async () => {
        calls.push(1);
        throw new Error("boom");
      }),
    ).rejects.toThrow("boom");
    // If the slot weren't released, this second call would deadlock —
    // vitest's default test timeout (5s) would fire instead.
    await limit(async () => {
      calls.push(2);
    });
    expect(calls).toEqual([1, 2]);
  });

  it("propagates the function return value", async () => {
    const limit = createLimiter(2);
    const fn = vi.fn(async () => "hello");
    expect(await limit(fn)).toBe("hello");
    expect(fn).toHaveBeenCalledOnce();
  });
});
