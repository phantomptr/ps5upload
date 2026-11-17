import { beforeEach, describe, expect, it } from "vitest";

import { guardElfldr, ourElfldrPid, resetElfldrGuard, waitForLoader, type LoaderHealth } from "./elfldrGuard";

let now = 0;
const clock = { now: () => now, sleep: async (ms: number) => void (now += ms) };

beforeEach(() => {
  now = 1_000_000;
  resetElfldrGuard();
});

describe("waiting for the console's loader", () => {
  it("goes ahead at once when it answers", async () => {
    let asked = 0;
    const r = await waitForLoader("10.0.0.2", { health: async () => (asked++, "healthy"), ...clock });
    expect(r).toBe("healthy");
    expect(asked).toBe(1);
  });

  it("waits out a patched elfldr's 15 s deadline when it is stuck", async () => {
    const answers: LoaderHealth[] = ["stuck", "stuck", "healthy"];
    const r = await waitForLoader("10.0.0.2", { health: async () => answers.shift() ?? "healthy", ...clock });
    expect(r).toBe("healthy");
    expect(now - 1_000_000).toBeLessThanOrEqual(20_000);
  });

  it("gives up after 20 s on a stock elfldr that stays stuck", async () => {
    const r = await waitForLoader("10.0.0.2", { health: async () => "stuck", ...clock });
    expect(r).toBe("stuck");
    expect(now - 1_000_000).toBeGreaterThanOrEqual(20_000);
  });

  it("never blocks a send on a check that fails", async () => {
    const r = await waitForLoader("10.0.0.2", {
      health: async () => {
        throw new Error("engine too old");
      },
      ...clock,
    });
    expect(r).toBe("unknown");
  });
});

describe("keeping the patched elfldr in place", () => {
  it("asks the engine at most once per console every 10 minutes", async () => {
    const calls: string[] = [];
    const deps = { ensure: async (h: string) => void calls.push(h), ...clock };
    await guardElfldr("10.0.0.2", false, deps);
    await guardElfldr("10.0.0.2", false, deps);
    await guardElfldr("10.0.0.3", false, deps);
    expect(calls).toEqual(["10.0.0.2", "10.0.0.3"]);
    now += 10 * 60_000;
    await guardElfldr("10.0.0.2", false, deps);
    expect(calls).toEqual(["10.0.0.2", "10.0.0.3", "10.0.0.2"]);
  });

  it("asks straight away after a fresh helper send (a reboot or wake brings the stock one back)", async () => {
    const calls: string[] = [];
    const deps = { ensure: async (h: string) => void calls.push(h), ...clock };
    await guardElfldr("10.0.0.2", false, deps);
    await guardElfldr("10.0.0.2", true, deps);
    expect(calls).toEqual(["10.0.0.2", "10.0.0.2"]);
  });

  it("asks again every 30 s while the console settles after a wake (#842)", async () => {
    const calls: number[] = [];
    const answers: unknown[] = [
      { action: "skipped", reason: "console_settling" },
      { action: "skipped", reason: "console_settling" },
      { action: "current" },
    ];
    const deps = {
      ensure: async () => {
        calls.push(now);
        return answers.shift();
      },
      ...clock,
    };
    const start = now;
    await guardElfldr("10.0.0.2", false, deps);
    expect(calls).toEqual([start, start + 30_000, start + 60_000]);
    // settled and swapped: the 10-minute spacing applies again
    await guardElfldr("10.0.0.2", false, deps);
    expect(calls).toHaveLength(3);
  });

  it("gives up after a few tries and asks at the next chance", async () => {
    const calls: string[] = [];
    const deps = {
      ensure: async (h: string) => {
        calls.push(h);
        return { action: "skipped", reason: "console_settling" };
      },
      ...clock,
    };
    await guardElfldr("10.0.0.2", false, deps);
    expect(calls).toHaveLength(5);
    await guardElfldr("10.0.0.2", false, deps);
    expect(calls).toHaveLength(10);
  });

  it("swallows a failure: it is an improvement, never a blocker", async () => {
    await expect(
      guardElfldr("10.0.0.2", false, {
        ensure: async () => {
          throw new Error("no helper");
        },
        ...clock,
      }),
    ).resolves.toBeUndefined();
  });
});

describe("the elfldr swap and a running playlist", () => {
  it("waits for the auto-loader to finish before swapping", async () => {
    let polls = 0;
    const order: string[] = [];
    await guardElfldr("10.0.0.2", true, {
      ensure: async () => void order.push("swap"),
      playlistRuns: () => {
        const running = polls++ < 2;
        if (running) order.push("playlist");
        return running;
      },
      ...clock,
    });
    expect(order).toEqual(["playlist", "playlist", "swap"]);
  });

  it("leaves a playlist that outlasts the wait, and asks again next time", async () => {
    let swaps = 0;
    const deps = { ensure: async () => void swaps++, playlistRuns: () => true, ...clock };
    await guardElfldr("10.0.0.2", false, deps);
    expect(swaps).toBe(0);
    await guardElfldr("10.0.0.2", false, { ...deps, playlistRuns: () => false });
    expect(swaps).toBe(1);
  });

  it("waits again for a playlist that started while the console settled", async () => {
    const order: string[] = [];
    const answers: unknown[] = [{ action: "skipped", reason: "console_settling" }, { action: "current" }];
    let running = 0;
    await guardElfldr("10.0.0.2", true, {
      ensure: async () => {
        order.push("swap");
        // A playlist starts during the 30 s wait and runs for two polls.
        if (answers.length === 2) running = 2;
        return answers.shift();
      },
      playlistRuns: () => {
        if (running <= 0) return false;
        running--;
        order.push("playlist");
        return true;
      },
      ...clock,
    });
    expect(order).toEqual(["swap", "playlist", "playlist", "swap"]);
  });

  it("one swap per console at a time: a second call while one waits does nothing", async () => {
    let swaps = 0;
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const deps = {
      ensure: async () => {
        swaps++;
        await gate;
        return { action: "current" };
      },
      ...clock,
    };
    const first = guardElfldr("10.0.0.2", true, deps);
    await Promise.resolve();
    await guardElfldr("10.0.0.2", true, deps);
    expect(swaps).toBe(1);
    release();
    await first;
    // Done: a fresh send asks again.
    await guardElfldr("10.0.0.2", true, deps);
    expect(swaps).toBe(2);
  });
});

describe("which elfldr is PS5Upload's", () => {
  it("remembers the pid of the build it swapped in or found in place", async () => {
    const outcomes = [
      { action: "upgraded", pid: 106 },
      { action: "skipped", pid: 77 },
    ];
    const deps = { ensure: async () => outcomes.shift(), ...clock, playlistRuns: () => false };
    expect(ourElfldrPid("10.0.0.2")).toBeNull();
    await guardElfldr("10.0.0.2", true, deps);
    expect(ourElfldrPid("10.0.0.2:9113")).toBe(106);
    // A skipped swap (another loader, a shared name) says nothing about whose loader runs.
    await guardElfldr("10.0.0.2", true, deps);
    expect(ourElfldrPid("10.0.0.2")).toBe(106);
    await guardElfldr("10.0.0.3", true, { ...deps, ensure: async () => ({ action: "current", pid: 51 }) });
    expect(ourElfldrPid("10.0.0.3")).toBe(51);
  });
});
