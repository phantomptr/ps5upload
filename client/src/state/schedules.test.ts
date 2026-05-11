import { describe, expect, it } from "vitest";
import { shouldFire, type Schedule } from "./schedules";

/**
 * `shouldFire` is the pure decision function inside the schedule
 * runner — bug here means missed cron firings or double-fires.
 * Stays pure (no localStorage / no zustand) so it's testable
 * directly with synthetic Date objects.
 */
describe("shouldFire", () => {
  function dailySchedule(overrides: Partial<Schedule> = {}): Schedule {
    return {
      id: "test-1",
      enabled: true,
      kind: "daily",
      action: "notif",
      hhmm: "03:00",
      label: "test",
      ...overrides,
    };
  }

  const now = (h: number, m: number) => new Date(2026, 5, 1, h, m, 0, 0);

  it("returns false when disabled", () => {
    const s = dailySchedule({ enabled: false });
    expect(shouldFire(s, now(3, 0))).toBe(false);
  });

  it("daily fires when hh:mm matches", () => {
    const s = dailySchedule({ hhmm: "03:00" });
    expect(shouldFire(s, now(3, 0))).toBe(true);
  });

  it("daily does not fire on different minute", () => {
    const s = dailySchedule({ hhmm: "03:00" });
    expect(shouldFire(s, now(3, 1))).toBe(false);
  });

  it("daily debounces against firing twice within 60s", () => {
    const s = dailySchedule({
      hhmm: "03:00",
      lastFiredMs: Date.now() - 30_000,
    });
    expect(shouldFire(s, now(3, 0))).toBe(false);
  });

  it("daily can fire again after debounce window expires", () => {
    const s = dailySchedule({
      hhmm: "03:00",
      lastFiredMs: Date.now() - 90_000,
    });
    expect(shouldFire(s, now(3, 0))).toBe(true);
  });

  it("weekly fires only on listed weekdays", () => {
    const tue = new Date(2026, 5, 2, 3, 0, 0, 0); // 2026-06-02 = Tue
    const wed = new Date(2026, 5, 3, 3, 0, 0, 0);
    const s: Schedule = {
      id: "w-1",
      enabled: true,
      kind: "weekly",
      action: "notif",
      hhmm: "03:00",
      weekdays: [2], // Tuesday
      label: "weekly",
    };
    expect(shouldFire(s, tue)).toBe(true);
    expect(shouldFire(s, wed)).toBe(false);
  });

  it("weekly without weekdays never fires", () => {
    const s: Schedule = {
      id: "w-2",
      enabled: true,
      kind: "weekly",
      action: "notif",
      hhmm: "03:00",
      label: "weekly",
    };
    expect(shouldFire(s, now(3, 0))).toBe(false);
  });

  it("once fires when oneShotMs is in the past", () => {
    const s: Schedule = {
      id: "o-1",
      enabled: true,
      kind: "once",
      action: "notif",
      oneShotMs: Date.now() - 1000,
      label: "once",
    };
    expect(shouldFire(s, new Date())).toBe(true);
  });

  it("once does not fire when oneShotMs is in the future", () => {
    const s: Schedule = {
      id: "o-2",
      enabled: true,
      kind: "once",
      action: "notif",
      oneShotMs: Date.now() + 60_000,
      label: "once",
    };
    expect(shouldFire(s, new Date())).toBe(false);
  });

  it("once without oneShotMs never fires", () => {
    const s: Schedule = {
      id: "o-3",
      enabled: true,
      kind: "once",
      action: "notif",
      label: "once",
    };
    expect(shouldFire(s, new Date())).toBe(false);
  });

  it("daily ignores malformed hhmm", () => {
    const s = dailySchedule({ hhmm: "garbage" });
    expect(shouldFire(s, now(3, 0))).toBe(false);
  });
});
