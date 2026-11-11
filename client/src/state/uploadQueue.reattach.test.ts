// R15 (#372): the self-hosted web UI's engine outlives the browser tab, so a reopened tab
// must pick the running job back up instead of forgetting it (or starting it again).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/materialize", () => ({
  releaseCopy: vi.fn(async () => {}),
  materializeRemote: vi.fn(async (p: string) => p),
}));
vi.mock("../lib/ensurePayloadCurrent", () => ({
  ensurePayloadCurrent: vi.fn(async () => {}),
}));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    uploadQueueLoad: vi.fn(async () => ({ items: [], continueOnFailure: false })),
    uploadQueueSave: vi.fn(async () => {}),
    startTransferFile: vi.fn(async () => "fresh-job"),
    jobStatus: vi.fn(async () => ({ status: "running" })),
    jobCancel: vi.fn(async () => {}),
    fsMkdir: vi.fn(async () => {}),
    fsDelete: vi.fn(async () => {}),
    fsListDir: vi.fn(async () => ({ entries: [], truncated: false })),
  };
});

const { resolveJob, recordInstalled } = vi.hoisted(() => ({
  resolveJob: vi.fn(),
  recordInstalled: vi.fn(),
}));
vi.mock("./consoleQueueBridge", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./consoleQueueBridge")>();
  return { ...actual, resolveInstallJob: resolveJob };
});
vi.mock("./pkgLibrary", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./pkgLibrary")>();
  return { ...actual, recordPkgInstalled: recordInstalled };
});

import { fsDelete, jobCancel, jobStatus, startTransferFile, uploadQueueLoad } from "../api/ps5";
import { useUploadQueueStore } from "./uploadQueue";

const base = {
  addr: "10.0.0.2:9113",
  strategy: "overwrite",
  reconcileMode: "fast",
  excludes: [],
  mountAfterUpload: false,
  mountReadOnly: false,
  registerAfterUpload: false,
  txIdHex: "00",
  bytesSent: 0,
  totalBytes: 0,
  bytesPerSec: 0,
  filesFinalized: 0,
  filesFinalizingTotal: 0,
  mountedAt: null,
  registeredAs: null,
  mountWarnings: [],
  error: null,
  errorReason: null,
  errorDetail: null,
  addedAt: 1,
  startedAt: 1,
  completedAt: null,
  sourceKind: "file",
  sourcePath: "/src/big.bin",
  displayName: "big.bin",
  resolvedDest: "/data/big.bin",
};

const row = (id: string) => useUploadQueueStore.getState().items.find((i) => i.id === id);
const until = async (cond: () => boolean) => {
  for (let i = 0; i < 800 && !cond(); i++) await new Promise((r) => setTimeout(r, 5));
  expect(cond()).toBe(true);
};

describe("the web UI re-attaches to running jobs after a reload", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("window", {}); // a browser: no Tauri
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      loaded: false,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.unstubAllGlobals();
  });

  it("follows a package row's install instead of sending or installing it again", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      items: [
        {
          ...base,
          id: "p",
          sourceKind: "pkg",
          status: "running",
          jobId: "upload-job",
          uploadDone: true,
          installPhase: "installing",
          installJobId: "install-job",
          deletePkgAfterInstall: false,
        },
      ],
      continueOnFailure: false,
    } as never);
    resolveJob.mockResolvedValueOnce({ state: "running", pct: 40 });
    resolveJob.mockResolvedValue({ state: "finished", result: { ok: true } });
    await useUploadQueueStore.getState().hydrate();
    await until(() => row("p")?.status === "done");
    expect(resolveJob).toHaveBeenCalledWith("install-job");
    expect(startTransferFile).not.toHaveBeenCalled();
    expect(jobStatus).not.toHaveBeenCalled();
    expect(row("p")?.installPhase).toBe("done");
    expect(recordInstalled).toHaveBeenCalledWith("10.0.0.2", "/data/big.bin");
    expect(fsDelete).not.toHaveBeenCalled();
  });

  it("removes the staged copy of a found install when the row asked for it", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      items: [
        {
          ...base,
          id: "p",
          sourceKind: "pkg",
          status: "running",
          uploadDone: true,
          installPhase: "installing",
          installJobId: "install-job",
        },
      ],
      continueOnFailure: false,
    } as never);
    resolveJob.mockResolvedValue({ state: "finished", result: { ok: true } });
    await useUploadQueueStore.getState().hydrate();
    await until(() => row("p")?.status === "done");
    expect(fsDelete).toHaveBeenCalledWith(expect.anything(), "/data/big.bin");
  });

  it("an install found unconfirmed keeps its staged copy and is not recorded as installed", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      items: [
        {
          ...base,
          id: "p",
          sourceKind: "pkg",
          status: "running",
          uploadDone: true,
          installPhase: "installing",
          installJobId: "install-job",
        },
      ],
      continueOnFailure: false,
    } as never);
    resolveJob.mockResolvedValue({
      state: "finished",
      result: { ok: true, unverified: true, message: "Check Downloads on the PS5. The package was kept." },
    });
    await useUploadQueueStore.getState().hydrate();
    await until(() => row("p")?.status === "done");
    expect(fsDelete).not.toHaveBeenCalled();
    expect(recordInstalled).not.toHaveBeenCalled();
    expect(row("p")?.installPhase).toBe("unverified");
    expect(row("p")?.installNote).toBe("Check Downloads on the PS5. The package was kept.");
  });

  it("marks a package row interrupted when its install left no job to follow", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      items: [
        {
          ...base,
          id: "p",
          sourceKind: "pkg",
          status: "running",
          jobId: "upload-job",
          uploadDone: true,
          installPhase: "installing",
        },
      ],
      continueOnFailure: false,
    } as never);
    await useUploadQueueStore.getState().hydrate();
    expect(row("p")?.status).toBe("failed");
    expect(row("p")?.installPhase).toBe("error");
    expect(startTransferFile).not.toHaveBeenCalled();
  });

  it("a failed status read does not call a live install lost", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      items: [
        {
          ...base,
          id: "i",
          sourceKind: "install",
          status: "running",
          install: { via: "console-path", path: "/data/a.pkg" },
          installJobId: "install-job",
        },
      ],
      continueOnFailure: false,
    } as never);
    resolveJob.mockResolvedValueOnce({ state: "unanswered" });
    resolveJob.mockResolvedValue({ state: "finished", result: { ok: true } });
    await useUploadQueueStore.getState().hydrate();
    await until(() => row("i")?.status === "done");
    expect(row("i")?.error).toBeNull();
    // A file the user pointed at is theirs: recorded, never deleted.
    expect(recordInstalled).toHaveBeenCalledWith("10.0.0.2", "/data/a.pkg");
    expect(fsDelete).not.toHaveBeenCalled();
  });

  it("adopts a job the engine is still running, without starting the upload again or cancelling it", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [{ ...base, id: "a", status: "running", jobId: "J1" }],
    } as never);
    let polls = 0;
    vi.mocked(jobStatus).mockImplementation((async (id: string) => {
      if (id !== "J1") throw new Error("unknown job");
      polls += 1;
      // The hydrate probe, then a running poll, then done.
      return polls < 3
        ? { status: "running", bytes_sent: 5, total_bytes: 10 }
        : { status: "done", bytes_sent: 10, dest: "/data/big.bin" };
    }) as never);

    await useUploadQueueStore.getState().hydrate();
    await until(() => row("a")?.status === "done");

    expect(startTransferFile).not.toHaveBeenCalled();
    expect(jobCancel).not.toHaveBeenCalled();
    expect(row("a")?.jobId).toBe("J1");
    expect(row("a")?.attachJobId).toBeUndefined();
  });

  it("leaves an item pending when the engine no longer knows its job", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [{ ...base, id: "b", status: "running", jobId: "GONE" }],
    } as never);
    vi.mocked(jobStatus).mockRejectedValue(new Error("404"));
    await useUploadQueueStore.getState().hydrate();
    await until(() => vi.mocked(jobStatus).mock.calls.some((c) => c[0] === "GONE"));
    await new Promise((r) => setTimeout(r, 0)); // its refusal handled
    expect(row("b")?.status).toBe("pending");
    expect(row("b")?.attachJobId).toBeUndefined();
    expect(useUploadQueueStore.getState().runningHosts["10.0.0.2"]).toBeFalsy();
  });

  it("Start all during the lookup waits for it instead of spinning (and hanging the tab)", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: true,
      items: [{ ...base, id: "d", status: "running", jobId: "GONE" }],
    } as never);
    let answer: (e: Error) => void = () => {};
    vi.mocked(jobStatus).mockImplementationOnce(
      () => new Promise((_, reject) => (answer = reject)) as never,
    );
    vi.mocked(jobStatus).mockResolvedValue({ status: "done", bytes_sent: 1, dest: "/data/big.bin" } as never);
    await useUploadQueueStore.getState().hydrate();
    expect(row("d")?.status).toBe("pending");

    const started = useUploadQueueStore.getState().start();
    // A macrotask still runs: this answer could never arrive while the loop spun.
    await new Promise((r) => setTimeout(r, 5));
    answer(new Error("job not found"));
    await started;
    expect(startTransferFile).toHaveBeenCalled();
    expect(row("d")?.status).not.toBe("pending");
  });

  it("never carries an adopt marker across a reload", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [{ ...base, id: "c", status: "pending", attachJobId: "STALE" }],
    } as never);
    await useUploadQueueStore.getState().hydrate();
    expect(row("c")?.attachJobId).toBeUndefined();
  });
});
