import { beforeEach, describe, expect, it } from "vitest";

// vitest's node env has no `window`; both stores persist through
// `window.localStorage`, so give them the same in-memory stub the settings
// store tests use.
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

const { useActivityHistoryStore } = await import("./activityHistory");
const { useTaskStore } = await import("./tasks");
const { installLibraryTaskBridge } = await import("./libraryTaskBridge");

describe("library task bridge", () => {
  beforeEach(() => {
    installLibraryTaskBridge();
  });

  it("mirrors a library action into a task and ends it", () => {
    const a = useActivityHistoryStore.getState();
    const id = a.start("library-move", "Move Game.exfat", {
      addr: "10.0.0.2:9113",
      totalBytes: 100,
    });
    const t = () =>
      useTaskStore.getState().tasks.find((x) => x.label === "Move Game.exfat");
    expect(t()).toMatchObject({ kind: "library-op", status: "running" });
    useActivityHistoryStore.getState().update(id, { bytes: 40 });
    expect(t()?.progress).toMatchObject({ current: 40, total: 100 });
    useActivityHistoryStore.getState().finish(id, "done");
    expect(t()?.status).toBe("done");
  });

  it("keeps the specific library kinds and records the failure reason", () => {
    const a = useActivityHistoryStore.getState();
    const id = a.start("library-mount", "Mount Game.exfat");
    const t = () =>
      useTaskStore.getState().tasks.find((x) => x.label === "Mount Game.exfat");
    expect(t()?.kind).toBe("library-mount");
    useActivityHistoryStore.getState().finish(id, "failed", { error: "busy" });
    expect(t()).toMatchObject({
      status: "failed",
      lastError: { code: "LIBRARY_OP_FAILED", message: "busy" },
    });
  });

  it("ends a stopped action as cancelled", () => {
    const id = useActivityHistoryStore
      .getState()
      .start("library-delete", "Delete Old.exfat");
    useActivityHistoryStore.getState().finish(id, "stopped");
    expect(
      useTaskStore.getState().tasks.find((x) => x.label === "Delete Old.exfat")
        ?.status,
    ).toBe("cancelled");
  });

  it("never mirrors uploads, which the task store already has", () => {
    const before = useTaskStore.getState().tasks.length;
    useActivityHistoryStore.getState().start("upload", "Upload A.exfat");
    expect(useTaskStore.getState().tasks.length).toBe(before);
  });

  it("leaves library installs to the install task they already register", () => {
    const before = useTaskStore.getState().tasks.length;
    useActivityHistoryStore.getState().start("library-install", "Installing Game.pkg");
    expect(useTaskStore.getState().tasks.length).toBe(before);
  });

  it("shows a library download as a download", () => {
    useActivityHistoryStore.getState().start("library-download", "Downloading Game.exfat");
    expect(
      useTaskStore.getState().tasks.find((x) => x.label === "Downloading Game.exfat")?.kind,
    ).toBe("download");
  });
});
