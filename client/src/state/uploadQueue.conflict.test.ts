import { beforeEach, describe, expect, it, vi } from "vitest";

// A row that failed because its destination is taken: Replace and Upload somewhere else re-run it.
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
  };
});
vi.mock("../lib/uploadLeftovers", () => ({
  discardUploadLeftovers: vi.fn(async () => {}),
}));
vi.mock("../lib/destinationConflict", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/destinationConflict")>();
  return { ...actual, clearForReplace: vi.fn(async () => ({ placed: false })) };
});

import { clearForReplace } from "../lib/destinationConflict";
import { discardUploadLeftovers } from "../lib/uploadLeftovers";
import { useUploadQueueStore, type QueueItem } from "./uploadQueue";

const clear = vi.mocked(clearForReplace);
const discard = vi.mocked(discardUploadLeftovers);
const HOST = "192.168.1.10";
const OLD_TX = "a".repeat(32);

function row(over: Partial<QueueItem> = {}): QueueItem {
  return {
    id: "r",
    sourceKind: "game-folder",
    sourcePath: "/src/Game",
    displayName: "Game",
    resolvedDest: "/data/homebrew/Game",
    addr: `${HOST}:9113`,
    txIdHex: OLD_TX,
    strategy: "resume",
    status: "failed",
    error: "the console refused to commit the transfer",
    errorReason: "ava1_commit_exists",
    errorDetail: null,
    startedAt: 1,
    ...over,
  } as QueueItem;
}

const startHost = vi.fn(async () => {});
const item = () => useUploadQueueStore.getState().items[0];

beforeEach(() => {
  clear.mockReset();
  clear.mockResolvedValue({ placed: false });
  discard.mockClear();
  startHost.mockClear();
  useUploadQueueStore.setState({ items: [], runningHosts: {}, running: false, startHost });
});

describe("Replace", () => {
  it("re-runs the row as a new job that overwrites, on its own", async () => {
    useUploadQueueStore.setState({ items: [row()] });
    await useUploadQueueStore.getState().replaceDestination("r");
    expect(clear).toHaveBeenCalledOnce();
    expect(item()).toMatchObject({ status: "pending", error: null, errorReason: null, strategy: "overwrite" });
    expect(item().txIdHex).not.toBe(OLD_TX);
    expect(startHost).toHaveBeenCalledWith(HOST, { onlyIds: ["r"] });
  });

  it("a finished folder moved into place re-runs as a Resume, which sends nothing again", async () => {
    clear.mockResolvedValue({ placed: true });
    useUploadQueueStore.setState({ items: [row({ strategy: "overwrite" })] });
    await useUploadQueueStore.getState().replaceDestination("r");
    expect(item()).toMatchObject({ status: "pending", strategy: "resume" });
  });

  it("a delete that fails leaves the row failed and says why", async () => {
    clear.mockRejectedValue(new Error("fs_delete_failed"));
    useUploadQueueStore.setState({ items: [row()] });
    await expect(useUploadQueueStore.getState().replaceDestination("r")).rejects.toThrow("fs_delete_failed");
    expect(item()).toMatchObject({ status: "failed", errorReason: "ava1_commit_exists", txIdHex: OLD_TX });
    expect(startHost).not.toHaveBeenCalled();
  });

  it("does nothing for a row that did not fail on a taken destination", async () => {
    useUploadQueueStore.setState({ items: [row({ errorReason: "ava1_no_space" })] });
    await useUploadQueueStore.getState().replaceDestination("r");
    expect(clear).not.toHaveBeenCalled();
    expect(item().status).toBe("failed");
  });
});

describe("Upload somewhere else", () => {
  it("sends the row into the picked folder under its name, and clears the old leftovers", () => {
    useUploadQueueStore.setState({ items: [row()] });
    expect(useUploadQueueStore.getState().uploadElsewhere("r", "/mnt/ext1/homebrew")).toBe(true);
    expect(item()).toMatchObject({ status: "pending", resolvedDest: "/mnt/ext1/homebrew/Game" });
    expect(item().txIdHex).not.toBe(OLD_TX);
    expect(discard).toHaveBeenCalledOnce();
    expect(discard.mock.calls[0][0]).toMatchObject({ resolvedDest: "/data/homebrew/Game", status: "failed" });
    expect(startHost).toHaveBeenCalledWith(HOST, { onlyIds: ["r"] });
  });

  it("refuses the folder it is already in", () => {
    useUploadQueueStore.setState({ items: [row()] });
    expect(useUploadQueueStore.getState().uploadElsewhere("r", "/data/homebrew")).toBe(false);
    expect(item().status).toBe("failed");
    expect(startHost).not.toHaveBeenCalled();
  });
});
