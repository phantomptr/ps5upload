import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Keep the real api/ps5 (UploadJobError class, generateTxIdHex, etc.) and
// only override the network-touching functions so we can drive the runner
// deterministically without a PS5 or engine.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/ensurePayloadCurrent", () => ({
  ensurePayloadCurrent: vi.fn(async () => {}),
}));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    uploadQueueLoad: vi.fn(async () => ({ items: [], continueOnFailure: false })),
    uploadQueueSave: vi.fn(async () => {}),
    startTransferFile: vi.fn(async () => "job"),
    startTransferDir: vi.fn(async () => "job"),
    startTransferDirReconcile: vi.fn(async () => "job"),
    startTransferZip: vi.fn(async () => "job"),
    jobStatus: vi.fn(async () => ({ status: "running" })),
    // Must be mocked: the real one falls through to an HTTP fetch
    // whenever the app is not running inside Tauri, which is always
    // true under vitest. Leaving it unmocked fired real cancel
    // requests at whatever engine was listening.
    jobCancel: vi.fn(async () => {}),
    fsMkdir: vi.fn(async () => {}),
    fsDelete: vi.fn(async () => {}),
    fsMount: vi.fn(async () => ({ mount_point: "/mnt/x", layout_valid: true })),
    smpStatus: vi.fn(async () => ({ running: false })),
    smpManualInstall: vi.fn(async () => ({ added: true })),
    powerStandby: vi.fn(async () => ({ ok: true })),
  };
});
vi.mock("./pkgLibrary", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./pkgLibrary")>();
  return {
    ...actual,
    runPkgInstall: vi.fn(async () => ({
      installed: true,
      mayNotLaunch: false,
      errMessage: "",
    })),
  };
});

import {
  jobStatus,
  startTransferFile,
  fsMount,
  smpStatus,
  smpManualInstall,
  powerStandby,
  fsDelete,
  uploadQueueSave,
} from "../api/ps5";
import { useRestAfterUploadStore } from "./restAfterUpload";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import {
  useUploadQueueStore,
  distinctPendingHosts,
  nextPendingForHost,
  installOrderPriority,
  type QueueItem,
  type AddQueueItem,
} from "./uploadQueue";
import { useUploadSettingsStore } from "./uploadSettings";
import { runPkgInstall } from "./pkgLibrary";

const mockedJobStatus = vi.mocked(jobStatus);
const mockedStartFile = vi.mocked(startTransferFile);
const mockedEnsurePayload = vi.mocked(ensurePayloadCurrent);
const mockedStandby = vi.mocked(powerStandby);
const mockedPkgInstall = vi.mocked(runPkgInstall);
const mockedFsDelete = vi.mocked(fsDelete);
const mockedQueueSave = vi.mocked(uploadQueueSave);

function installLocalStorageStub() {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    get length() {
      return store.size;
    },
  });
}

function addItem(addr: string, name: string): void {
  const input: AddQueueItem = {
    sourceKind: "file",
    sourcePath: `/src/${name}`,
    displayName: name,
    resolvedDest: `/data/${name}`,
    addr,
    strategy: "overwrite",
    reconcileMode: "fast",
    excludes: [],
    mountAfterUpload: false,
    mountReadOnly: false,
    registerAfterUpload: false,
  };
  useUploadQueueStore.getState().add(input);
}

const itemsByStatus = (status: string) =>
  useUploadQueueStore.getState().items.filter((i) => i.status === status);

// ── Pure partition helpers ──────────────────────────────────────────────────

function qi(addr: string, status: QueueItem["status"]): QueueItem {
  return { id: addr + status, addr, status } as QueueItem;
}

describe("distinctPendingHosts", () => {
  it("returns pending hosts (port-stripped) in first-seen order, deduped", () => {
    const items = [
      qi("192.168.1.10:9113", "done"), // not pending → ignored
      qi("192.168.1.20:9113", "pending"),
      qi("192.168.1.10:9113", "pending"),
      qi("192.168.1.20:9114", "pending"), // same host, diff port → deduped
    ];
    expect(distinctPendingHosts(items)).toEqual(["192.168.1.20", "192.168.1.10"]);
  });

  it("is empty when nothing is pending", () => {
    expect(distinctPendingHosts([qi("a:9113", "done")])).toEqual([]);
  });
});

describe("nextPendingForHost", () => {
  it("returns the first pending item for the given host, ignoring others", () => {
    const items = [
      qi("10.0.0.1:9113", "running"),
      qi("10.0.0.2:9113", "pending"), // other host
      qi("10.0.0.1:9113", "pending"), // ← this one
    ];
    expect(nextPendingForHost(items, "10.0.0.1")?.id).toBe(
      "10.0.0.1:9113pending",
    );
    expect(nextPendingForHost(items, "10.0.0.9")).toBeNull();
  });
});

describe("install ordering (base → update → DLC)", () => {
  const pkg = (
    id: string,
    addr: string,
    category: string | null,
    dest = "/data/pkg_library/x.pkg",
  ): QueueItem =>
    ({
      id,
      addr,
      status: "pending",
      sourceKind: "pkg",
      category,
      resolvedDest: dest,
    }) as QueueItem;

  it("prioritises by category gd(0) < gp(1) < ac(2)", () => {
    expect(installOrderPriority(pkg("a", "h:9113", "gd"))).toBe(0);
    expect(installOrderPriority(pkg("b", "h:9113", "gp"))).toBe(1);
    expect(installOrderPriority(pkg("c", "h:9113", "ac"))).toBe(2);
  });

  it("falls back to the staged dest path when category is absent", () => {
    expect(
      installOrderPriority(
        pkg("u", "h:9113", null, "/data/pkg_library/updates/x.pkg"),
      ),
    ).toBe(1);
    expect(
      installOrderPriority(
        pkg("d", "h:9113", null, "/data/pkg_library/dlc/x.pkg"),
      ),
    ).toBe(2);
    expect(
      installOrderPriority(pkg("b", "h:9113", null, "/data/pkg_library/x.pkg")),
    ).toBe(0);
  });

  it("treats non-pkg items as priority 0", () => {
    expect(
      installOrderPriority({ id: "f", sourceKind: "folder" } as QueueItem),
    ).toBe(0);
  });

  it("picks base before update before DLC regardless of add order", () => {
    // The reported bug: a DLC + update queued AHEAD of the base.
    const items = [
      pkg("dlc", "h:9113", "ac"),
      pkg("update", "h:9113", "gp"),
      pkg("base", "h:9113", "gd"),
    ];
    expect(nextPendingForHost(items, "h")?.id).toBe("base");
  });

  it("keeps add-order within the same category (manual reorder preserved)", () => {
    const items = [
      pkg("dlc2", "h:9113", "ac"),
      pkg("dlc1", "h:9113", "ac"),
    ];
    // Both DLC (prio 2) → the first-added one wins, not re-sorted.
    expect(nextPendingForHost(items, "h")?.id).toBe("dlc2");
  });
});

// ── Runner: serial vs per-console parallel ───────────────────────────────────

describe("upload runner concurrency (per-console, parallel)", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus.mockReset().mockResolvedValue({
      status: "running",
    } as Awaited<ReturnType<typeof jobStatus>>);
    mockedStartFile.mockReset().mockResolvedValue("job");
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("start() runs DIFFERENT consoles concurrently", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");

    void useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(50);

    // Both consoles' first item should be running at once.
    const running = itemsByStatus("running");
    expect(running).toHaveLength(2);
    const hosts = running.map((i) => i.addr).sort();
    expect(hosts).toEqual(["192.168.1.10:9113", "192.168.1.20:9113"]);
    // Both hosts marked running, and the flat flag is true.
    expect(useUploadQueueStore.getState().runningHosts).toEqual({
      "192.168.1.10": true,
      "192.168.1.20": true,
    });
    expect(useUploadQueueStore.getState().running).toBe(true);
  });

  it("SAME console stays serial (its 2nd item waits)", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.10:9113", "A2"); // same console
    addItem("192.168.1.20:9113", "B1");

    void useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(50);

    const running = itemsByStatus("running");
    // One per console: A1 + B1 running, A2 pending behind A1.
    expect(running).toHaveLength(2);
    expect(running.map((i) => i.displayName).sort()).toEqual(["A1", "B1"]);
    expect(itemsByStatus("pending").map((i) => i.displayName)).toEqual(["A2"]);
  });

  it("startHost drains ONLY its own console", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");

    void useUploadQueueStore.getState().startHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);

    // Only console A is running; B stays pending and unstarted.
    expect(itemsByStatus("running").map((i) => i.displayName)).toEqual(["A1"]);
    expect(itemsByStatus("pending").map((i) => i.displayName)).toEqual(["B1"]);
    expect(useUploadQueueStore.getState().runningHosts).toEqual({
      "192.168.1.10": true,
    });
  });

  it("stopHost cancels the engine job for that console only", async () => {
    // Stopping must actually abort the transfer on the engine, not just
    // reset the row locally -- otherwise the upload keeps running on the
    // console after the UI says it stopped. And it must not cancel a
    // sibling console's job.
    const { jobCancel } = await import("../api/ps5");
    const mockedCancel = vi.mocked(jobCancel);
    mockedCancel.mockClear();

    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");

    void useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(50);

    useUploadQueueStore.getState().stopHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);

    expect(mockedCancel).toHaveBeenCalledTimes(1);
    // Whatever id the started transfer returned is the one cancelled.
    expect(mockedCancel).toHaveBeenCalledWith("job");
  });

  it("cancels a job whose start was still in flight when Stop was clicked", async () => {
    // The window this covers: POST /api/transfer/{...} has been sent but has
    // not answered yet, so the UI holds no job id. The engine's .rar route is
    // the worst case — it plans the entire archive inside the request handler
    // before minting the job id, so a large .rar sits in that call for
    // seconds. A user who hits Cancel during that window used to orphan the
    // transfer: the store's generation was bumped, the late-arriving job id
    // was dropped on the floor, and the engine happily uploaded the whole
    // archive with nothing left able to cancel it. Killing the app was the
    // only way to stop it (user report, 5.4.7).
    const { jobCancel } = await import("../api/ps5");
    const mockedCancel = vi.mocked(jobCancel);
    mockedCancel.mockClear();

    let release!: (jobId: string) => void;
    mockedStartFile.mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          release = resolve;
        }),
    );

    addItem("192.168.1.10:9113", "A1");
    void useUploadQueueStore.getState().startHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);

    // Cancel lands mid-flight: there is genuinely nothing to cancel yet.
    useUploadQueueStore.getState().stopHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);
    expect(mockedCancel).not.toHaveBeenCalled();

    // The engine finally answers. That job is live on the wire right now, so
    // the late id must be cancelled rather than discarded.
    release("late-job");
    await vi.advanceTimersByTimeAsync(50);

    expect(mockedCancel).toHaveBeenCalledWith("late-job");
  });

  it("stopHost stops ONE console while siblings keep running", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");

    void useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(50);
    expect(itemsByStatus("running")).toHaveLength(2);

    useUploadQueueStore.getState().stopHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);

    // A reset to pending, B still uploading.
    const byName = (st: string) =>
      itemsByStatus(st).map((i) => i.displayName).sort();
    expect(byName("pending")).toEqual(["A1"]);
    expect(byName("running")).toEqual(["B1"]);
    expect(useUploadQueueStore.getState().runningHosts).toEqual({
      "192.168.1.20": true,
    });
    expect(useUploadQueueStore.getState().running).toBe(true);
  });

  it("both consoles drain to completion (running clears)", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");
    // Let every job report done on the first poll.
    mockedJobStatus.mockResolvedValue({
      status: "done",
      bytes_sent: 100,
      elapsed_ms: 10,
    } as Awaited<ReturnType<typeof jobStatus>>);

    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;

    expect(itemsByStatus("done")).toHaveLength(2);
    expect(useUploadQueueStore.getState().running).toBe(false);
    expect(useUploadQueueStore.getState().runningHosts).toEqual({});
  });
});

// ── Rest mode after upload (#165) ────────────────────────────────────────────

describe("rest mode after upload", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedStandby.mockReset().mockResolvedValue({ ok: true } as Awaited<
      ReturnType<typeof powerStandby>
    >);
    // Every job reports done on the first poll so a drain completes fast.
    mockedJobStatus.mockReset().mockResolvedValue({
      status: "done",
      bytes_sent: 100,
      elapsed_ms: 10,
    } as Awaited<ReturnType<typeof jobStatus>>);
    mockedStartFile.mockReset().mockResolvedValue("job");
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
    });
    useRestAfterUploadStore.setState({ enabled: false });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    useRestAfterUploadStore.setState({ enabled: false });
    vi.useRealTimers();
  });

  it("does NOT enter rest mode when the setting is off (default)", async () => {
    addItem("192.168.1.10:9113", "A1");
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;
    expect(itemsByStatus("done")).toHaveLength(1);
    expect(mockedStandby).not.toHaveBeenCalled();
  });

  it("enters rest mode on the drained console when enabled", async () => {
    useRestAfterUploadStore.setState({ enabled: true });
    addItem("192.168.1.10:9113", "A1");
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;
    expect(itemsByStatus("done")).toHaveLength(1);
    // Standby targets the mgmt addr of the drained host.
    expect(mockedStandby).toHaveBeenCalledTimes(1);
    expect(mockedStandby).toHaveBeenCalledWith("192.168.1.10:9114");
  });

  it("sleeps EACH console that drains, independently", async () => {
    useRestAfterUploadStore.setState({ enabled: true });
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;
    const called = mockedStandby.mock.calls.map((c) => c[0]).sort();
    expect(called).toEqual(["192.168.1.10:9114", "192.168.1.20:9114"]);
  });

  it("does NOT sleep a console that was Stopped mid-drain", async () => {
    useRestAfterUploadStore.setState({ enabled: true });
    // Keep the job running so the item never reaches "done".
    mockedJobStatus.mockResolvedValue({
      status: "running",
    } as Awaited<ReturnType<typeof jobStatus>>);
    addItem("192.168.1.10:9113", "A1");
    void useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().stopHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(200);
    // Stop re-stamps the generation, so the drain's finally block skips the
    // hook (isLive() is false) and nothing completed anyway.
    expect(mockedStandby).not.toHaveBeenCalled();
  });
});

// ── Per-item cancel ──────────────────────────────────────────────────────────

describe("cancelItem (per-item cancel)", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus.mockReset().mockResolvedValue({
      status: "running",
    } as Awaited<ReturnType<typeof jobStatus>>);
    mockedStartFile.mockReset().mockResolvedValue("job");
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  const byId = (name: string) =>
    useUploadQueueStore.getState().items.find((i) => i.displayName === name)!;

  it("removes a PENDING item without disturbing the running one", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.10:9113", "A2"); // pending behind A1 (same console)

    void useUploadQueueStore.getState().startHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);
    expect(itemsByStatus("running").map((i) => i.displayName)).toEqual(["A1"]);

    useUploadQueueStore.getState().cancelItem(byId("A2").id);
    await vi.advanceTimersByTimeAsync(10);

    // A2 gone, A1 still uploading.
    expect(
      useUploadQueueStore.getState().items.map((i) => i.displayName),
    ).toEqual(["A1"]);
    expect(itemsByStatus("running").map((i) => i.displayName)).toEqual(["A1"]);
  });

  it("cancels the RUNNING item and resumes the console's next pending", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.10:9113", "A2");

    void useUploadQueueStore.getState().startHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);
    expect(byId("A1").status).toBe("running");

    useUploadQueueStore.getState().cancelItem(byId("A1").id);
    await vi.advanceTimersByTimeAsync(200);

    // A1 dropped; A2 now the running item; console still active.
    expect(
      useUploadQueueStore.getState().items.map((i) => i.displayName),
    ).toEqual(["A2"]);
    expect(itemsByStatus("running").map((i) => i.displayName)).toEqual(["A2"]);
    expect(useUploadQueueStore.getState().runningHosts).toEqual({
      "192.168.1.10": true,
    });
  });

  it("cancelling the only running item leaves the console idle", async () => {
    addItem("192.168.1.10:9113", "A1");

    void useUploadQueueStore.getState().startHost("192.168.1.10");
    await vi.advanceTimersByTimeAsync(50);

    useUploadQueueStore.getState().cancelItem(byId("A1").id);
    await vi.advanceTimersByTimeAsync(100);

    expect(useUploadQueueStore.getState().items).toHaveLength(0);
    expect(useUploadQueueStore.getState().running).toBe(false);
    expect(useUploadQueueStore.getState().runningHosts).toEqual({});
  });

  it("does not touch a sibling console when cancelling a running item", async () => {
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");

    void useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(50);
    expect(itemsByStatus("running")).toHaveLength(2);

    useUploadQueueStore.getState().cancelItem(byId("A1").id);
    await vi.advanceTimersByTimeAsync(100);

    // A1 gone, B1 keeps uploading untouched.
    expect(
      useUploadQueueStore.getState().items.map((i) => i.displayName),
    ).toEqual(["B1"]);
    expect(itemsByStatus("running").map((i) => i.displayName)).toEqual(["B1"]);
    expect(useUploadQueueStore.getState().runningHosts).toEqual({
      "192.168.1.20": true,
    });
  });
});

describe("queue recovery and persistence visibility", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedQueueSave.mockReset().mockResolvedValue(undefined);
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
      persistenceError: null,
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("retries only the selected failed item", () => {
    addItem("192.168.1.10:9113", "failed.bin");
    addItem("192.168.1.10:9113", "pending.bin");
    const [failed, pending] = useUploadQueueStore.getState().items;
    useUploadQueueStore.setState({
      items: [
        {
          ...failed,
          status: "failed",
          error: "network failed",
          errorReason: "socket_closed",
          errorDetail: "reset",
          completedAt: 123,
        },
        pending,
      ],
    });

    expect(useUploadQueueStore.getState().retryItem(failed.id)).toBe(true);
    const [retried, untouched] = useUploadQueueStore.getState().items;
    expect(retried).toMatchObject({
      id: failed.id,
      status: "pending",
      error: null,
      errorReason: null,
      errorDetail: null,
      completedAt: null,
    });
    expect(untouched.id).toBe(pending.id);
    expect(useUploadQueueStore.getState().retryItem(pending.id)).toBe(false);
  });

  it("surfaces a failed save and clears the warning after a successful save", async () => {
    mockedQueueSave.mockRejectedValueOnce(new Error("disk full"));
    addItem("192.168.1.10:9113", "first.bin");
    await vi.advanceTimersByTimeAsync(400);
    expect(useUploadQueueStore.getState().persistenceError).toContain("disk full");

    addItem("192.168.1.10:9113", "second.bin");
    await vi.advanceTimersByTimeAsync(400);
    expect(useUploadQueueStore.getState().persistenceError).toBeNull();
  });
});

// ── Runner: auto-resume after failure ────────────────────────────────────────

describe("upload runner auto-resume", () => {
  const ADDR = "192.168.1.10:9113";

  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus.mockReset();
    mockedStartFile.mockReset().mockResolvedValue("job");
    mockedEnsurePayload.mockReset().mockResolvedValue(undefined as never);
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: false,
      loaded: true,
    });
    useUploadSettingsStore.setState({ autoResume: true });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("recoverable failure → re-deploys payload, retries, and completes", async () => {
    addItem(ADDR, "A1");
    // Attempt 0 fails with a connection-class error (no payload reason ⇒
    // recoverable); the retry's poll reports done.
    mockedJobStatus
      .mockResolvedValueOnce({
        status: "failed",
        error: "connection reset by peer",
      } as Awaited<ReturnType<typeof jobStatus>>)
      .mockResolvedValue({
        status: "done",
        bytes_sent: 100,
      } as Awaited<ReturnType<typeof jobStatus>>);

    const p = useUploadQueueStore.getState().start();
    // Drive through: first poll → fail → recovering → 5s backoff → heal →
    // retry → done.
    await vi.advanceTimersByTimeAsync(20_000);
    await p;

    expect(itemsByStatus("done")).toHaveLength(1);
    // Two transfer starts = original + one resume.
    expect(mockedStartFile).toHaveBeenCalledTimes(2);
    // Healed at least once beyond the preflight (preflight + recovery).
    expect(mockedEnsurePayload.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("surfaces the 'recovering' state between attempts", async () => {
    addItem(ADDR, "A1");
    mockedJobStatus
      .mockResolvedValueOnce({
        status: "failed",
        error: "broken pipe",
      } as Awaited<ReturnType<typeof jobStatus>>)
      .mockResolvedValue({
        status: "running",
      } as Awaited<ReturnType<typeof jobStatus>>);

    void useUploadQueueStore.getState().start();
    // Far enough to hit the failure + enter recovering, but inside the 5s
    // backoff so it hasn't retried yet.
    await vi.advanceTimersByTimeAsync(1_000);

    const item = useUploadQueueStore.getState().items[0];
    expect(item.status).toBe("running");
    expect(item.recovering).toBe(true);
    expect(item.recoverAttempt).toBe(1);
  });

  it("fatal failure (out of space) → fails immediately, no retry", async () => {
    addItem(ADDR, "A1");
    mockedJobStatus.mockResolvedValue({
      status: "failed",
      error: "no space",
      error_reason: "fs_write_failed_errno_28",
    } as Awaited<ReturnType<typeof jobStatus>>);

    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(20_000);
    await p;

    expect(itemsByStatus("failed")).toHaveLength(1);
    // No resume attempt for a fatal error.
    expect(mockedStartFile).toHaveBeenCalledTimes(1);
  });

  it("auto-resume OFF → a recoverable failure still fails immediately", async () => {
    useUploadSettingsStore.setState({ autoResume: false });
    addItem(ADDR, "A1");
    mockedJobStatus.mockResolvedValue({
      status: "failed",
      error: "connection reset by peer",
    } as Awaited<ReturnType<typeof jobStatus>>);

    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(20_000);
    await p;

    expect(itemsByStatus("failed")).toHaveLength(1);
    expect(mockedStartFile).toHaveBeenCalledTimes(1);
  });

  it("Stop during a recovery backoff aborts cleanly without retrying", async () => {
    addItem(ADDR, "A1");
    // Always fail recoverably, so without a Stop it would loop + heal.
    mockedJobStatus.mockResolvedValue({
      status: "failed",
      error: "connection reset by peer",
    } as Awaited<ReturnType<typeof jobStatus>>);

    void useUploadQueueStore.getState().start();
    // Get into the first recovery's backoff window.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(useUploadQueueStore.getState().items[0].recovering).toBe(true);
    const startsBeforeStop = mockedStartFile.mock.calls.length;
    const healsBeforeStop = mockedEnsurePayload.mock.calls.length;

    useUploadQueueStore.getState().stop();
    // Advance well past the 5s backoff + any heal poll.
    await vi.advanceTimersByTimeAsync(60_000);

    // No further transfer start and no recovery heal after Stop.
    expect(mockedStartFile.mock.calls.length).toBe(startsBeforeStop);
    expect(mockedEnsurePayload.mock.calls.length).toBe(healsBeforeStop);
    expect(useUploadQueueStore.getState().running).toBe(false);
    // The stopped item must not be left claiming to be recovering.
    const item = useUploadQueueStore.getState().items[0];
    expect(item.recovering).toBe(false);
    expect(item.status).not.toBe("running");
  });

  it("a console added mid-run is still drained (hot-add)", async () => {
    // continueOnFailure=true is required for the re-loop to pick up new hosts.
    useUploadQueueStore.setState({ continueOnFailure: true });
    mockedJobStatus.mockResolvedValue({
      status: "done",
      bytes_sent: 100,
    } as Awaited<ReturnType<typeof jobStatus>>);

    addItem("192.168.1.10:9113", "A1");
    const p = useUploadQueueStore.getState().start();
    // Add a second console AFTER start() captured its initial host set.
    addItem("192.168.1.20:9113", "B1");
    await vi.advanceTimersByTimeAsync(10_000);
    await p;

    // Both consoles' items drained, not left stuck pending.
    expect(itemsByStatus("done").map((i) => i.displayName).sort()).toEqual([
      "A1",
      "B1",
    ]);
    expect(itemsByStatus("pending")).toHaveLength(0);
  });

  it("gives up after the attempt cap and surfaces the failure", async () => {
    addItem(ADDR, "A1");
    // Always fail with a recoverable error: original + 3 recovery attempts,
    // then terminal failed.
    mockedJobStatus.mockResolvedValue({
      status: "failed",
      error: "connection reset by peer",
    } as Awaited<ReturnType<typeof jobStatus>>);

    const p = useUploadQueueStore.getState().start();
    // 5s + 15s + 30s backoffs ⇒ well under 90s of fake time.
    await vi.advanceTimersByTimeAsync(120_000);
    await p;

    expect(itemsByStatus("failed")).toHaveLength(1);
    // 1 original + 3 recovery retries = 4 transfer starts.
    expect(mockedStartFile).toHaveBeenCalledTimes(4);
  });
});

describe("upload runner — unverified PKG install", () => {
  const ADDR = "192.168.1.10:9113";

  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus.mockReset().mockResolvedValue({
      status: "done",
      bytes_sent: 100,
      elapsed_ms: 10,
      dest: "/user/data/ps5upload/pkg_library/Game.pkg",
    } as Awaited<ReturnType<typeof jobStatus>>);
    mockedStartFile.mockReset().mockResolvedValue("job");
    mockedFsDelete.mockClear();
    mockedPkgInstall.mockReset().mockResolvedValue({
      installed: false,
      mayNotLaunch: false,
      acceptedUnverified: true,
      errMessage: "Accepted, but completion was not verified.",
    });
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
    });
  });

  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("finishes the upload with a warning and keeps staging", async () => {
    useUploadQueueStore.getState().add({
      sourceKind: "pkg",
      sourcePath: "/src/Game.pkg",
      displayName: "Game.pkg",
      resolvedDest: "/user/data/ps5upload/pkg_library/Game.pkg",
      addr: ADDR,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: false,
      registerAfterUpload: false,
      installAfterUpload: true,
      deletePkgAfterInstall: true,
      contentId: "UP0000-CUSA00001_00-GAME000000000000",
    });

    const run = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5_000);
    await run;

    const item = useUploadQueueStore.getState().items[0];
    expect(item.status).toBe("done");
    expect(item.installPhase).toBe("unverified");
    expect(item.mountWarnings).toContain(
      "Accepted, but completion was not verified.",
    );
    expect(mockedFsDelete).not.toHaveBeenCalled();
  });
});

describe("upload runner — a rejected PKG install must not re-upload", () => {
  const ADDR = "192.168.1.10:9113";

  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus.mockReset().mockResolvedValue({
      status: "done",
      bytes_sent: 100,
      elapsed_ms: 10,
      dest: "/user/data/ps5upload/pkg_library/updates/Update.pkg",
    } as Awaited<ReturnType<typeof jobStatus>>);
    mockedStartFile.mockReset().mockResolvedValue("job");
    mockedFsDelete.mockClear();
    // The 2026-09-08 report: the console never saw the update, and the
    // message is one the recovery policy has never seen — in any of 19
    // languages.
    mockedPkgInstall.mockReset().mockResolvedValue({
      installed: false,
      mayNotLaunch: false,
      errMessage:
        "This update couldn’t be applied because ps5upload couldn’t reach " +
        "your PS5’s payload loader on port 9021…",
    });
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
    });
  });

  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("fails the row once, with exactly one transfer", async () => {
    useUploadQueueStore.getState().add({
      sourceKind: "pkg",
      sourcePath: "/src/Update.pkg",
      displayName: "Update.pkg",
      resolvedDest: "/user/data/ps5upload/pkg_library/updates/Update.pkg",
      addr: ADDR,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: false,
      registerAfterUpload: false,
      installAfterUpload: true,
      deletePkgAfterInstall: true,
      contentId: "UP0000-CUSA00001_00-GAME000000000000",
    });

    const run = useUploadQueueStore.getState().start();
    // Long past every auto-recovery backoff (5s + 15s + 30s).
    await vi.advanceTimersByTimeAsync(120_000);
    await run;

    const item = useUploadQueueStore.getState().items[0];
    expect(item.status).toBe("failed");
    // The whole point: the bytes were committed, so re-running the item would
    // re-upload the package. A user watched 5.93 GiB start over six seconds
    // after this error. Exactly one transfer, ever.
    expect(mockedStartFile).toHaveBeenCalledTimes(1);
    // And the staged pkg is kept, so the retry is an install retry.
    expect(mockedFsDelete).not.toHaveBeenCalled();
  });

  it("still auto-recovers a genuine transport failure", async () => {
    // Guard against over-correcting: the case auto-recovery exists for must
    // keep re-running the item.
    mockedJobStatus.mockReset().mockResolvedValue({
      status: "failed",
      error: "connect 192.168.1.10:9113: Connection refused",
    } as Awaited<ReturnType<typeof jobStatus>>);
    useUploadQueueStore.getState().add({
      sourceKind: "pkg",
      sourcePath: "/src/Update.pkg",
      displayName: "Update.pkg",
      resolvedDest: "/user/data/ps5upload/pkg_library/updates/Update.pkg",
      addr: ADDR,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: false,
      registerAfterUpload: false,
      installAfterUpload: true,
      deletePkgAfterInstall: true,
      contentId: "UP0000-CUSA00001_00-GAME000000000000",
    });
    const run = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(120_000);
    await run;
    expect(mockedStartFile).toHaveBeenCalledTimes(4); // 1 + 3 retries
  });
});

// ── ShadowMount+ hand-off on image upload + mount-after-upload ────────────────

describe("upload runner — ShadowMount+ hand-off (image + mountAfterUpload)", () => {
  const ADDR = "192.168.1.10:9113";
  const mockedFsMount = vi.mocked(fsMount);
  const mockedSmpStatus = vi.mocked(smpStatus);
  const mockedSmpInstall = vi.mocked(smpManualInstall);

  function addImageItem(): void {
    useUploadQueueStore.getState().add({
      sourceKind: "image",
      sourcePath: "/src/g.ffpkg",
      displayName: "g.ffpkg",
      resolvedDest: "/data/homebrew/g.ffpkg",
      addr: ADDR,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: true,
      mountReadOnly: true,
      registerAfterUpload: false,
    } as AddQueueItem);
  }

  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus
      .mockReset()
      .mockResolvedValue({
        status: "done",
        bytes_sent: 100,
        elapsed_ms: 10,
        dest: "/data/homebrew/g.ffpkg",
      } as Awaited<ReturnType<typeof jobStatus>>);
    mockedStartFile.mockReset().mockResolvedValue("job");
    mockedFsMount.mockClear();
    mockedSmpStatus.mockReset();
    mockedSmpInstall.mockReset().mockResolvedValue({ added: true });
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      continueOnFailure: true,
      loaded: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("SMP running → hands off via manual.lst, does NOT mount itself", async () => {
    mockedSmpStatus.mockResolvedValue({ running: true } as Awaited<
      ReturnType<typeof smpStatus>
    >);
    addImageItem();
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;

    expect(mockedSmpInstall).toHaveBeenCalledWith(
      expect.any(String),
      "/data/homebrew/g.ffpkg",
    );
    expect(mockedFsMount).not.toHaveBeenCalled();
    expect(itemsByStatus("done")).toHaveLength(1);
  });

  it("SMP not running → mounts natively, no hand-off", async () => {
    mockedSmpStatus.mockResolvedValue({ running: false } as Awaited<
      ReturnType<typeof smpStatus>
    >);
    addImageItem();
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;

    expect(mockedFsMount).toHaveBeenCalledTimes(1);
    expect(mockedSmpInstall).not.toHaveBeenCalled();
    expect(itemsByStatus("done")).toHaveLength(1);
  });

  it("SMP status probe throws → falls back to native mount", async () => {
    mockedSmpStatus.mockRejectedValue(new Error("unreachable"));
    addImageItem();
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;

    expect(mockedFsMount).toHaveBeenCalledTimes(1);
    expect(itemsByStatus("done")).toHaveLength(1);
  });
});

describe("resumeFailedRecoverable", () => {
  beforeEach(() => {
    installLocalStorageStub();
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      loaded: true,
    });
    useUploadSettingsStore.setState({ autoResume: true });
  });

  /** Add a row, then force it to a failed state with the given failure
   *  classification, and return its id. */
  function failItem(
    addr: string,
    name: string,
    opts: { reason?: string | null; message?: string | null },
  ): string {
    addItem(addr, name);
    const item = useUploadQueueStore
      .getState()
      .items.find((i) => i.displayName === name)!;
    useUploadQueueStore.setState((s) => ({
      items: s.items.map((i) =>
        i.id === item.id
          ? {
              ...i,
              status: "failed" as const,
              errorReason: opts.reason ?? null,
              error: opts.message ?? null,
            }
          : i,
      ),
    }));
    return item.id;
  }

  const byId = (id: string) =>
    useUploadQueueStore.getState().items.find((i) => i.id === id)!;

  it("re-drives only connection-class failures for the target host, then restarts it", async () => {
    const startHost = vi.fn(async () => {});
    useUploadQueueStore.setState({ startHost });

    const recoverA = failItem("192.168.1.10:9113", "A-net", {
      message: "connection refused",
    });
    const fatalA = failItem("192.168.1.10:9113", "A-space", {
      reason: "no_space",
    });
    const recoverB = failItem("192.168.1.20:9113", "B-net", {
      message: "connection reset",
    });

    const n = await useUploadQueueStore
      .getState()
      .resumeFailedRecoverable("192.168.1.10:9113");

    expect(n).toBe(1);
    expect(byId(recoverA).status).toBe("pending"); // resumed
    expect(byId(fatalA).status).toBe("failed"); // fatal → left alone
    expect(byId(recoverB).status).toBe("failed"); // other host → untouched
    expect(startHost).toHaveBeenCalledTimes(1);
    expect(startHost).toHaveBeenCalledWith("192.168.1.10:9113");
  });

  it("no-ops and never starts the queue when autoResume is off", async () => {
    const startHost = vi.fn(async () => {});
    useUploadQueueStore.setState({ startHost });
    useUploadSettingsStore.setState({ autoResume: false });

    const id = failItem("192.168.1.10:9113", "A", {
      message: "connection refused",
    });

    const n = await useUploadQueueStore
      .getState()
      .resumeFailedRecoverable("192.168.1.10:9113");

    expect(n).toBe(0);
    expect(byId(id).status).toBe("failed");
    expect(startHost).not.toHaveBeenCalled();
  });

  it("does not restart the drain when only fatal failures remain", async () => {
    const startHost = vi.fn(async () => {});
    useUploadQueueStore.setState({ startHost });

    const id = failItem("192.168.1.10:9113", "A", {
      reason: "path_not_allowed",
    });

    const n = await useUploadQueueStore
      .getState()
      .resumeFailedRecoverable("192.168.1.10:9113");

    expect(n).toBe(0);
    expect(byId(id).status).toBe("failed");
    expect(startHost).not.toHaveBeenCalled();
  });
});
