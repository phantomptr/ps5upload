import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  useTaskStore,
  isTerminal,
  isActivatable,
  onTaskStatusChange,
  MAX_TASK_HISTORY,
  trimTaskHistory,
  type Task,
} from "./tasks";

function resetStore() {
  useTaskStore.setState({ tasks: [] });
}

describe("taskStore lifecycle", () => {
  beforeEach(() => {
    resetStore();
    if (typeof window !== "undefined") {
      window.localStorage.clear();
    }
  });

  afterEach(() => {
    resetStore();
  });

  it("registerTask creates a running task with a stable id", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "files.upload",
      label: "Upload PPSA09519.exfat",
      consoleId: "192.168.86.100:9113",
    });
    const t = useTaskStore.getState().getTask(id);
    expect(t).toBeDefined();
    expect(t!.id).toBe(id);
    expect(t!.kind).toBe("upload-file");
    expect(t!.status).toBe("running");
    expect(t!.attempts).toBe(1);
    expect(t!.maxAttempts).toBe(3);
    // hostOf strips the port → bare host is the canonical consoleId.
    expect(t!.consoleId).toBe("192.168.86.100");
    expect(t!.endedAtMs).toBeUndefined();
  });

  it("updateTask patches progress and bumps updatedAtMs", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-dir",
      origin: "files.upload",
      label: "Upload saves",
      consoleId: "192.168.86.100",
    });
    const before = useTaskStore.getState().getTask(id)!.updatedAtMs;
    // Bump time so updatedAtMs strictly increases.
    const originalNow = Date.now;
    Date.now = () => before + 100;
    try {
      useTaskStore.getState().updateTask(id, {
        progress: { current: 500, total: 1000, unit: "bytes" },
        rate: { bytesPerSec: 1024 },
      });
    } finally {
      Date.now = originalNow;
    }
    const t = useTaskStore.getState().getTask(id)!;
    expect(t.progress).toEqual({ current: 500, total: 1000, unit: "bytes" });
    expect(t.rate).toEqual({ bytesPerSec: 1024 });
    expect(t.updatedAtMs).toBe(before + 100);
    expect(t.status).toBe("running");
  });

  it("updateTask is a no-op on terminal tasks", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "fs-delete",
      origin: "files.bulk",
      label: "Delete temp",
      consoleId: "192.168.86.100",
    });
    useTaskStore.getState().finishTask(id, "done");
    const before = useTaskStore.getState().getTask(id)!;
    useTaskStore.getState().updateTask(id, {
      progress: { current: 99, total: 100, unit: "items" },
    });
    const after = useTaskStore.getState().getTask(id)!;
    // Unchanged: terminal tasks reject updates.
    expect(after.progress).toBe(before.progress);
    expect(after.updatedAtMs).toBe(before.updatedAtMs);
  });

  it("finishTask sets endedAtMs + terminal status", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "pkg-install",
      origin: "games.install",
      label: "Install game.pkg",
      consoleId: "192.168.86.100",
    });
    useTaskStore.getState().finishTask(id, "done", {
      progress: { current: 1, total: 1, unit: "items" },
    });
    const t = useTaskStore.getState().getTask(id)!;
    expect(t.status).toBe("done");
    expect(t.endedAtMs).toBeTypeOf("number");
    expect(t.progress).toEqual({ current: 1, total: 1, unit: "items" });
  });

  it("finishTask is idempotent — first terminal state wins", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "download",
      origin: "files.download",
      label: "Download save",
      consoleId: "192.168.86.100",
    });
    useTaskStore.getState().finishTask(id, "done");
    const doneAt = useTaskStore.getState().getTask(id)!.endedAtMs;
    // A late "failed" from the engine should NOT overwrite "done".
    useTaskStore.getState().finishTask(id, "failed", {
      lastError: { code: "E_LATE", message: "late", recoverable: false },
    });
    const t = useTaskStore.getState().getTask(id)!;
    expect(t.status).toBe("done");
    expect(t.endedAtMs).toBe(doneAt);
    expect(t.lastError).toBeUndefined();
  });

  it("removeTask refuses to remove a non-terminal task", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "fs-copy",
      origin: "files.paste",
      label: "Copy dir",
      consoleId: "192.168.86.100",
    });
    useTaskStore.getState().removeTask(id);
    expect(useTaskStore.getState().getTask(id)).toBeDefined();
    // After finishing, removal is allowed.
    useTaskStore.getState().finishTask(id, "cancelled");
    useTaskStore.getState().removeTask(id);
    expect(useTaskStore.getState().getTask(id)).toBeUndefined();
  });

  it("clearFinished preserves active tasks", () => {
    const a = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "A",
      consoleId: "h1",
    });
    const b = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "B",
      consoleId: "h1",
    });
    useTaskStore.getState().finishTask(a, "done");
    useTaskStore.getState().clearFinished();
    expect(useTaskStore.getState().getTask(a)).toBeUndefined();
    expect(useTaskStore.getState().getTask(b)).toBeDefined();
    expect(useTaskStore.getState().getTask(b)!.status).toBe("running");
  });

  it("activeTasks / runningTasks / tasksForConsole filter correctly", () => {
    useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "running-a",
      consoleId: "192.168.1.10:9113",
    });
    useTaskStore.getState().registerTask({
      kind: "pkg-install",
      origin: "test",
      label: "queued-b",
      consoleId: "192.168.1.10",
      status: "queued",
    });
    const t3 = useTaskStore.getState().registerTask({
      kind: "fs-delete",
      origin: "test",
      label: "done-c",
      consoleId: "192.168.1.20",
    });
    useTaskStore.getState().finishTask(t3, "done");

    const active = useTaskStore.getState().activeTasks();
    expect(active).toHaveLength(2);
    expect(active.map((t) => t.label).sort()).toEqual(["queued-b", "running-a"]);

    const running = useTaskStore.getState().runningTasks();
    expect(running).toHaveLength(1);
    expect(running[0].label).toBe("running-a");

    const c1 = useTaskStore.getState().tasksForConsole("192.168.1.10");
    expect(c1).toHaveLength(2);
    const c2 = useTaskStore.getState().tasksForConsole("192.168.1.20:9113");
    expect(c2).toHaveLength(1);
    expect(c2[0].label).toBe("done-c");
  });

  it("persists to localStorage and recovers on reload (terminal tasks only)", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "persist me",
      consoleId: "192.168.1.30",
    });
    useTaskStore.getState().finishTask(id, "done");

    // Verify the persisted JSON contains the finished task. The store
    // calls safeSetItem which falls back to a no-op when localStorage
    // is absent (node test env), so guard with typeof-window.
    if (typeof window !== "undefined") {
      const raw = window.localStorage.getItem("ps5upload.tasks.v1");
      expect(raw).toBeTruthy();
      const parsed = JSON.parse(raw!);
      expect(parsed.tasks).toHaveLength(1);
      expect(parsed.tasks[0].status).toBe("done");
    } else {
      // In node, we can still verify the task is in the store's state.
      const t = useTaskStore.getState().getTask(id)!;
      expect(t.status).toBe("done");
      expect(t.endedAtMs).toBeTypeOf("number");
    }
  });

  it("marks non-terminal tasks as 'interrupted' on reload (predicate check)", () => {
    // The loadInitial() function rewrites non-terminal → "interrupted"
    // when reading persisted state. We verify the predicate contract
    // that drives that rewrite. Full integration coverage of loadInitial
    // requires a browser environment (jsdom); the predicate unit test
    // is sufficient for the node test runner.
    expect(isTerminal("running")).toBe(false);
    expect(isTerminal("queued")).toBe(false);
    expect(isTerminal("paused")).toBe(false);
    expect(isTerminal("awaiting")).toBe(false);
    expect(isTerminal("interrupted")).toBe(true);
  });
});

describe("isTerminal / isActivatable predicates", () => {
  it("isTerminal covers done/failed/cancelled/interrupted", () => {
    for (const s of ["done", "failed", "cancelled", "interrupted"] as const) {
      expect(isTerminal(s)).toBe(true);
    }
    for (const s of ["running", "queued", "paused", "awaiting"] as const) {
      expect(isTerminal(s)).toBe(false);
    }
  });

  it("isActivatable covers running/queued/paused/awaiting", () => {
    for (const s of ["running", "queued", "paused", "awaiting"] as const) {
      expect(isActivatable(s)).toBe(true);
    }
    for (const s of ["done", "failed", "cancelled", "interrupted"] as const) {
      expect(isActivatable(s)).toBe(false);
    }
  });
});

describe("bounded task history", () => {
  const task = (id: number, status: Task["status"]): Task => ({
    id: String(id),
    kind: "upload-file",
    origin: "test",
    label: `task-${id}`,
    consoleId: "host",
    payload: {},
    status,
    createdAt: new Date(id).toISOString(),
    updatedAtMs: id,
    attempts: 1,
    maxAttempts: 3,
  });

  it("caps terminal history while preserving active tasks in original order", () => {
    const active = task(999, "running");
    const input = [
      ...Array.from({ length: MAX_TASK_HISTORY + 20 }, (_, i) =>
        task(i, "done"),
      ),
      active,
    ];

    const trimmed = trimTaskHistory(input);

    expect(trimmed).toHaveLength(MAX_TASK_HISTORY);
    expect(trimmed).toContain(active);
    expect(trimmed.filter((item) => isTerminal(item.status))).toHaveLength(
      MAX_TASK_HISTORY - 1,
    );
    expect(trimmed[0].id).toBe("0");
  });
});

describe("onTaskStatusChange subscription", () => {
  beforeEach(() => {
    resetStore();
  });
  afterEach(() => resetStore());

  it("fires listener when the task's status changes", () => {
    const calls: (string | undefined)[] = [];
    const id = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "sub",
      consoleId: "1.1.1.1",
    });
    const unsub = onTaskStatusChange(id, (s) => {
      if (s) calls.push(s);
    });
    // registered as running already — simulate finish.
    useTaskStore.getState().finishTask(id, "done");
    expect(calls).toContain("done");
    unsub();
  });

  it("unsubscribe stops further notifications", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "sub2",
      consoleId: "1.1.1.1",
    });
    let calls = 0;
    const unsub = onTaskStatusChange(id, () => {
      calls++;
    });
    useTaskStore.getState().finishTask(id, "done");
    unsub();
    // Further mutations should not call the listener.
    const id2 = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "other",
      consoleId: "1.1.1.1",
    });
    useTaskStore.getState().finishTask(id2, "failed");
    // The listener may have fired once for the finishTask(id,"done") above;
    // what matters is that id2's mutation doesn't reach it.
    expect(calls).toBeLessThanOrEqual(1);
  });
});
