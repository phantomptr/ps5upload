import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { busyTasks, startUpdateChecks } from "./update";

// What keeps an app that is left running up to date (`startUpdateChecks`). The automatic install
// waiting for a stream to end is in update.idle.test.ts.

describe("startUpdateChecks", () => {
  const events = new Map<string, () => void>();
  const target = (visible: { state: string }) => ({
    addEventListener: (k: string, f: () => void) => void events.set(k, f),
    removeEventListener: (k: string) => void events.delete(k),
    get visibilityState() {
      return visible.state;
    },
  });

  beforeEach(() => {
    vi.useFakeTimers();
    events.clear();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("checks at once, every hour, and when the window comes back, and stops when told", () => {
    const visible = { state: "visible" };
    vi.stubGlobal("document", target(visible));
    vi.stubGlobal("window", target(visible));
    const ensure = vi.fn();
    const stop = startUpdateChecks(ensure);
    expect(ensure).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(60 * 60 * 1000);
    expect(ensure).toHaveBeenCalledTimes(2);
    events.get("focus")?.();
    expect(ensure).toHaveBeenCalledTimes(3);
    visible.state = "hidden";
    events.get("visibilitychange")?.();
    expect(ensure).toHaveBeenCalledTimes(3);
    stop();
    expect(events.size).toBe(0);
    vi.advanceTimersByTime(3 * 60 * 60 * 1000);
    expect(ensure).toHaveBeenCalledTimes(3);
  });
});

describe("busyTasks", () => {
  it("still counts a running task", () => {
    expect(busyTasks([{ kind: "upload-file", status: "running" }])).toBe(true);
    expect(busyTasks([])).toBe(false);
  });
});
