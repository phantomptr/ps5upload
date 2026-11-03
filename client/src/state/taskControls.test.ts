import { beforeEach, describe, expect, it, vi } from "vitest";

const owners = vi.hoisted(() => ({
  transfer: {
    phasesByHost: {} as Record<string, { kind: string }>,
    cancel: vi.fn(),
  },
  bulk: {
    byHost: {} as Record<string, { op: string | null; cancelRequested: boolean }>,
    requestCancel: vi.fn(),
  },
  queue: {
    items: [] as Array<{ id: string; status: string }>,
    cancelItem: vi.fn(),
    retryItem: vi.fn(() => true),
    startHost: vi.fn(async () => {}),
  },
}));

vi.mock("./transfer", () => ({
  useTransferStore: { getState: () => owners.transfer },
}));
vi.mock("./fsBulkOp", () => ({
  useFsBulkOpStore: { getState: () => owners.bulk },
}));
const invoke = vi.hoisted(() => vi.fn(async () => ({})));
vi.mock("../lib/invokeLogged", () => ({ invoke }));
vi.mock("./uploadQueue", () => ({
  useUploadQueueStore: { getState: () => owners.queue },
}));

import { commandTask, taskCapabilities } from "./taskControls";
import type { Task } from "./tasks";

const task = (control: Task["control"], status: Task["status"] = "running") =>
  ({ id: "task", status, control } as Task);

describe("real task controls", () => {
  beforeEach(() => {
    owners.transfer.phasesByHost = {};
    owners.bulk.byHost = {};
    owners.queue.items = [];
    owners.transfer.cancel.mockClear();
    owners.bulk.requestCancel.mockClear();
    owners.queue.cancelItem.mockClear();
    owners.queue.retryItem.mockReset().mockReturnValue(true);
    owners.queue.startHost.mockClear();
  });

  it("never advertises controls for an unowned facade task", () => {
    expect(taskCapabilities(task(undefined))).toEqual({
      canCancel: false,
      canRetry: false,
      canPause: false,
      canResume: false,
    });
  });

  it("delegates transfer cancellation to the transfer owner", async () => {
    owners.transfer.phasesByHost.ps5 = { kind: "running" };
    const current = task({ owner: "transfer", host: "ps5" });
    expect(taskCapabilities(current).canCancel).toBe(true);
    expect(await commandTask(current, "cancel")).toBe(true);
    expect(owners.transfer.cancel).toHaveBeenCalledWith("ps5");
  });

  it("advertises and runs retry only for a failed queue row", async () => {
    owners.queue.items = [{ id: "row", status: "failed" }];
    const current = task(
      { owner: "upload-queue", host: "ps5", itemId: "row" },
      "failed",
    );
    expect(taskCapabilities(current)).toMatchObject({
      canCancel: false,
      canRetry: true,
    });
    expect(await commandTask(current, "retry")).toBe(true);
    expect(owners.queue.retryItem).toHaveBeenCalledWith("row");
    expect(owners.queue.startHost).toHaveBeenCalledWith("ps5", { onlyIds: ["row"] });
  });

  it("does not start a queue when retry was rejected by its owner", async () => {
    owners.queue.retryItem.mockReturnValue(false);
    const current = task({
      owner: "upload-queue",
      host: "ps5",
      itemId: "missing",
    });
    expect(await commandTask(current, "retry")).toBe(false);
    expect(owners.queue.startHost).not.toHaveBeenCalled();
  });

  it("cancels a running NAS copy through the engine's job cancel", async () => {
    invoke.mockClear();
    const current = task({ owner: "remote-fetch", jobId: "j-9" });
    expect(taskCapabilities(current).canCancel).toBe(true);
    expect(await commandTask(current, "cancel")).toBe(true);
    expect(invoke).toHaveBeenCalledWith("job_cancel", { jobId: "j-9" });
    expect(taskCapabilities(task({ owner: "remote-fetch", jobId: "j-9" }, "done")).canCancel).toBe(false);
  });
});
