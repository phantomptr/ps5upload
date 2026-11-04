import { afterEach, beforeEach, expect, it, vi } from "vitest";

const checks = vi.hoisted(() => vi.fn());
vi.mock("../api/collection", () => ({ collection: { checks, check: vi.fn(), cancelCheck: vi.fn() } }));

import { refreshCopyChecks, useCopyChecks } from "./copyChecks";

beforeEach(() => {
  vi.useFakeTimers();
  checks.mockReset();
  useCopyChecks.setState({ byPath: {}, error: null });
});
afterEach(() => vi.useRealTimers());

const running = { path: "/a", state: "running" } as never;

it("keeps looking after a failed read while a check runs, and clears the error", async () => {
  useCopyChecks.setState({ byPath: { "/a": running } });
  checks.mockRejectedValueOnce(new Error("engine restarting"));
  await refreshCopyChecks();
  expect(useCopyChecks.getState().error).toBe("engine restarting");
  checks.mockResolvedValueOnce({ checks: [{ path: "/a", state: "ok" }] });
  await vi.advanceTimersByTimeAsync(5_000);
  expect(useCopyChecks.getState().error).toBeNull();
  expect(useCopyChecks.getState().byPath["/a"]).toMatchObject({ state: "ok" });
});

it("does not poll on an error with nothing running", async () => {
  checks.mockRejectedValue(new Error("down"));
  await refreshCopyChecks();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(checks).toHaveBeenCalledTimes(1);
});
