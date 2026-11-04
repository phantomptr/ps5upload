import { expect, it, vi } from "vitest";

// Loading persisted tasks: what was still running when the app closed comes back interrupted,
// and the store remembers which ones this start-up interrupted (the activity bar lists them).
it("records which tasks this start-up interrupted", async () => {
  const mem = new Map<string, string>();
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (k: string) => (mem.has(k) ? (mem.get(k) as string) : null),
      setItem: (k: string, v: string) => void mem.set(k, String(v)),
      removeItem: (k: string) => void mem.delete(k),
      clear: () => mem.clear(),
    },
    addEventListener: () => {},
    removeEventListener: () => {},
  };
  const base = { origin: "x", createdAt: new Date(0).toISOString(), consoleId: "", payload: {}, label: "L", updatedAtMs: 5, endedAtMs: null };
  mem.set(
    "ps5upload.tasks.v1",
    JSON.stringify({
      tasks: [
        { ...base, id: "run", kind: "fpkg-convert", status: "running" },
        { ...base, id: "old", kind: "fpkg-convert", status: "done", endedAtMs: 4 },
        // A console page's task kept running on the console: the next read finds it, not "interrupted".
        { ...base, id: "console-run", kind: "console-task", status: "running" },
        { ...base, id: "console-old", kind: "console-task", status: "done", endedAtMs: 4 },
      ],
    }),
  );
  vi.resetModules();
  const { interruptedAtLoad, useTaskStore } = await import("./tasks");
  expect(useTaskStore.getState().tasks.find((t) => t.id === "run")?.status).toBe("interrupted");
  expect([...interruptedAtLoad]).toEqual(["run"]);
  expect(useTaskStore.getState().tasks.some((t) => t.id === "console-run")).toBe(false);
  expect(useTaskStore.getState().tasks.find((t) => t.id === "console-old")?.status).toBe("done");
});
