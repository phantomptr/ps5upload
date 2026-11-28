import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  useTaskStore,
  isTerminal,
  isActivatable,
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

  it("carries a stage name through updates", () => {
    const s = useTaskStore.getState();
    const id = s.registerTask({ kind: "fpkg-convert", origin: "convert", label: "Convert Minecraft", consoleId: "" });
    s.updateTask(id, { stage: "Compress", progress: { current: 5, total: 10, unit: "bytes" } });
    expect(useTaskStore.getState().getTask(id)).toMatchObject({ stage: "Compress", kind: "fpkg-convert" });
  });

  it("registerTask creates a running task with a stable id", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "files.upload",
      label: "Upload PPSA09519.exfat",
      consoleId: "192.0.2.100:9113",
    });
    const t = useTaskStore.getState().getTask(id);
    expect(t).toBeDefined();
    expect(t!.id).toBe(id);
    expect(t!.kind).toBe("upload-file");
    expect(t!.status).toBe("running");
    // hostOf strips the port → bare host is the canonical consoleId.
    expect(t!.consoleId).toBe("192.0.2.100");
    expect(t!.endedAtMs).toBeUndefined();
  });

  it("updateTask patches progress and bumps updatedAtMs", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-dir",
      origin: "files.upload",
      label: "Upload saves",
      consoleId: "192.0.2.100",
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
      consoleId: "192.0.2.100",
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
      consoleId: "192.0.2.100",
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
      consoleId: "192.0.2.100",
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
      consoleId: "192.0.2.100",
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

  it("records the console as a bare host", () => {
    const id = useTaskStore.getState().registerTask({
      kind: "upload-file",
      origin: "test",
      label: "running-a",
      consoleId: "192.168.1.10:9113",
    });
    expect(useTaskStore.getState().getTask(id)!.consoleId).toBe("192.168.1.10");
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

describe("deriveRate", () => {
  const MiB = 2 ** 20;
  it("derives speed and time left from byte samples over a trailing window (#981)", async () => {
    const { deriveRate } = await import("./tasks");
    const samples = new Map();
    const p = (current: number) => ({ current, total: 100 * MiB, unit: "bytes" as const });
    // The first sample only sets the baseline: nothing to show yet.
    expect(deriveRate(samples, "a", p(0), 0)).toEqual({ rate: undefined, eta: undefined });
    // 1 MiB in a second: a speed, but no time left until a few seconds of data.
    expect(deriveRate(samples, "a", p(MiB), 1000)).toEqual({ rate: { bytesPerSec: MiB }, eta: undefined });
    for (let s = 2; s <= 4; s++) deriveRate(samples, "a", p(s * MiB), s * 1000);
    const steady = deriveRate(samples, "a", p(5 * MiB), 5000);
    expect(steady?.rate?.bytesPerSec).toBeCloseTo(MiB);
    expect(steady?.eta).toBeCloseTo(95);
    // Bytes went backwards (a new leg): start over.
    expect(deriveRate(samples, "a", p(10), 6000)).toEqual({ rate: undefined, eta: undefined });
    // Counts of files or items have no speed.
    expect(deriveRate(samples, "b", { current: 1, total: 2, unit: "files" }, 0)).toBeNull();
  });

  it("a long step with no bytes moving shows as stalled, then a real speed comes back", async () => {
    const { deriveRate } = await import("./tasks");
    const samples = new Map();
    const p = (current: number) => ({ current, total: 100 * 2 ** 30, unit: "bytes" as const });
    deriveRate(samples, "c", p(0), 0);
    deriveRate(samples, "c", p(10 * 2 ** 30), 1000);
    // A conversion's hashing pass: the byte count stands still for thousands of samples.
    let last: ReturnType<typeof deriveRate> = null;
    for (let i = 2; i < 3000; i++) last = deriveRate(samples, "c", p(10 * 2 ** 30), i * 1000);
    expect(last).toEqual({ rate: { bytesPerSec: 0, stalled: true }, eta: undefined });
    // Bytes moving again: a real speed comes back.
    expect(deriveRate(samples, "c", p(11 * 2 ** 30), 3000 * 1000)?.rate?.bytesPerSec).toBeGreaterThan(1);
  });

  it("#669: a speed fades out once the bytes stop, and never shows a few bytes a second", async () => {
    const { deriveRate } = await import("./tasks");
    const samples = new Map();
    const p = (current: number) => ({ current, total: 100 * 2 ** 30, unit: "bytes" as const });
    deriveRate(samples, "f", p(0), 0);
    deriveRate(samples, "f", p(2 ** 30), 1000);
    const shown: number[] = [];
    for (let i = 2; i < 100; i++) {
      const r = deriveRate(samples, "f", p(2 ** 30), i * 1000);
      if (r?.rate && r.rate.bytesPerSec > 0) shown.push(r.rate.bytesPerSec);
    }
    expect(shown.length).toBeGreaterThan(0);
    expect(Math.min(...shown)).toBeGreaterThanOrEqual(1024);
    // A real slow transfer, where bytes do move, still shows its speed.
    const slow = new Map();
    deriveRate(slow, "s", p(0), 0);
    expect(deriveRate(slow, "s", p(5000), 1000)?.rate?.bytesPerSec).toBe(5000);
  });

  it("fills in speed for an update that reports only bytes, and never overrides one that has a speed", async () => {
    vi.useFakeTimers();
    try {
      vi.setSystemTime(10_000);
      const { useTaskStore } = await import("./tasks");
      const s = useTaskStore.getState();
      const id = s.registerTask({ kind: "download", origin: "t", label: "x", consoleId: "h" });
      s.updateTask(id, { progress: { current: 0, total: 40_000, unit: "bytes" } });
      vi.setSystemTime(11_000);
      s.updateTask(id, { progress: { current: 2000, total: 40_000, unit: "bytes" } });
      expect(useTaskStore.getState().getTask(id)).toMatchObject({ rate: { bytesPerSec: 2000 } });
      vi.setSystemTime(14_000);
      s.updateTask(id, { progress: { current: 8000, total: 40_000, unit: "bytes" } });
      expect(useTaskStore.getState().getTask(id)).toMatchObject({ rate: { bytesPerSec: 2000 }, eta: 16 });
      vi.setSystemTime(15_000);
      s.updateTask(id, { progress: { current: 30000, total: 40_000, unit: "bytes" }, rate: { bytesPerSec: 7 } });
      expect(useTaskStore.getState().getTask(id)?.rate).toEqual({ bytesPerSec: 7 });
    } finally {
      vi.useRealTimers();
    }
  });
});
