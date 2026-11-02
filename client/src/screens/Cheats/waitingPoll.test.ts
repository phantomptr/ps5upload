import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { startWaitingPoll, WAITING_POLL_MS } from "./waitingPoll";

describe("startWaitingPoll (#627: Cheats screen and the game page's Cheats tab)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("reads the list again every interval and hands each read on, until stopped", async () => {
    let n = 0;
    const read = vi.fn(async () => ++n);
    const got: number[] = [];
    const stop = startWaitingPoll(read, (d) => got.push(d));
    await vi.advanceTimersByTimeAsync(WAITING_POLL_MS - 1);
    expect(read).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(got).toEqual([1]);
    await vi.advanceTimersByTimeAsync(2 * WAITING_POLL_MS);
    expect(got).toEqual([1, 2, 3]);
    stop();
    await vi.advanceTimersByTimeAsync(5 * WAITING_POLL_MS);
    expect(read).toHaveBeenCalledTimes(3);
  });

  it("skips a failed read quietly and keeps going", async () => {
    const read = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue("ok");
    const got: string[] = [];
    const stop = startWaitingPoll(read, (d) => got.push(d));
    await vi.advanceTimersByTimeAsync(2 * WAITING_POLL_MS);
    expect(got).toEqual(["ok"]);
    stop();
  });

  it("does not read, or overwrite, while a switch is being flipped", async () => {
    let busy = true;
    const read = vi.fn(async () => "list");
    const got: string[] = [];
    const stop = startWaitingPoll(
      read,
      (d) => got.push(d),
      () => busy,
    );
    await vi.advanceTimersByTimeAsync(3 * WAITING_POLL_MS);
    expect(read).not.toHaveBeenCalled();
    busy = false;
    await vi.advanceTimersByTimeAsync(WAITING_POLL_MS);
    expect(got).toEqual(["list"]);
    stop();
  });

  it("does not stack reads behind a slow console", async () => {
    let finish: (v: string) => void = () => {};
    const read = vi.fn(() => new Promise<string>((r) => (finish = r)));
    const got: string[] = [];
    const stop = startWaitingPoll(read, (d) => got.push(d));
    await vi.advanceTimersByTimeAsync(4 * WAITING_POLL_MS);
    expect(read).toHaveBeenCalledTimes(1);
    finish("late");
    await vi.advanceTimersByTimeAsync(WAITING_POLL_MS);
    expect(got).toEqual(["late"]);
    expect(read).toHaveBeenCalledTimes(2);
    stop();
  });

  it("drops a read that lands after it was stopped", async () => {
    let finish: (v: string) => void = () => {};
    const read = vi.fn(() => new Promise<string>((r) => (finish = r)));
    const got: string[] = [];
    const stop = startWaitingPoll(read, (d) => got.push(d));
    await vi.advanceTimersByTimeAsync(WAITING_POLL_MS);
    stop();
    finish("stale");
    await vi.advanceTimersByTimeAsync(WAITING_POLL_MS);
    expect(got).toEqual([]);
  });
});
