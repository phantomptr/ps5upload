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

  // Deleting a big game folder takes minutes, and the row stays "failed" meanwhile. Its busy
  // mark was the button's own, gone after a look at another screen: a second Replace deleted the
  // destination again under the upload the first one had started, and Retry failed re-ran the
  // row into the folder being deleted.
  describe("while the copy on the PS5 is being deleted", () => {
    const slowDelete = () => {
      let done!: (v: { placed: boolean }) => void;
      clear.mockReturnValue(new Promise((r) => (done = r)));
      return () => done({ placed: false });
    };

    it("a second Replace does nothing", async () => {
      const finish = slowDelete();
      useUploadQueueStore.setState({ items: [row()] });
      const first = useUploadQueueStore.getState().replaceDestination("r");
      const second = useUploadQueueStore.getState().replaceDestination("r");
      finish();
      await Promise.all([first, second]);
      expect(clear).toHaveBeenCalledOnce();
      expect(startHost).toHaveBeenCalledOnce();
    });

    it("Retry failed and Upload somewhere else leave the row to it", async () => {
      const finish = slowDelete();
      useUploadQueueStore.setState({ items: [row()] });
      const replacing = useUploadQueueStore.getState().replaceDestination("r");
      useUploadQueueStore.getState().retryFailed(HOST);
      expect(item()).toMatchObject({ status: "failed", txIdHex: OLD_TX });
      expect(useUploadQueueStore.getState().uploadElsewhere("r", "/mnt/ext1/homebrew")).toBe(false);
      expect(item().resolvedDest).toBe("/data/homebrew/Game");
      finish();
      await replacing;
      expect(item()).toMatchObject({ status: "pending", strategy: "overwrite" });
      expect(startHost).toHaveBeenCalledWith(HOST, { onlyIds: ["r"] });
      // And once it is over, the row is like any other again.
      expect(clear).toHaveBeenCalledOnce();
    });

    it("a row that is no longer the failed one when the delete ends is left as it is", async () => {
      const finish = slowDelete();
      useUploadQueueStore.setState({ items: [row()] });
      const replacing = useUploadQueueStore.getState().replaceDestination("r");
      // Started again some other way meanwhile (another device's change synced in, a restart).
      useUploadQueueStore.setState({ items: [row({ status: "running", errorReason: null, error: null })] });
      finish();
      await replacing;
      expect(item()).toMatchObject({ status: "running", txIdHex: OLD_TX });
      expect(startHost).not.toHaveBeenCalled();
    });

    it("a delete that failed can be tried again", async () => {
      clear.mockRejectedValueOnce(new Error("fs_delete_failed"));
      useUploadQueueStore.setState({ items: [row()] });
      await expect(useUploadQueueStore.getState().replaceDestination("r")).rejects.toThrow();
      clear.mockResolvedValue({ placed: false });
      await useUploadQueueStore.getState().replaceDestination("r");
      expect(item().status).toBe("pending");
    });
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
