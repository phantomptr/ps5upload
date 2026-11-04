import { beforeEach, describe, expect, it, vi } from "vitest";

// A row that is cancelled, removed or cleared takes its console-side partial with it (R2-1 F10).
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/materialize", () => ({
  releaseCopy: vi.fn(async () => {}),
  materializeRemote: vi.fn(async (p: string) => p),
}));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    uploadQueueLoad: vi.fn(async () => ({ items: [], continueOnFailure: false })),
    uploadQueueSave: vi.fn(async () => {}),
    jobStatus: vi.fn(async () => ({ status: "failed" })),
    jobCancel: vi.fn(async () => {}),
  };
});
vi.mock("../lib/uploadLeftovers", () => ({
  discardUploadLeftovers: vi.fn(async () => {}),
}));

import { discardUploadLeftovers } from "../lib/uploadLeftovers";
import { useUploadQueueStore, type QueueItem } from "./uploadQueue";

const discard = vi.mocked(discardUploadLeftovers);
const HOST = "192.168.1.10";

function row(id: string, over: Partial<QueueItem> = {}): QueueItem {
  return {
    id,
    sourceKind: "pkg",
    sourcePath: `/src/${id}.pkg`,
    displayName: id,
    resolvedDest: `/data/pkg/${id}.pkg`,
    addr: `${HOST}:9113`,
    txIdHex: "a".repeat(32),
    status: "failed",
    startedAt: 1,
    ...over,
  } as QueueItem;
}
const discarded = () => discard.mock.calls.map((c) => c[0].id);

beforeEach(() => {
  discard.mockClear();
  useUploadQueueStore.setState({ items: [], runningHosts: {}, running: false });
});

describe("what a row leaves on the console goes with it", () => {
  it("on Remove of a row that is not running", () => {
    useUploadQueueStore.setState({ items: [row("a")] });
    useUploadQueueStore.getState().remove("a");
    expect(discarded()).toEqual(["a"]);
  });

  it("not on Remove of a running row: nothing stopped its job", () => {
    useUploadQueueStore.setState({ items: [row("a", { status: "running" })] });
    useUploadQueueStore.getState().remove("a");
    expect(discarded()).toEqual([]);
  });

  it("on Cancel of an idle row and of a running one, handed over with its job", () => {
    useUploadQueueStore.setState({
      items: [row("a"), row("b", { status: "running", jobId: "j" })],
      runningHosts: { [HOST]: true } as never,
    });
    useUploadQueueStore.getState().cancelItem("a");
    useUploadQueueStore.getState().cancelItem("b");
    expect(discarded()).toEqual(["a", "b"]);
    const running = discard.mock.calls[1][0];
    expect(running).toMatchObject({ status: "running", jobId: "j" });
    // Later rows are read when the sweep runs, not when the row went.
    expect(discard.mock.calls[1][1]()).toEqual([]);
  });

  it("on Clear, except a running install, which stays", () => {
    useUploadQueueStore.setState({
      items: [row("a"), row("i", { sourceKind: "install", status: "running" })],
    });
    useUploadQueueStore.getState().clear(HOST);
    expect(discarded()).toEqual(["a"]);
  });
});
