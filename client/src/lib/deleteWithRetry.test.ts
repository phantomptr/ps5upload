import { describe, expect, it, vi } from "vitest";
import { deleteWithRetry } from "./deleteWithRetry";

describe("deleteWithRetry", () => {
  it("returns ok=true with attempts=1 when the first call succeeds", async () => {
    const deleter = vi.fn().mockResolvedValue(undefined);
    const sleep = vi.fn().mockResolvedValue(undefined);
    const result = await deleteWithRetry({ deleter, sleep });
    expect(result).toEqual({ ok: true, attempts: 1 });
    expect(deleter).toHaveBeenCalledTimes(1);
    expect(sleep).not.toHaveBeenCalled();
  });

  it("retries on failure and succeeds on a later attempt", async () => {
    const deleter = vi
      .fn()
      .mockRejectedValueOnce(new Error("EBUSY"))
      .mockResolvedValueOnce(undefined);
    const sleep = vi.fn().mockResolvedValue(undefined);
    const result = await deleteWithRetry({ deleter, sleep });
    expect(result).toEqual({ ok: true, attempts: 2 });
    expect(deleter).toHaveBeenCalledTimes(2);
    // One sleep between attempt 1 and attempt 2.
    expect(sleep).toHaveBeenCalledTimes(1);
    expect(sleep).toHaveBeenCalledWith(500);
  });

  it("returns ok=false after exhausting maxAttempts and exposes lastError", async () => {
    const errors = [
      new Error("EBUSY"),
      new Error("EAGAIN"),
      new Error("io error"),
    ];
    const deleter = vi
      .fn()
      .mockRejectedValueOnce(errors[0])
      .mockRejectedValueOnce(errors[1])
      .mockRejectedValueOnce(errors[2]);
    const sleep = vi.fn().mockResolvedValue(undefined);
    const result = await deleteWithRetry({ deleter, sleep });
    expect(result).toEqual({
      ok: false,
      attempts: 3,
      lastError: errors[2],
    });
    expect(deleter).toHaveBeenCalledTimes(3);
    // Two backoff sleeps: between 1↔2 (500ms) and 2↔3 (1000ms).
    expect(sleep).toHaveBeenCalledTimes(2);
    expect(sleep.mock.calls).toEqual([[500], [1000]]);
  });

  it("does not sleep after the final failed attempt", async () => {
    // Important — sleeping after the last attempt would needlessly
    // delay the surface of the failure to the user.
    const deleter = vi.fn().mockRejectedValue(new Error("nope"));
    const sleep = vi.fn().mockResolvedValue(undefined);
    await deleteWithRetry({ deleter, sleep, maxAttempts: 2 });
    expect(deleter).toHaveBeenCalledTimes(2);
    expect(sleep).toHaveBeenCalledTimes(1);
  });

  it("invokes onAttemptFail for every failed attempt", async () => {
    const deleter = vi
      .fn()
      .mockRejectedValueOnce(new Error("first"))
      .mockResolvedValueOnce(undefined);
    const onAttemptFail = vi.fn();
    await deleteWithRetry({
      deleter,
      sleep: vi.fn().mockResolvedValue(undefined),
      onAttemptFail,
    });
    expect(onAttemptFail).toHaveBeenCalledTimes(1);
    expect(onAttemptFail).toHaveBeenCalledWith(1, new Error("first"));
  });

  it("respects custom maxAttempts and backoffMs", async () => {
    const deleter = vi.fn().mockRejectedValue(new Error("x"));
    const sleep = vi.fn().mockResolvedValue(undefined);
    await deleteWithRetry({
      deleter,
      sleep,
      maxAttempts: 4,
      backoffMs: 100,
    });
    expect(deleter).toHaveBeenCalledTimes(4);
    // 100, 200, 300 — three sleeps before the fourth attempt.
    expect(sleep.mock.calls).toEqual([[100], [200], [300]]);
  });

  it("treats non-Error throws (string, RPC object) as lastError without coercion", async () => {
    const rpcError = { code: -32603, message: "internal" };
    const deleter = vi.fn().mockRejectedValue(rpcError);
    const sleep = vi.fn().mockResolvedValue(undefined);
    const result = await deleteWithRetry({
      deleter,
      sleep,
      maxAttempts: 1,
    });
    expect(result).toEqual({
      ok: false,
      attempts: 1,
      lastError: rpcError,
    });
  });
});
