import { describe, expect, it } from "vitest";

import {
  formatTaskAmount,
  sortActiveTasks,
  sortFinishedTasks,
  statusRailClass,
} from "./TaskList";
import type { Task } from "../state/tasks";

/**
 * The Tasks tab used to sort active tasks by `updatedAtMs`, most recent
 * first. That is the one field guaranteed to change on every poll, so with
 * two transfers running the rows traded places roughly twice a second and
 * the progress bars appeared to jump between them.
 *
 * Ordering must depend only on things that do not change while a task runs.
 */

function task(over: Partial<Task> & Pick<Task, "id">): Task {
  return {
    kind: "upload",
    origin: "files.upload",
    createdAt: "2026-08-29T10:00:00.000Z",
    status: "running",
    attempts: 0,
    maxAttempts: 3,
    consoleId: "192.0.2.100",
    payload: {},
    label: "Upload",
    updatedAtMs: 0,
    ...over,
  } as Task;
}

describe("sortActiveTasks", () => {
  it("orders by when the task started, oldest first", () => {
    const a = task({ id: "a", createdAt: "2026-08-29T10:00:00.000Z" });
    const b = task({ id: "b", createdAt: "2026-08-29T10:05:00.000Z" });
    expect(sortActiveTasks([b, a]).map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("does not reorder when a task reports progress", () => {
    // The actual bug: a poll tick bumps one task's updatedAtMs, and the
    // list must not move underneath the user because of it.
    const a = task({ id: "a", createdAt: "2026-08-29T10:00:00.000Z", updatedAtMs: 1 });
    const b = task({ id: "b", createdAt: "2026-08-29T10:05:00.000Z", updatedAtMs: 2 });
    const before = sortActiveTasks([a, b]).map((t) => t.id);
    // b ticks, then a ticks — the order they arrive must not matter.
    const afterBTick = sortActiveTasks([a, { ...b, updatedAtMs: 99 }]).map((t) => t.id);
    const afterATick = sortActiveTasks([{ ...a, updatedAtMs: 100 }, b]).map((t) => t.id);
    expect(afterBTick).toEqual(before);
    expect(afterATick).toEqual(before);
  });

  it("stays deterministic for tasks created in the same millisecond", () => {
    // Two uploads started by one click share a timestamp; without a
    // tiebreak their order would depend on array order and could still
    // flip between renders.
    const a = task({ id: "aaa", createdAt: "2026-08-29T10:00:00.000Z" });
    const b = task({ id: "bbb", createdAt: "2026-08-29T10:00:00.000Z" });
    expect(sortActiveTasks([b, a]).map((t) => t.id)).toEqual(["aaa", "bbb"]);
    expect(sortActiveTasks([a, b]).map((t) => t.id)).toEqual(["aaa", "bbb"]);
  });

  it("does not mutate the array it is given", () => {
    const a = task({ id: "a", createdAt: "2026-08-29T10:05:00.000Z" });
    const b = task({ id: "b", createdAt: "2026-08-29T10:00:00.000Z" });
    const input = [a, b];
    sortActiveTasks(input);
    expect(input.map((t) => t.id)).toEqual(["a", "b"]);
  });
});

describe("sortFinishedTasks", () => {
  it("shows the most recently finished first", () => {
    // Finished tasks may reorder freely: endedAtMs never changes again,
    // so there is nothing to churn.
    const a = task({ id: "a", status: "done", endedAtMs: 100 });
    const b = task({ id: "b", status: "done", endedAtMs: 200 });
    expect(sortFinishedTasks([a, b]).map((t) => t.id)).toEqual(["b", "a"]);
  });

  it("puts a task with no end timestamp last", () => {
    const a = task({ id: "a", status: "done", endedAtMs: 100 });
    const b = task({ id: "b", status: "done", endedAtMs: null });
    expect(sortFinishedTasks([b, a]).map((t) => t.id)).toEqual(["a", "b"]);
  });
});

/**
 * Progress is not always bytes: a batch install counts packages
 * (`unit: "items"`), and the row used to print "31 B / 100 B" for it.
 */
describe("formatTaskAmount", () => {
  it("formats byte progress as a size", () => {
    expect(formatTaskAmount(1536, "bytes")).toBe("1.50 KiB");
  });

  it("prints item and file counts as plain numbers, never as bytes", () => {
    expect(formatTaskAmount(31, "items")).toBe("31");
    expect(formatTaskAmount(100, "items")).toBe("100");
    expect(formatTaskAmount(7, "files")).toBe("7");
    expect(formatTaskAmount(31, "items")).not.toMatch(/B$/);
  });

  it("does not show a fraction of an item", () => {
    expect(formatTaskAmount(2.6, "items")).toBe("2");
  });
});

/**
 * The status rail is the row's leading edge. With a physical `border-l` it
 * stayed on the left in Arabic and Persian, where the row starts on the right.
 */
describe("statusRailClass", () => {
  it("colours the inline-start edge, so it mirrors in RTL", () => {
    for (const s of ["running", "done", "failed", "queued"] as const) {
      const cls = statusRailClass(s);
      expect(cls).toMatch(/^border-s-\[var\(--color-/);
      expect(cls).not.toContain("border-l-");
    }
  });

  it("gives each outcome its own colour", () => {
    expect(statusRailClass("running")).toContain("--color-accent");
    expect(statusRailClass("done")).toContain("--color-good");
    expect(statusRailClass("failed")).toContain("--color-bad");
    expect(statusRailClass("interrupted")).toContain("--color-bad");
    expect(statusRailClass("queued")).toContain("--color-border-strong");
  });
});
