import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Keep the real api/ps5 exports (error classes, id helpers) and override only
// the network-touching ones so the runner can be driven without an engine.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    startTransferFile: vi.fn(async () => "job"),
    startTransferDir: vi.fn(async () => "job"),
    startTransferDirReconcile: vi.fn(async () => "job"),
    startTransferZip: vi.fn(async () => "job"),
    startTransfer7z: vi.fn(async () => "job"),
    startTransferRar: vi.fn(async () => "job"),
    jobStatus: vi.fn(async () => ({ status: "running" })),
    jobCancel: vi.fn(async () => {}),
    resumeTxidLookup: vi.fn(async () => null),
    resumeTxidRemember: vi.fn(async () => {}),
    resumeTxidForget: vi.fn(async () => {}),
    notifSend: vi.fn(async () => {}),
  };
});

import { jobCancel, startTransferRar } from "../api/ps5";
import { useTransferStore } from "./transfer";

const mockedCancel = vi.mocked(jobCancel);
const mockedStartRar = vi.mocked(startTransferRar);

const ADDR = "192.168.1.10:9113";

describe("one-shot upload — cancel while the start request is in flight", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useTransferStore.setState({ phasesByHost: {} });
    mockedCancel.mockClear();
    mockedStartRar.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("cancels the late-arriving job instead of orphaning it", async () => {
    // The engine's .rar route plans the entire archive inside the request
    // handler before it mints a job id, so POST /api/transfer/rar can sit
    // unanswered for seconds on a large archive. A user who hits Cancel in
    // that window has no job id to cancel — and before the fix the id that
    // arrived afterwards was simply dropped, leaving the transfer running
    // with nothing able to stop it. The reporter had to kill the app and
    // delete the partial file from the console by hand (5.4.7).
    let release!: (jobId: string) => void;
    mockedStartRar.mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          release = resolve;
        }),
    );

    void useTransferStore.getState().start({
      sourceKind: "archive",
      srcPath: "/src/game.rar",
      dest: "/data/game",
      addr: ADDR,
    });
    await vi.advanceTimersByTimeAsync(10);

    // Cancel lands mid-flight: genuinely nothing to cancel yet.
    useTransferStore.getState().cancel(ADDR);
    await vi.advanceTimersByTimeAsync(10);
    expect(mockedCancel).not.toHaveBeenCalled();

    // The engine answers. That job is live on the wire right now.
    release("late-job");
    await vi.advanceTimersByTimeAsync(10);

    expect(mockedCancel).toHaveBeenCalledWith("late-job");
  });

  it("does not cancel when no one asked it to", async () => {
    // Guard against over-firing: a normal start must leave the job alone.
    void useTransferStore.getState().start({
      sourceKind: "archive",
      srcPath: "/src/game.rar",
      dest: "/data/game",
      addr: ADDR,
    });
    await vi.advanceTimersByTimeAsync(10);
    expect(mockedCancel).not.toHaveBeenCalled();
  });
});
