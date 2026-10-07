import { describe, expect, it, vi } from "vitest";

import { closeRunningGameFirst, SETTLE_AFTER_CLOSE_MS } from "./launchSwap";

const game = (titleId: string) => ({ titleId, appId: 7, pid: 100 });

function deps(over: Partial<Parameters<typeof closeRunningGameFirst>[1]> = {}) {
  let running = new Map([["PPSA00001", game("PPSA00001")]]);
  const slept: number[] = [];
  const d = {
    running: vi.fn(async () => running),
    confirm: vi.fn(async () => true),
    close: vi.fn(async () => {
      running = new Map();
      return true;
    }),
    sleep: vi.fn(async (ms: number) => void slept.push(ms)),
    ...over,
  };
  return { d, slept, setRunning: (m: typeof running) => (running = m) };
}

describe("starting a game while another one is running", () => {
  it("starts straight away when nothing else is running", async () => {
    const { d, setRunning } = deps();
    setRunning(new Map());
    expect(await closeRunningGameFirst("PPSA00002", d)).toBe("clear");
    expect(d.confirm).not.toHaveBeenCalled();
    expect(d.close).not.toHaveBeenCalled();
  });

  it("does not count the game being started as another one", async () => {
    const { d } = deps();
    expect(await closeRunningGameFirst("PPSA00001", d)).toBe("clear");
    expect(d.close).not.toHaveBeenCalled();
  });

  it("asks, closes the running game, waits for it to go, then lets the console settle", async () => {
    const { d, slept } = deps();
    expect(await closeRunningGameFirst("PPSA00002", d)).toBe("closed");
    expect(d.confirm).toHaveBeenCalledWith(game("PPSA00001"));
    expect(d.close).toHaveBeenCalledWith(game("PPSA00001"));
    // The settle wait is the last thing before the launch.
    expect(slept[slept.length - 1]).toBe(SETTLE_AFTER_CLOSE_MS);
  });

  it("starts nothing when the user says no", async () => {
    const { d } = deps({ confirm: vi.fn(async () => false) });
    expect(await closeRunningGameFirst("PPSA00002", d)).toBe("cancelled");
    expect(d.close).not.toHaveBeenCalled();
  });

  it("starts nothing when the running game cannot be closed", async () => {
    const { d } = deps({ close: vi.fn(async () => false) });
    expect(await closeRunningGameFirst("PPSA00002", d)).toBe("close_failed");
  });

  it("starts nothing when the closed game is still there after the wait", async () => {
    // close() says ok but the process never leaves the list.
    const { d } = deps({ close: vi.fn(async () => true) });
    expect(await closeRunningGameFirst("PPSA00002", d)).toBe("close_failed");
  });

  it("goes ahead when the running list cannot be read: the launch itself will say", async () => {
    const { d } = deps({
      running: vi.fn(async () => {
        throw new Error("busy");
      }),
    });
    expect(await closeRunningGameFirst("PPSA00002", d)).toBe("clear");
  });
});
