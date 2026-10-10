import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/ps5", () => ({ jobCancel: vi.fn(async () => {}) }));

import { jobCancel } from "../api/ps5";
import { downloadForHost, useFsDownloadOpStore } from "./fsBulkOp";

const mockedCancel = vi.mocked(jobCancel);

describe("download Stop", () => {
  beforeEach(() => {
    mockedCancel.mockClear();
    useFsDownloadOpStore.setState({ byHost: {} });
  });

  it("cancels the engine job and stops the runner", () => {
    const store = useFsDownloadOpStore.getState();
    const runId = store.begin("192.168.1.2", {
      jobId: "job-1",
      rootName: "f.bin",
      rootSrcPath: "/data/f.bin",
      destDir: "/tmp",
    });
    store.requestStop("192.168.1.2");
    expect(mockedCancel).toHaveBeenCalledWith("job-1");
    const slot = downloadForHost(
      useFsDownloadOpStore.getState(),
      "192.168.1.2",
    );
    expect(slot.active).toBe(false);
    expect(slot.runId).not.toBe(runId);
  });

  it("does not call the engine when nothing is downloading", () => {
    useFsDownloadOpStore.getState().requestStop("192.168.1.2");
    expect(mockedCancel).not.toHaveBeenCalled();
  });

  it("only cancels the console it was asked to", () => {
    const store = useFsDownloadOpStore.getState();
    store.begin("192.168.1.2", {
      jobId: "job-a",
      rootName: "a",
      rootSrcPath: "/a",
      destDir: "/tmp",
    });
    store.begin("192.168.1.3", {
      jobId: "job-b",
      rootName: "b",
      rootSrcPath: "/b",
      destDir: "/tmp",
    });
    store.requestStop("192.168.1.3");
    expect(mockedCancel).toHaveBeenCalledTimes(1);
    expect(mockedCancel).toHaveBeenCalledWith("job-b");
    expect(
      downloadForHost(useFsDownloadOpStore.getState(), "192.168.1.2").active,
    ).toBe(true);
  });
});
