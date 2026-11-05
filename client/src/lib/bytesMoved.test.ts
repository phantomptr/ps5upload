import { beforeEach, describe, expect, it } from "vitest";

import { useTaskStore } from "../state/tasks";
import { useTransferStore } from "../state/transfer";
import { useUploadQueueStore } from "../state/uploadQueue";
import { bytesMoved } from "./bytesMoved";

const HOST = "192.168.1.10";

describe("bytesMoved", () => {
  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
    useTransferStore.setState({ phasesByHost: {} });
    useUploadQueueStore.setState({ items: [] });
  });

  it("is null when nothing running reports a count", () => {
    expect(bytesMoved(HOST)).toBeNull();
  });

  it("sees a queue upload, which has no one-shot phase", () => {
    useUploadQueueStore.setState({
      items: [
        { status: "running", addr: `${HOST}:9113`, bytesSent: 5 },
        { status: "running", addr: "10.0.0.2:9113", bytesSent: 100 },
        { status: "done", addr: `${HOST}:9113`, bytesSent: 100 },
      ] as never,
    });
    expect(bytesMoved(HOST)).toBe(5);
  });

  it("sees a Files upload, and only one that reports bytes", () => {
    useTaskStore.setState({
      tasks: [
        {
          status: "running",
          consoleId: HOST,
          progress: { unit: "bytes", current: 7, total: 9 },
        },
        {
          status: "running",
          consoleId: HOST,
          progress: { unit: "files", current: 3, total: 9 },
        },
      ] as never,
    });
    expect(bytesMoved(HOST)).toBe(7);
  });
});
