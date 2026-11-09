import { describe, expect, it } from "vitest";

import {
  BLIND_RETRY_MS,
  CONFIRM_CHECKS,
  ENGINE_STARTING_RETRY_MS,
  nextCheckDelay,
  STATUS_POLL_MS,
  statusAfterMiss,
  UP_MISS_THRESHOLD,
  WAKE_POLL_MS,
} from "./statusSchedule";

describe("statusAfterMiss", () => {
  it("never calls a console not yet seen down on one failed check", () => {
    expect(statusAfterMiss("unknown", 1)).toBe("unknown");
    expect(statusAfterMiss("unknown", CONFIRM_CHECKS - 1)).toBe("unknown");
    expect(statusAfterMiss("unknown", CONFIRM_CHECKS)).toBe("down");
  });

  it("holds a console that was up through a few misses", () => {
    for (let n = 1; n < UP_MISS_THRESHOLD; n++) expect(statusAfterMiss("up", n)).toBe("up");
    expect(statusAfterMiss("up", UP_MISS_THRESHOLD)).toBe("down");
    // During an upload the threshold is the caller's.
    expect(statusAfterMiss("up", 5, { upThreshold: 6 })).toBe("up");
    expect(statusAfterMiss("down", 1)).toBe("down");
  });
});

describe("nextCheckDelay", () => {
  const now = 1_000_000;

  it("retries within a second while the engine is still starting", () => {
    expect(
      nextCheckDelay({ outcome: "blind", status: "unknown", streak: 1, engineStarting: true, now }),
    ).toBe(ENGINE_STARTING_RETRY_MS);
  });

  it("backs off a check without a verdict, up to the normal pace", () => {
    const d = (streak: number) => nextCheckDelay({ outcome: "blind", status: "up", streak, now });
    expect(d(1)).toBe(BLIND_RETRY_MS);
    expect(d(2)).toBe(BLIND_RETRY_MS * 2);
    expect(d(10)).toBe(STATUS_POLL_MS);
  });

  it("confirms a first failure quickly, then settles", () => {
    const d = (streak: number, status: "unknown" | "up" | "down") =>
      nextCheckDelay({ outcome: "miss", status, streak, now });
    expect(d(1, "unknown")).toBe(1_000);
    expect(d(2, "unknown")).toBe(2_000);
    expect(d(3, "down")).toBe(STATUS_POLL_MS);
    // A console that was up: slower confirming checks, so a busy helper has time.
    expect(d(1, "up")).toBe(3_000);
    expect(d(2, "up")).toBe(5_000);
  });

  it("keeps the normal pace for settled consoles and during uploads", () => {
    expect(nextCheckDelay({ outcome: "up", status: "up", streak: 0, now })).toBe(STATUS_POLL_MS);
    expect(nextCheckDelay({ outcome: "rest", status: "down", streak: 3, now })).toBe(
      STATUS_POLL_MS,
    );
    expect(
      nextCheckDelay({ outcome: "miss", status: "up", streak: 1, transferring: true, now }),
    ).toBe(STATUS_POLL_MS);
  });

  it("asks a console that was just woken every few seconds until it is up", () => {
    const wakeUntil = now + 30_000;
    expect(nextCheckDelay({ outcome: "rest", status: "down", streak: 3, wakeUntil, now })).toBe(
      WAKE_POLL_MS,
    );
    expect(nextCheckDelay({ outcome: "up", status: "up", streak: 0, wakeUntil, now })).toBe(
      STATUS_POLL_MS,
    );
    // The window is over: back to the normal pace.
    expect(
      nextCheckDelay({ outcome: "rest", status: "down", streak: 3, wakeUntil, now: wakeUntil + 1 }),
    ).toBe(STATUS_POLL_MS);
  });
});
