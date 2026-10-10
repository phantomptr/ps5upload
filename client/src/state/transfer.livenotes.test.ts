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
      bytes_sent: 10,
      total_bytes: 100,
      phase: "skipping",
      skip_done_bytes: 1,
      skip_total_bytes: 9,
      bottleneck: "network",
      settling: true,
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

import { phaseForHost, useTransferStore } from "./transfer";

beforeEach(() => {
  vi.useFakeTimers();
  useTransferStore.setState({ phasesByHost: {} });
});

describe("one-shot transfer live notes", () => {
  it("carries the skipping, bottleneck and settling notes of a running job, and only then", async () => {
    await useTransferStore.getState().start({
      sourceKind: "file",
      srcPath: "/a",
      dest: "/b",
      addr: "10.0.0.2",
    });
    await vi.advanceTimersByTimeAsync(800);
    const p = phaseForHost(useTransferStore.getState(), "10.0.0.2");
    expect(p.kind).toBe("running");
    if (p.kind === "running") {
      expect(p.live).toEqual({
        skipping: true,
        skipDoneBytes: 1,
        skipTotalBytes: 9,
        bottleneck: "network",
        settling: true,
      });
    }
    useTransferStore.getState().reset("10.0.0.2");
  });
});
