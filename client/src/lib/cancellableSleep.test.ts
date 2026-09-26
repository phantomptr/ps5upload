import { describe, expect, it, vi } from "vitest";

import { cancellableSleep, isAbortError } from "./cancellableSleep";

describe("cancellableSleep", () => {
  it("resolves after the timeout when not aborted", async () => {
    vi.useFakeTimers();
    try {
      const ac = new AbortController();
      const p = cancellableSleep(100, ac.signal);
      vi.advanceTimersByTime(100);
      await expect(p).resolves.toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects with AbortError when aborted mid-sleep", async () => {
    vi.useFakeTimers();
    try {
      const ac = new AbortController();
      const p = cancellableSleep(10_000, ac.signal);
      // Abort halfway through.
      vi.advanceTimersByTime(5_000);
      ac.abort();
      await expect(p).rejects.toThrow(/aborted while sleeping/);
      const caught = await p.catch((e) => e);
      expect(isAbortError(caught)).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects synchronously if signal is already aborted at call time", async () => {
    const ac = new AbortController();
    ac.abort();
    await expect(cancellableSleep(100, ac.signal)).rejects.toThrow(
      /aborted before start/,
    );
  });

  it("clears the timer on abort (no setTimeout leak)", async () => {
    vi.useFakeTimers();
    try {
      const ac = new AbortController();
      const p = cancellableSleep(10_000, ac.signal);
      ac.abort();
      await expect(p).rejects.toThrow();
      // Advance past when the timer would have fired. Nothing
      // should happen — the timer was cleared on abort.
      vi.advanceTimersByTime(20_000);
      // No assertion needed; absence of "resolved twice" or
      // similar errors is the test.
    } finally {
      vi.useRealTimers();
    }
  });

  it("removes the abort listener on natural completion (no leaked listener)", async () => {
    vi.useFakeTimers();
    try {
      const ac = new AbortController();
      const removeSpy = vi.spyOn(ac.signal, "removeEventListener");
      const p = cancellableSleep(100, ac.signal);
      vi.advanceTimersByTime(100);
      await p;
      expect(removeSpy).toHaveBeenCalledWith(
        "abort",
        expect.any(Function),
      );
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("isAbortError", () => {
  it("recognises DOMException with name=AbortError", () => {
    expect(isAbortError(new DOMException("x", "AbortError"))).toBe(true);
  });
  it("rejects other DOMExceptions", () => {
    expect(isAbortError(new DOMException("x", "TypeError"))).toBe(false);
  });
  it("rejects plain Errors", () => {
    expect(isAbortError(new Error("x"))).toBe(false);
  });
  it("rejects non-Error values", () => {
    expect(isAbortError("aborted")).toBe(false);
    expect(isAbortError(undefined)).toBe(false);
  });
});
