import { afterEach, describe, expect, it } from "vitest";

import { transferScreenBusy } from "./ps5Transfers";
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
