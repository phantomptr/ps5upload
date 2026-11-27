import { describe, expect, it } from "vitest";
import { speedAndEta } from "./taskWiring";

describe("speedAndEta (#444)", () => {
  it("gives a running transfer's task its speed and time left", () => {
    expect(speedAndEta(25, 125, 10)).toEqual({ rate: { bytesPerSec: 10 }, eta: 10 });
    expect(speedAndEta(5, 0, 10)).toEqual({ rate: { bytesPerSec: 10 } });
    expect(speedAndEta(5, 100, 0)).toEqual({});
  });
});

describe("an upload picked back up after a reload (#954)", () => {
  it("runs again in its earlier task instead of beside an Interrupted one", async () => {
    const { installTaskWiring } = await import("./taskWiring");
    const { interruptedAtLoad, useTaskStore } = await import("./tasks");
    const { useUploadQueueStore } = await import("./uploadQueue");
    installTaskWiring();

    const item = {
      id: "q-1",
      addr: "192.168.0.2:9113",
      sourcePath: "/games/Game",
      sourceKind: "folder",
      displayName: "Game",
      resolvedDest: "/data/homebrew/Game",
      status: "pending",
      bytesSent: 0,
      totalBytes: 100,
    };
    const set = (status: string, bytesSent = 0) =>
      useUploadQueueStore.setState((s) => ({
        items: [{ ...(s.items[0] ?? item), ...item, status, bytesSent } as never],
      }));
    set("pending");
    set("running", 10);
    const tasksFor = () =>
      useTaskStore.getState().tasks.filter((t) => (t.payload as { queueItemId?: string })?.queueItemId === "q-1");
    expect(tasksFor()).toHaveLength(1);
    const first = tasksFor()[0];

    // The page reloads: the upload's task is marked interrupted and the queue row waits for the
    // engine to say whether its job is still going.
    useTaskStore.setState((s) => ({
      tasks: s.tasks.map((t) => {
        if (t.id !== first.id) return t;
        interruptedAtLoad.add(t.id);
        return { ...t, status: "interrupted" as const, endedAtMs: t.updatedAtMs };
      }),
    }));
    set("pending");
    set("running", 50);

    expect(tasksFor()).toHaveLength(1);
    expect(tasksFor()[0].id).toBe(first.id);
    expect(tasksFor()[0].status).toBe("running");

    set("done", 100);
    expect(tasksFor()[0].status).toBe("done");
  });
});
