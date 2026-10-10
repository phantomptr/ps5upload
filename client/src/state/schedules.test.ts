import { describe, expect, it, vi } from "vitest";

vi.mock("./notifications", () => ({ pushNotification: vi.fn() }));

import {
  CATCH_UP_MS,
  evaluateSchedule,
  latestDue,
  parseSchedules,
  type Schedule,
} from "./schedules";

/**
 * `evaluateSchedule` is the pure decision inside the schedule runner — a
 * bug here means missed reminders or double-fires. Pure (no storage, no
 * zustand), so it's tested directly with synthetic times.
 */
describe("evaluateSchedule", () => {
  function daily(overrides: Partial<Schedule> = {}): Schedule {
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
  // 2026-06-01 is a Monday.
  const at = (d: number, h: number, m: number, sec = 0) =>
    new Date(2026, 5, d, h, m, sec, 0).getTime();

  it("does nothing when disabled", () => {
    expect(evaluateSchedule(daily({ enabled: false }), at(1, 3, 0))).toBeNull();
  });

  it("fires at the due minute", () => {
    expect(evaluateSchedule(daily(), at(1, 3, 0))).toEqual({
      kind: "fire",
      dueMs: at(1, 3, 0),
    });
  });

  it("still fires when the tick lands after the due minute (no skipped day)", () => {
    // The old exact-minute check missed this whenever a 30 s tick slipped.
    expect(evaluateSchedule(daily(), at(1, 3, 1, 10))?.kind).toBe("fire");
    expect(evaluateSchedule(daily(), at(1, 3, 14))?.kind).toBe("fire");
  });

  it("does not fire before the due time", () => {
    // Previous day's 03:00 is long past and nothing says it was live then.
    expect(evaluateSchedule(daily(), at(1, 2, 59))).toBeNull();
  });

  it("does not fire the same run twice", () => {
    const s = daily({ lastFiredMs: at(1, 3, 0, 20) });
    expect(evaluateSchedule(s, at(1, 3, 1))).toBeNull();
    expect(evaluateSchedule(s, at(1, 3, 10))).toBeNull();
    // ...but fires the next day's run.
    expect(evaluateSchedule(s, at(2, 3, 0))?.kind).toBe("fire");
  });

  it("records a run as missed when it is too late to fire", () => {
    const s = daily({ lastFiredMs: at(1, 3, 0) });
    // App reopened the next afternoon.
    expect(evaluateSchedule(s, at(2, 15, 0))).toEqual({
      kind: "missed",
      dueMs: at(2, 3, 0),
    });
    // Already recorded: not again.
    expect(evaluateSchedule({ ...s, missedAtMs: at(2, 3, 0) }, at(2, 15, 1))).toBeNull();
  });

  it("the catch-up window is capped", () => {
    const s = daily({ armedMs: at(1, 0, 0) });
    expect(evaluateSchedule(s, at(1, 3, 0) + CATCH_UP_MS)?.kind).toBe("fire");
    expect(evaluateSchedule(s, at(1, 3, 0) + CATCH_UP_MS + 1000)?.kind).toBe("missed");
  });

  it("a run due before the schedule was armed neither fires nor counts as missed", () => {
    const s = daily({ armedMs: at(1, 3, 5) });
    expect(evaluateSchedule(s, at(1, 3, 6))).toBeNull();
    expect(evaluateSchedule(s, at(1, 16, 0))).toBeNull();
  });

  it("older schedules without armedMs or a fire are never reported missed", () => {
    expect(evaluateSchedule(daily(), at(1, 15, 0))).toBeNull();
  });

  it("weekly is due only on listed weekdays", () => {
    const s = daily({ kind: "weekly", weekdays: [1] }); // Monday
    expect(evaluateSchedule(s, at(1, 3, 0))?.kind).toBe("fire");
    expect(evaluateSchedule(s, at(2, 3, 0))).toBeNull(); // Tuesday
    expect(latestDue(s, at(3, 12, 0))).toBe(at(1, 3, 0));
  });

  it("weekly without weekdays is never due", () => {
    expect(latestDue(daily({ kind: "weekly", weekdays: [] }), at(1, 3, 0))).toBeNull();
    expect(latestDue(daily({ kind: "weekly" }), at(1, 3, 0))).toBeNull();
  });

  it("once fires when its time has come, not before", () => {
    const s = daily({ kind: "once", oneShotMs: at(1, 10, 0), hhmm: undefined });
    expect(evaluateSchedule(s, at(1, 9, 59))).toBeNull();
    expect(evaluateSchedule(s, at(1, 10, 2))?.kind).toBe("fire");
  });

  it("once without a time never fires", () => {
    expect(evaluateSchedule(daily({ kind: "once", hhmm: undefined }), at(1, 3, 0))).toBeNull();
  });

  it("ignores a malformed time", () => {
    expect(evaluateSchedule(daily({ hhmm: "garbage" }), at(1, 3, 0))).toBeNull();
    expect(evaluateSchedule(daily({ hhmm: "25:00" }), at(1, 3, 0))).toBeNull();
  });
});

describe("parseSchedules", () => {
  it("drops stored PS5 power-tick schedules and keeps reminders", () => {
    const raw = JSON.stringify([
      { id: "a", enabled: true, kind: "daily", action: "notif", hhmm: "03:00", label: "back up" },
      { id: "b", enabled: true, kind: "daily", action: "power_tick", hhmm: "02:00", label: "nightly tick", host: "192.168.0.5" },
      { id: "c", enabled: true, kind: "daily", action: "something_else", hhmm: "01:00", label: "x" },
    ]);
    expect(parseSchedules(raw).map((s) => s.id)).toEqual(["a"]);
  });

  it("is empty for nothing stored, junk, or a non-list", () => {
    expect(parseSchedules(null)).toEqual([]);
    expect(parseSchedules("{oops")).toEqual([]);
    expect(parseSchedules(JSON.stringify({ id: "a" }))).toEqual([]);
  });
});
