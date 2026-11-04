import { beforeEach, describe, expect, it } from "vitest";

import { useLogsStore, type LogInput } from "./logs";

const line = (level: LogInput["level"], message = "m"): LogInput => ({
  level,
  source: "test",
  message,
});

describe("logs store", () => {
  beforeEach(() => useLogsStore.getState().clear());

  it("appendMany adds a batch in one update", () => {
    let updates = 0;
    const unsub = useLogsStore.subscribe(() => updates++);
    useLogsStore
      .getState()
      .appendMany([line("info"), line("error"), line("warn"), line("error")]);
    unsub();
    expect(updates).toBe(1);
    expect(useLogsStore.getState().entries).toHaveLength(4);
    expect(useLogsStore.getState().errorCount).toBe(2);
  });

  it("keeps the error count in step as the ring drops old lines", () => {
    const { appendMany } = useLogsStore.getState();
    appendMany(Array.from({ length: 10 }, () => line("error")));
    appendMany(Array.from({ length: 495 }, () => line("info")));
    // 505 lines into a 500 ring: the first 5 errors fell off.
    const s = useLogsStore.getState();
    expect(s.entries).toHaveLength(500);
    expect(s.errorCount).toBe(5);
    expect(s.errorCount).toBe(s.entries.filter((e) => e.level === "error").length);
  });

  it("counts correctly when one batch is bigger than the ring", () => {
    useLogsStore.getState().appendMany([line("error")]);
    const batch = [
      ...Array.from({ length: 300 }, () => line("error")),
      ...Array.from({ length: 300 }, () => line("info")),
    ];
    useLogsStore.getState().appendMany(batch);
    const s = useLogsStore.getState();
    expect(s.entries).toHaveLength(500);
    expect(s.errorCount).toBe(s.entries.filter((e) => e.level === "error").length);
    expect(s.errorCount).toBe(200);
  });

  it("debug lines cannot push a warning out", () => {
    const { appendMany } = useLogsStore.getState();
    appendMany([line("warn", "keep me"), line("error", "and me")]);
    for (let i = 0; i < 20; i++) {
      appendMany(Array.from({ length: 100 }, () => line("debug")));
      appendMany(Array.from({ length: 10 }, () => line("trace")));
    }
    const s = useLogsStore.getState();
    expect(s.entries.map((e) => e.message)).toEqual(expect.arrayContaining(["keep me", "and me"]));
    expect(s.errorCount).toBe(1);
    expect(s.entries.filter((e) => e.level === "debug" || e.level === "trace").length).toBe(300);
    // still in the order they came
    const ids = s.entries.map((e) => e.id);
    expect([...ids].sort((a, b) => a - b)).toEqual(ids);
  });

  it("append still works one line at a time; clear resets the count", () => {
    useLogsStore.getState().append("error", "x", "boom");
    expect(useLogsStore.getState().errorCount).toBe(1);
    useLogsStore.getState().clear();
    expect(useLogsStore.getState().errorCount).toBe(0);
    expect(useLogsStore.getState().entries).toHaveLength(0);
  });
});
