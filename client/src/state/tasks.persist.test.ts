import { afterEach, beforeEach, expect, it, vi } from "vitest";

const writes = vi.hoisted(() => [] as string[]);
vi.mock("../lib/safeStorage", () => ({
  safeGetItem: () => null,
  safeSetItem: (_k: string, v: string) => void writes.push(v),
}));

import { useTaskStore } from "./tasks";

beforeEach(() => {
  vi.useFakeTimers();
  writes.length = 0;
  useTaskStore.setState({ tasks: [] });
});
afterEach(() => vi.useRealTimers());

const newTask = () =>
  useTaskStore.getState().registerTask({
    kind: "upload-file",
    origin: "test",
    label: "ticks",
    consoleId: "192.168.1.31",
  });

it("writes progress at most every 5 s, also while ticks never pause", () => {
  const id = newTask();
  writes.length = 0;
  for (let i = 0; i < 200; i++) {
    useTaskStore.getState().updateTask(id, { stage: `s${i}` });
    vi.advanceTimersByTime(100);
  }
  // 20 s of 10 Hz ticks: not one write per tick, and not none (a debounce would never find a
  // quiet window while a transfer runs).
  expect(writes.length).toBeGreaterThanOrEqual(3);
  expect(writes.length).toBeLessThanOrEqual(5);
});

it("writes a task's end at once, with no stale progress write after it", () => {
  const id = newTask();
  writes.length = 0;
  useTaskStore.getState().updateTask(id, { stage: "last" });
  expect(writes).toHaveLength(0);
  useTaskStore.getState().finishTask(id, "done");
  expect(writes).toHaveLength(1);
  const saved = JSON.parse(writes[0]) as { tasks: Array<{ id: string; status: string; stage?: string }> };
  expect(saved.tasks.find((t) => t.id === id)).toMatchObject({ status: "done", stage: "last" });
  vi.advanceTimersByTime(2000);
  expect(writes).toHaveLength(1);
});
