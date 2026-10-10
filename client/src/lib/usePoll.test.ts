import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createPoller, transferBusy } from "./usePoll";
import type { Task } from "../state/tasks";
import type { TransferPhase } from "../state/transfer";

describe("createPoller", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("runs immediately, then every interval", () => {
    const fn = vi.fn();
    const p = createPoller({ fn, ms: 1000 });
    p.start();
    expect(fn).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(3000);
    expect(fn).toHaveBeenCalledTimes(4);
    p.stop();
    vi.advanceTimersByTime(5000);
    expect(fn).toHaveBeenCalledTimes(4);
  });

  it("never overlaps: the next tick waits for the call in flight", async () => {
    let resolve!: () => void;
    const fn = vi.fn(() => new Promise<void>((r) => (resolve = r)));
    const p = createPoller({ fn, ms: 1000 });
    p.start();
    vi.advanceTimersByTime(10_000);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(p.inFlight()).toBe(true);
    p.kick();
    expect(fn).toHaveBeenCalledTimes(1);
    resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(p.inFlight()).toBe(false);
    vi.advanceTimersByTime(1000);
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it("skips ticks while paused and catches up on kick", () => {
    let paused = false;
    const fn = vi.fn();
    const p = createPoller({ fn, ms: 1000, paused: () => paused });
    p.start();
    paused = true;
    vi.advanceTimersByTime(5000);
    expect(fn).toHaveBeenCalledTimes(1);
    p.kick(); // still paused: no run
    expect(fn).toHaveBeenCalledTimes(1);
    paused = false;
    p.kick();
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it("a forced kick runs even while paused (explicit refresh)", () => {
    const fn = vi.fn();
    const p = createPoller({ fn, ms: 1000, paused: () => true, immediate: false });
    p.start();
    vi.advanceTimersByTime(3000);
    expect(fn).not.toHaveBeenCalled();
    p.kick(true);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("keeps polling after the call throws or rejects", async () => {
    const fn = vi
      .fn()
      .mockImplementationOnce(() => {
        throw new Error("x");
      })
      .mockImplementationOnce(() => Promise.reject(new Error("y")))
      .mockImplementation(() => undefined);
    const p = createPoller({ fn, ms: 1000 });
    p.start();
    vi.advanceTimersByTime(1000);
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(1000);
    expect(fn).toHaveBeenCalledTimes(3);
  });
});

describe("transferBusy", () => {
  const task = (consoleId: string, kind: Task["kind"], status: Task["status"]) =>
    ({ consoleId, kind, status }) as Task;
  const running = { kind: "running" } as TransferPhase;
  const idle = { kind: "idle" } as TransferPhase;

  it("is false when nothing moves", () => {
    expect(transferBusy("192.168.1.2", [], {}, {})).toBe(false);
    expect(transferBusy(null, [], { "192.168.1.2": idle }, {})).toBe(false);
  });

  it("scopes to the given console, ignoring its port", () => {
    const tasks = [task("192.168.1.2", "upload-file", "running")];
    expect(transferBusy("192.168.1.2:9021", tasks, {}, {})).toBe(true);
    expect(transferBusy("192.168.1.3", tasks, {}, {})).toBe(false);
    expect(transferBusy("192.168.1.3", [], { "192.168.1.3": running }, {})).toBe(true);
    expect(transferBusy("192.168.1.3", [], {}, { "192.168.1.3": true })).toBe(true);
  });

  it("without a host, any console's transfer counts", () => {
    expect(transferBusy(null, [], {}, { "10.0.0.1": true })).toBe(true);
    expect(transferBusy(undefined, [task("10.0.0.1", "pkg-install", "queued")], {}, {})).toBe(true);
  });

  it("quick tasks and finished transfers don't pause polling", () => {
    expect(transferBusy("10.0.0.1", [task("10.0.0.1", "upload-file", "done")], {}, {})).toBe(false);
  });
});
