import { beforeEach, describe, expect, it } from "vitest";

// The task store persists through `window.localStorage`; vitest's node env has no window.
const mem = new globalThis.Map<string, string>();
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

const { useTaskStore } = await import("./tasks");
const { beginTask, trackTask } = await import("./trackTask");

describe("trackTask", () => {
  beforeEach(() => useTaskStore.setState({ tasks: [] }));

  it("ends the task done when the operation succeeds", async () => {
    const value = await trackTask(
      { kind: "backup-snapshot", origin: "backup", label: "Backup PS5" },
      async (report) => {
        report({ stage: "Copying" });
        return 1;
      },
    );
    expect(value).toBe(1);
    const t = useTaskStore.getState().tasks.find((x) => x.label === "Backup PS5");
    expect(t).toMatchObject({ status: "done", stage: "Copying" });
  });

  it("shows the task running while the operation is in flight", async () => {
    let seen: string | undefined;
    await trackTask({ kind: "bug-report", origin: "bug-report", label: "Bug report" }, async () => {
      seen = useTaskStore.getState().tasks[0]?.status;
    });
    expect(seen).toBe("running");
  });

  it("ends the task failed, never running, when the operation throws", async () => {
    await expect(
      trackTask({ kind: "save-restore", origin: "saves", label: "Restore save" }, async () => {
        throw new Error("USB gone");
      }),
    ).rejects.toThrow("USB gone");
    const t = useTaskStore.getState().tasks.find((x) => x.label === "Restore save");
    expect(t).toMatchObject({ status: "failed", lastError: { message: "USB gone" } });
  });

  it("settles a begun task once: a failure is not overwritten by the finally's done", () => {
    const t = beginTask({ kind: "save-backup", origin: "saves", label: "Save backup A" });
    t.report({ stage: "Zipping" });
    t.fail(new Error("disk full"));
    t.done();
    expect(useTaskStore.getState().tasks[0]).toMatchObject({
      status: "failed",
      stage: "Zipping",
      lastError: { message: "disk full" },
    });
  });
});
