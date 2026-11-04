import { afterEach, describe, expect, it } from "vitest";

import { transferBytesMoved, transferScreenBusy } from "./ps5Transfers";
import { IDLE_FS_UPLOAD, useFsUploadStore, type FsUploadActive } from "../state/fsUpload";
import {
  IDLE_BULK,
  IDLE_DOWNLOAD,
  useFsBulkOpStore,
  useFsDownloadOpStore,
} from "../state/fsBulkOp";
import { useUploadQueueStore } from "../state/uploadQueue";
import { useTransferStore } from "../state/transfer";
import type { QueueItem } from "../state/uploadQueue";

// transferScreenBusy only reads `addr` + `status`, so a minimal stub is
// enough — cast through unknown to avoid restating the whole QueueItem shape.
function runningItem(addr: string): QueueItem {
  return { id: `id-${addr}`, addr, status: "running" } as unknown as QueueItem;
}

describe("transferScreenBusy", () => {
  afterEach(() => {
    useUploadQueueStore.setState({ running: false, items: [] });
    useTransferStore.setState({ phasesByHost: {} });
  });

  it("is false when nothing is transferring", () => {
    expect(transferScreenBusy()).toBe(false);
  });

  it("is true while the upload queue is running (global, no host)", () => {
    useUploadQueueStore.setState({ running: true });
    expect(transferScreenBusy()).toBe(true);
  });

  it("is true during a one-shot transfer (starting/running phase)", () => {
    useTransferStore.setState({
      phasesByHost: { "192.168.1.2": { kind: "starting" } },
    });
    expect(transferScreenBusy()).toBe(true);
  });

  // Multi-console: a busy console must NOT report another console busy.
  it("scopes queue activity to the asked-for host", () => {
    useUploadQueueStore.setState({
      running: true,
      items: [runningItem("192.168.1.2:9113")],
    });
    // Console A is uploading…
    expect(transferScreenBusy("192.168.1.2")).toBe(true);
    // …but console B is idle and must be free to install in parallel.
    expect(transferScreenBusy("192.168.1.9")).toBe(false);
  });

  it("scopes the one-shot transfer to its target host", () => {
    // Console A has a one-shot running; console B has none.
    useTransferStore.setState({
      phasesByHost: { "192.168.1.2": { kind: "running" } as never },
    });
    expect(transferScreenBusy("192.168.1.2")).toBe(true);
    expect(transferScreenBusy("192.168.1.9")).toBe(false);
  });

  it("two consoles each run a one-shot fully in parallel (no cross-block)", () => {
    useTransferStore.setState({
      phasesByHost: {
        "192.168.1.2": { kind: "running" } as never,
        "192.168.1.9": { kind: "running" } as never,
      },
    });
    expect(transferScreenBusy("192.168.1.2")).toBe(true);
    expect(transferScreenBusy("192.168.1.9")).toBe(true);
  });

  it("ignores a one-shot transfer whose phase is terminal", () => {
    useTransferStore.setState({
      phasesByHost: { "192.168.1.2": { kind: "idle" } },
    });
    expect(transferScreenBusy("192.168.1.2")).toBe(false);
  });
});

describe("work on the Files screen", () => {
  const HOST = "192.168.1.2";
  afterEach(() => {
    useFsUploadStore.setState({ byHost: {} });
    useFsDownloadOpStore.setState({ byHost: {} });
    useFsBulkOpStore.setState({ byHost: {} });
    useUploadQueueStore.setState({ running: false, items: [] });
  });

  it("counts a Files upload as busy, on that console only", () => {
    useFsUploadStore.setState({
      byHost: { [HOST]: { ...IDLE_FS_UPLOAD, active: { sent: 5 } as FsUploadActive } },
    });
    expect(transferScreenBusy(HOST)).toBe(true);
    expect(transferScreenBusy()).toBe(true);
    expect(transferScreenBusy("192.168.1.3")).toBe(false);
  });

  it("counts a Files download as busy", () => {
    useFsDownloadOpStore.setState({ byHost: { [HOST]: { ...IDLE_DOWNLOAD, active: true } } });
    expect(transferScreenBusy(HOST)).toBe(true);
  });

  it("counts a copy on the console as busy", () => {
    useFsBulkOpStore.setState({ byHost: { [HOST]: { ...IDLE_BULK, op: "paste-copy" } } });
    expect(transferScreenBusy(HOST)).toBe(true);
  });

  // The proof-of-life shortcut read a store nothing writes to any more.
  it("reads progress from the running queue row, and from Files", () => {
    expect(transferBytesMoved(HOST)).toBeNull();
    useUploadQueueStore.setState({
      items: [{ id: "a", addr: HOST, status: "running", bytesSent: 100 } as unknown as QueueItem],
    });
    expect(transferBytesMoved(HOST)).toBe(100);
    useFsUploadStore.setState({
      byHost: { [HOST]: { ...IDLE_FS_UPLOAD, active: { sent: 5 } as FsUploadActive } },
    });
    expect(transferBytesMoved(HOST)).toBe(105);
    expect(transferBytesMoved("192.168.1.3")).toBeNull();
  });
});
