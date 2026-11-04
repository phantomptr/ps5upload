import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Only the network-touching api/ps5 calls are replaced, so the one-shot
// runner can be driven through a flaky engine without one.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    startTransferFile: vi.fn(async () => "job-1"),
    jobStatus: vi.fn(async () => ({ status: "running" })),
    jobCancel: vi.fn(async () => {}),
    resumeTxidLookup: vi.fn(async () => null),
    resumeTxidRemember: vi.fn(async () => {}),
    resumeTxidForget: vi.fn(async () => {}),
    toastPush: vi.fn(async () => {}),
    fsMount: vi.fn(),
    fsUnmount: vi.fn(async () => {}),
    smpStatus: vi.fn(async () => ({ running: false })),
  };
});
vi.mock("../lib/fileResumeTx", () => ({ fileResumeTxId: vi.fn(async () => "tx") }));

import { fsMount, jobStatus, startTransferFile } from "../api/ps5";
import { LOST_POLL_INTERVAL_MS, MAX_POLL_FAILURES, useTransferStore } from "./transfer";

const status = vi.mocked(jobStatus);
const startFile = vi.mocked(startTransferFile);
const ADDR = "10.0.0.5";
const phase = () => useTransferStore.getState().phasesByHost[ADDR];

async function startUpload() {
  await useTransferStore.getState().start({
    sourceKind: "file",
    srcPath: "/src/game.pkg",
    dest: "/data/game.pkg",
    addr: ADDR,
  });
  // First poll fires after the initial delay.
  await vi.advanceTimersByTimeAsync(250);
}

describe("one-shot upload — status polls that fail", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTransferStore.setState({ phasesByHost: {} });
    status.mockReset();
    startFile.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("one failed poll does not fail an upload the engine is still sending", async () => {
    status
      .mockRejectedValueOnce(new Error("request timed out"))
      .mockResolvedValue({ status: "running", bytes_sent: 10, total_bytes: 100 } as never);
    await startUpload();
    expect(phase()).toMatchObject({ kind: "running" });
    await vi.advanceTimersByTimeAsync(600);
    expect(phase()).toMatchObject({ kind: "running", bytesSent: 10 });
    expect(phase()).not.toHaveProperty("lostContact");
  });

  it("after several failures shows lost contact, keeps the job, and re-attaches to the same job", async () => {
    status.mockRejectedValue(new Error("connection refused"));
    await startUpload();
    for (let i = 0; i < MAX_POLL_FAILURES; i++) await vi.advanceTimersByTimeAsync(600);
    const p = phase();
    expect(p).toMatchObject({ kind: "running", jobId: "job-1" });
    expect(p.kind === "running" && p.lostContact?.error).toBe("connection refused");

    // Re-attach polls the SAME job again; it is still running, so contact is back.
    status.mockResolvedValue({ status: "running", bytes_sent: 50, total_bytes: 100 } as never);
    useTransferStore.getState().reattach(ADDR);
    await vi.advanceTimersByTimeAsync(10);
    expect(status).toHaveBeenLastCalledWith("job-1", ADDR);
    expect(phase()).toMatchObject({ kind: "running", bytesSent: 50 });
    expect(phase()).not.toHaveProperty("lostContact");
    // Nothing started a competing upload.
    expect(startFile).toHaveBeenCalledTimes(1);
  });

  it("Check again while a status read is still out does not start a second poll loop", async () => {
    status.mockRejectedValue(new Error("connection refused"));
    await startUpload();
    for (let i = 0; i < MAX_POLL_FAILURES; i++) await vi.advanceTimersByTimeAsync(600);
    // The next read hangs; Check again is pressed twice meanwhile.
    let release: (v: unknown) => void = () => {};
    status.mockImplementationOnce(() => new Promise((r) => (release = r)) as never);
    useTransferStore.getState().reattach(ADDR);
    await vi.advanceTimersByTimeAsync(10);
    const callsBefore = status.mock.calls.length;
    useTransferStore.getState().reattach(ADDR);
    useTransferStore.getState().reattach(ADDR);
    await vi.advanceTimersByTimeAsync(10);
    expect(status.mock.calls.length).toBe(callsBefore);
    status.mockResolvedValue({ status: "running", bytes_sent: 1, total_bytes: 100 } as never);
    release({ status: "running", bytes_sent: 1, total_bytes: 100 });
    await vi.advanceTimersByTimeAsync(10);
    // One loop: one read per 500 ms interval from here on.
    const at = status.mock.calls.length;
    await vi.advanceTimersByTimeAsync(2000);
    expect(status.mock.calls.length - at).toBeLessThanOrEqual(4);
    useTransferStore.getState().reset(ADDR);
  });

  it("keeps polling slowly while out of contact and picks the job back up", async () => {
    status.mockRejectedValue(new Error("connection refused"));
    await startUpload();
    for (let i = 0; i < MAX_POLL_FAILURES; i++) await vi.advanceTimersByTimeAsync(600);
    status.mockResolvedValue({ status: "done", bytes_sent: 100, elapsed_ms: 1000 } as never);
    await vi.advanceTimersByTimeAsync(LOST_POLL_INTERVAL_MS + 10);
    expect(phase()).toMatchObject({ kind: "done", jobId: "job-1" });
  });

  it("fails right away when the engine says the job does not exist", async () => {
    status.mockRejectedValue(new Error("job not found"));
    await startUpload();
    expect(phase()).toMatchObject({ kind: "failed", jobId: "job-1" });
  });
});

describe("one-shot upload — Retry", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTransferStore.setState({ phasesByHost: {} });
    status.mockReset();
    startFile.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("starts the upload that failed again, with its own arguments", async () => {
    status.mockResolvedValue({ status: "failed", error: "disk full" } as never);
    await startUpload();
    expect(phase()).toMatchObject({ kind: "failed", error: "disk full" });
    status.mockResolvedValue({ status: "running", bytes_sent: 1, total_bytes: 10 } as never);
    expect(useTransferStore.getState().retry(ADDR)).toBe(true);
    await vi.advanceTimersByTimeAsync(250);
    expect(startFile).toHaveBeenCalledTimes(2);
    expect(startFile.mock.calls[1].slice(0, 3)).toEqual(["/src/game.pkg", "/data/game.pkg", ADDR]);
    useTransferStore.getState().reset(ADDR);
  });

  it("after a failed mount, Retry repeats only the mount", async () => {
    const mount = vi.mocked(fsMount);
    mount.mockRejectedValueOnce(new Error("mount busy"));
    status.mockResolvedValue({ status: "done", dest: "/data/g.ffpkg", bytes_sent: 9, elapsed_ms: 5, files_sent: 1 } as never);
    await useTransferStore.getState().start({
      sourceKind: "image",
      srcPath: "/src/g.ffpkg",
      dest: "/data/g.ffpkg",
      addr: ADDR,
      mountAfterUpload: true,
    });
    await vi.advanceTimersByTimeAsync(250);
    expect(phase()).toMatchObject({ kind: "failed", error: "upload completed, but mount failed: mount busy" });
    startFile.mockClear();

    // It fails again: still the mount that is on offer.
    mount.mockRejectedValueOnce(new Error("still busy"));
    expect(useTransferStore.getState().retry(ADDR)).toBe(true);
    await vi.advanceTimersByTimeAsync(10);
    expect(phase()).toMatchObject({ kind: "failed", error: "upload completed, but mount failed: still busy" });

    mount.mockResolvedValueOnce({ mount_point: "/data/g", kernel_ro: true, layout_valid: false } as never);
    expect(useTransferStore.getState().retry(ADDR)).toBe(true);
    await vi.advanceTimersByTimeAsync(10);
    expect(phase()).toMatchObject({ kind: "done", dest: "/data/g.ffpkg", mountedAt: "/data/g", bytesSent: 9 });
    const done = phase();
    expect(done.kind === "done" && done.mountWarnings).toHaveLength(1);
    expect(mount).toHaveBeenLastCalledWith(ADDR, "/data/g.ffpkg", { mountPoint: "/data/g", readOnly: true });
    expect(startFile).not.toHaveBeenCalled();
    useTransferStore.getState().reset(ADDR);
  });

  it("has nothing to retry when no upload failed", () => {
    expect(useTransferStore.getState().retry(ADDR)).toBe(false);
  });
});
