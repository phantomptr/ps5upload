import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    startTransferFile: vi.fn(async () => "file-job"),
    startTransferDir: vi.fn(async () => "dir-job"),
    jobStatus: vi.fn(async () => ({
      status: "running",
      bytes_sent: 5,
      total_bytes: 100,
      files_count: 2,
    })),
    jobCancel: vi.fn(async () => {}),
    resumeTxidLookup: vi.fn(async () => null),
    resumeTxidRemember: vi.fn(async () => {}),
    resumeTxidForget: vi.fn(async () => {}),
    toastPush: vi.fn(async () => {}),
  };
});
vi.mock("../api/ava1", () => ({
  pairingStatus: vi.fn(),
  pairingConfirm: vi.fn(),
}));
vi.mock("../lib/jobFiles", () => ({
  fetchJobFiles: vi.fn(async () => [
    { rel_path: "a.bin", size: 5 },
    { rel_path: "b.bin", size: 95 },
  ]),
}));

import { fetchJobFiles } from "../lib/jobFiles";
import { phaseForHost, useTransferStore } from "./transfer";

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(fetchJobFiles).mockClear();
  useTransferStore.setState({ phasesByHost: {} });
});

describe("one-shot transfer file list", () => {
  it("reads the list once from its own route and counts files from the snapshot", async () => {
    await useTransferStore.getState().start({
      sourceKind: "folder",
      srcPath: "/a",
      dest: "/b",
      addr: "10.0.0.2",
    });
    // Several polls (500 ms apart).
    await vi.advanceTimersByTimeAsync(3000);
    const p = phaseForHost(useTransferStore.getState(), "10.0.0.2");
    expect(p.kind).toBe("running");
    if (p.kind === "running") {
      expect(p.fileCount).toBe(2);
      expect(p.files.map((f) => f.rel_path)).toEqual(["a.bin", "b.bin"]);
      expect(p.filesCompleted).toBe(1);
    }
    expect(fetchJobFiles).toHaveBeenCalledTimes(1);
    expect(fetchJobFiles).toHaveBeenCalledWith("dir-job");
    useTransferStore.getState().reset("10.0.0.2");
  });
});
