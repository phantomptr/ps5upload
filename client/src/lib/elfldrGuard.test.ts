import { beforeEach, describe, expect, it } from "vitest";

import { guardElfldr, resetElfldrGuard, waitForLoader, type LoaderHealth } from "./elfldrGuard";

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
