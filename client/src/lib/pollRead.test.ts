import { describe, expect, it, vi } from "vitest";

import { MAX_POLL_FAILURES, readWithRetries } from "./pollRead";

describe("readWithRetries", () => {
  const sleep = vi.fn(async () => {});

  it("rides out failures and returns the read that worked", async () => {
    const read = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce(new Error("engine restarting"))
      .mockRejectedValueOnce(new Error("engine restarting"))
      .mockResolvedValue("running");
    await expect(readWithRetries(read, sleep)).resolves.toBe("running");
    expect(read).toHaveBeenCalledTimes(3);
  });

  it("gives up with the last error after the limit", async () => {
    const read = vi.fn(async () => {
      throw new Error("still down");
    });
    await expect(readWithRetries(read, sleep)).rejects.toThrow("still down");
    expect(read).toHaveBeenCalledTimes(MAX_POLL_FAILURES);
  });

  it("does not ask again when the engine says it has no such job", async () => {
    const read = vi.fn(async () => {
      throw new Error("job not found");
    });
    await expect(readWithRetries(read, sleep)).rejects.toThrow("job not found");
    expect(read).toHaveBeenCalledTimes(1);
  });
});
