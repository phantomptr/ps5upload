import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Keep the real api/ps5 (UploadJobError class, generateTxIdHex, etc.) and
// only override the network-touching functions so we can drive the runner
// deterministically without a PS5 or engine.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const releaseCopy = vi.fn(async () => {});
const materializeRemote = vi.fn(async (p: string) => p);
vi.mock("../lib/materialize", () => ({
  releaseCopy: (...a: unknown[]) => releaseCopy(...(a as [])),
  materializeRemote: (p: string) => materializeRemote(p),
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
    pkgInstallStop: vi.fn(async () => {}),
  };
});
vi.mock("../api/ava1", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ava1")>();
  return { ...actual, startPs5ToPs5: vi.fn(async () => "c2c-job") };
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
  uploadQueueLoad,
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
import { runPkgInstall, pkgLibraryStore } from "./pkgLibrary";
import {
  registerInstallExecutor,
  registerInstallJobResolver,
  type InstallResult,
} from "./consoleQueueBridge";
import { isUploadItem, libraryInstallStates, sameInstall } from "./uploadQueue";

// The store before any test stubs its actions (some blocks replace startHost).
const pristineQueue = useUploadQueueStore.getState();

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

  it("keeps the engine job id on a finished item (the key for its job summary)", async () => {
    addItem("192.168.1.10:9113", "A1");
    mockedStartFile.mockResolvedValueOnce("job-for-summary");
    mockedJobStatus.mockResolvedValue({
      status: "done",
      bytes_sent: 100,
      elapsed_ms: 10,
    } as Awaited<ReturnType<typeof jobStatus>>);
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;
    expect(itemsByStatus("done")[0].jobId).toBe("job-for-summary");
  });
});

// ── From another console (#433) ──────────────────────────────────────────────

describe("an item from another console", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    useUploadQueueStore.setState({ items: [], running: false, runningHosts: {}, loaded: true });
  });

  it("is copied from that console to this one, resuming under its own job id", async () => {
    const { startPs5ToPs5 } = await import("../api/ava1");
    vi.mocked(startPs5ToPs5).mockClear();
    useUploadQueueStore.getState().add({
      sourceKind: "ps5",
      fromConsole: "192.168.1.100",
      sourcePath: "/data/homebrew/PPSA11386-app",
      displayName: "PPSA11386-app",
      resolvedDest: "/data/homebrew/PPSA11386-app",
      addr: "192.168.1.99",
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: false,
      registerAfterUpload: false,
    });
    mockedJobStatus.mockResolvedValue({
      status: "done",
      bytes_sent: 100,
      elapsed_ms: 10,
    } as Awaited<ReturnType<typeof jobStatus>>);
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;
    const item = useUploadQueueStore.getState().items[0];
    expect(vi.mocked(startPs5ToPs5)).toHaveBeenCalledWith(
      "192.168.1.100",
      "/data/homebrew/PPSA11386-app",
      "192.168.1.99",
      "/data/homebrew/PPSA11386-app",
      item.txIdHex,
    );
    expect(item.status).toBe("done");
    expect(mockedStartFile).not.toHaveBeenCalledWith(
      "/data/homebrew/PPSA11386-app",
      expect.anything(),
      expect.anything(),
      expect.anything(),
    );
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
    expect(mockedStandby).toHaveBeenCalledWith("192.168.1.10");
  });

  it("sleeps EACH console that drains, independently", async () => {
    useRestAfterUploadStore.setState({ enabled: true });
    addItem("192.168.1.10:9113", "A1");
    addItem("192.168.1.20:9113", "B1");
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(5000);
    await p;
    const called = mockedStandby.mock.calls.map((c) => c[0]).sort();
    expect(called).toEqual(["192.168.1.10", "192.168.1.20"]);
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

  it("retries a password-failed archive with the typed password, in memory only", async () => {
    addItem("192.168.1.10:9113", "game.rar");
    const [it0] = useUploadQueueStore.getState().items;
    useUploadQueueStore.setState({
      items: [
        {
          ...it0,
          sourceKind: "archive",
          status: "failed",
          error: "rar_password_required",
          errorReason: "ava1_rar_password_required",
        },
      ],
    });
    expect(
      useUploadQueueStore.getState().retryWithPassword(it0.id, "s3cret"),
    ).toBe(true);
    const [r] = useUploadQueueStore.getState().items;
    expect(r).toMatchObject({ status: "pending", error: null, rarPassword: "s3cret" });
    // The password reaches the live item, never the saved document.
    await vi.advanceTimersByTimeAsync(400);
    const saved = JSON.stringify(mockedQueueSave.mock.calls[mockedQueueSave.mock.calls.length - 1] ?? []);
    expect(saved).not.toContain("s3cret");
    // An empty password retries nothing; a row that did not fail for a password is refused.
    expect(useUploadQueueStore.getState().retryWithPassword(it0.id, "")).toBe(false);
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

  it("creates every folder down to a package staged on a USB drive", async () => {
    const { fsMkdir } = await import("../api/ps5");
    vi.mocked(fsMkdir).mockClear();
    useUploadQueueStore.getState().add({
      sourceKind: "pkg",
      sourcePath: "/src/Update.pkg",
      displayName: "Update.pkg",
      resolvedDest: "/mnt/usb0/ps5upload/pkg_library/updates/abc/Update.pkg",
      addr: ADDR,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: false,
      registerAfterUpload: false,
      installAfterUpload: false,
      deletePkgAfterInstall: false,
      contentId: "UP0000-CUSA00001_00-GAME000000000000",
    });
    const run = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(120_000);
    await run;
    expect(vi.mocked(fsMkdir).mock.calls.map((c) => c[1])).toEqual([
      "/mnt/usb0/ps5upload",
      "/mnt/usb0/ps5upload/pkg_library",
      "/mnt/usb0/ps5upload/pkg_library/updates",
      "/mnt/usb0/ps5upload/pkg_library/updates/abc",
    ]);
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

describe("install items", () => {
  const host = "10.0.0.2";
  let calls: string[];
  let gate: Map<string, (r: InstallResult) => void>;

  beforeEach(() => {
    calls = [];
    gate = new Map();
    registerInstallExecutor(
      (req) =>
        new Promise<InstallResult>((resolve) => {
          const key =
            req.via === "stream" ? req.source : req.via === "library" ? req.path : req.via;
          calls.push(key);
          gate.set(key, resolve);
        }),
    );
    // Earlier blocks leave fake timers, live drain loops and mocks behind.
    vi.useRealTimers();
    useUploadQueueStore.getState().stop();
    mockedEnsurePayload.mockReset().mockResolvedValue(undefined as never);
    useUploadQueueStore.setState({
      ...pristineQueue,
      items: [],
      runningHosts: {},
      running: false,
      continueOnFailure: true,
      loaded: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
  });

  const waitFor = async (cond: () => boolean) => {
    for (let i = 0; i < 400 && !cond(); i++) await new Promise((r) => setTimeout(r, 5));
    expect(cond()).toBe(true);
  };

  it("runs an install item and resolves its waiter with the result", async () => {
    const q = useUploadQueueStore.getState().enqueueInstall({
      host,
      request: { via: "stream", source: "/games/a.pkg" },
      displayName: "A",
    });
    await waitFor(() => calls.length === 1);
    gate.get("/games/a.pkg")!({ ok: true });
    await expect(q.done).resolves.toEqual({ ok: true });
    await waitFor(
      () => useUploadQueueStore.getState().items.find((i) => i.id === q.id)?.status === "done",
    );
  });

  it("[RF 3] a second install on the same console waits for the first", async () => {
    const a = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/a.pkg" }, displayName: "A",
    });
    const b = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "library", path: "/staged/b.pkg" }, displayName: "B",
    });
    await waitFor(() => calls.length === 1);
    expect(calls).toEqual(["/a.pkg"]);
    gate.get("/a.pkg")!({ ok: true });
    await a.done;
    await waitFor(() => calls.length === 2);
    gate.get("/staged/b.pkg")!({ ok: false, message: "rejected" });
    await expect(b.done).resolves.toEqual({ ok: false, message: "rejected" });
  });

  it("orders installs base → update → DLC by category", async () => {
    const s = useUploadQueueStore.getState();
    // Queue all three while the console is held by a first install, so the
    // drain rule (not add order) decides what runs next.
    const hold = s.enqueueInstall({ host, request: { via: "stream", source: "/hold.pkg" }, displayName: "H" });
    await waitFor(() => calls.length === 1);
    const dlc = s.enqueueInstall({ host, request: { via: "stream", source: "/dlc.pkg" }, displayName: "D", category: "ac" });
    const upd = s.enqueueInstall({ host, request: { via: "stream", source: "/upd.pkg" }, displayName: "U", category: "gp" });
    const base = s.enqueueInstall({ host, request: { via: "stream", source: "/base.pkg" }, displayName: "B", category: "gd" });
    gate.get("/hold.pkg")!({ ok: true });
    await hold.done;
    for (const key of ["/base.pkg", "/upd.pkg", "/dlc.pkg"]) {
      await waitFor(() => gate.has(key));
      gate.get(key)!({ ok: true });
    }
    await Promise.all([dlc.done, upd.done, base.done]);
    expect(calls).toEqual(["/hold.pkg", "/base.pkg", "/upd.pkg", "/dlc.pkg"]);
  });

  it("a running install can be stopped, and its row goes once the job ends", async () => {
    const api = await import("../api/ps5");
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/stop.pkg" }, displayName: "S",
    });
    await waitFor(() => calls.length === 1);
    useUploadQueueStore.setState((st) => ({
      items: st.items.map((i) => (i.id === q.id ? { ...i, installJobId: "job-7" } : i)),
    }));
    useUploadQueueStore.getState().cancelItem(q.id);
    expect(vi.mocked(api.pkgInstallStop)).toHaveBeenCalledWith("job-7");
    expect(useUploadQueueStore.getState().items.find((i) => i.id === q.id)?.stopping).toBe(true);
    // The engine ends the job as stopped; the row then leaves the queue.
    gate.get("/stop.pkg")!({ ok: false, message: "stopped" });
    await waitFor(() => !useUploadQueueStore.getState().items.some((i) => i.id === q.id));
  });

  it("an install stopped before the engine has its job is stopped once the job comes", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.pkgInstallStop).mockClear();
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/early.pkg" }, displayName: "E",
    });
    await waitFor(() => calls.length === 1);
    useUploadQueueStore.getState().cancelItem(q.id);
    expect(vi.mocked(api.pkgInstallStop)).not.toHaveBeenCalled();
    // The engine answers with its job a moment later: that job is the one stopped.
    useUploadQueueStore.setState((st) => ({
      items: st.items.map((i) => (i.id === q.id ? { ...i, installJobId: "job-late" } : i)),
    }));
    await waitFor(() => vi.mocked(api.pkgInstallStop).mock.calls.length > 0);
    expect(vi.mocked(api.pkgInstallStop)).toHaveBeenCalledWith("job-late");
    gate.get("/early.pkg")!({ ok: false, message: "stopped" });
    await waitFor(() => !useUploadQueueStore.getState().items.some((i) => i.id === q.id));
  });

  it("an install leaves uploads waiting for Start alone (#410)", async () => {
    const s = useUploadQueueStore.getState();
    s.add({
      sourceKind: "folder",
      sourcePath: "/src/Game",
      displayName: "Game",
      resolvedDest: "/data/homebrew/Game",
      addr: `${host}:9113`,
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: false,
      registerAfterUpload: false,
    });
    const q = s.enqueueInstall({ host, request: { via: "stream", source: "/ps4.pkg" }, displayName: "P" });
    await waitFor(() => calls.length === 1);
    gate.get("/ps4.pkg")!({ ok: true });
    await q.done;
    await waitFor(() => !useUploadQueueStore.getState().runningHosts[host]);
    const upload = useUploadQueueStore.getState().items.find((i) => i.sourcePath === "/src/Game");
    expect(upload?.status).toBe("pending");
  });

  it("a second identical install joins the first instead of failing", async () => {
    const s = useUploadQueueStore.getState();
    const a = s.enqueueInstall({ host, request: { via: "stream", source: "/x.pkg" }, displayName: "X" });
    const b = s.enqueueInstall({ host, request: { via: "stream", source: "/x.pkg" }, displayName: "X" });
    expect(b.id).toBe(a.id);
    await waitFor(() => calls.length === 1);
    gate.get("/x.pkg")!({ ok: true });
    await expect(a.done).resolves.toEqual({ ok: true });
    await expect(b.done).resolves.toEqual({ ok: true });
    expect(calls).toEqual(["/x.pkg"]);
  });

  it("never auto-recovers an install item", async () => {
    useUploadSettingsStore.setState({ autoResume: true });
    registerInstallExecutor(async () => {
      throw new Error("connection reset");
    });
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "library", path: "/p.pkg" }, displayName: "P",
    });
    await expect(q.done).resolves.toMatchObject({ ok: false, message: "connection reset" });
    expect(useUploadQueueStore.getState().items.find((i) => i.id === q.id)?.status).toBe("failed");
  });

  it("sameInstall matches on request identity, not display name", () => {
    const item = {
      sourceKind: "install", addr: "10.0.0.2:9113",
      install: { via: "stream", source: "/x.pkg" }, status: "pending",
    } as unknown as QueueItem;
    expect(sameInstall(item, { host: "10.0.0.2", request: { via: "stream", source: "/x.pkg" }, displayName: "other" })).toBe(true);
    expect(sameInstall(item, { host: "10.0.0.3", request: { via: "stream", source: "/x.pkg" }, displayName: "X" })).toBe(false);
  });
});

describe("install item lifecycle", () => {
  const host = "10.0.0.2";
  beforeEach(() => {
    vi.useRealTimers();
    useUploadQueueStore.getState().stop();
    mockedEnsurePayload.mockReset().mockResolvedValue(undefined as never);
    useUploadQueueStore.setState({
      ...pristineQueue,
      items: [],
      runningHosts: {},
      running: false,
      continueOnFailure: true,
      loaded: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
  });

  it("[RF 1] removing a queued install resolves its waiter", async () => {
    registerInstallExecutor(() => new Promise(() => {})); // never finishes
    const s = useUploadQueueStore.getState();
    s.enqueueInstall({ host, request: { via: "stream", source: "/a.pkg" }, displayName: "A" });
    const b = s.enqueueInstall({ host, request: { via: "stream", source: "/b.pkg" }, displayName: "B" });
    useUploadQueueStore.getState().remove(b.id);
    await expect(b.done).resolves.toMatchObject({
      ok: false,
      message: expect.stringMatching(/removed from the queue/i),
    });
  });

  it("[RF 1] a queue stopped by a failure leaves later waiters pending", async () => {
    useUploadQueueStore.setState({ continueOnFailure: false });
    registerInstallExecutor(async (req) =>
      req.via === "stream" && req.source === "/a.pkg" ? { ok: false, message: "no" } : { ok: true },
    );
    const s = useUploadQueueStore.getState();
    const a = s.enqueueInstall({ host, request: { via: "stream", source: "/a.pkg" }, displayName: "A" });
    const b = s.enqueueInstall({ host, request: { via: "stream", source: "/b.pkg" }, displayName: "B" });
    await expect(a.done).resolves.toMatchObject({ ok: false });
    let settled = false;
    void b.done.then(() => (settled = true));
    await new Promise((r) => setTimeout(r, 50));
    expect(settled).toBe(false);
    expect(useUploadQueueStore.getState().items.find((i) => i.id === b.id)?.status).toBe("pending");
  });

  it("[RF 4] retryInstall re-runs one failed item with a fresh waiter", async () => {
    let n = 0;
    registerInstallExecutor(async () => (++n === 1 ? { ok: false, message: "first" } : { ok: true }));
    const first = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "library", path: "/p.pkg" }, displayName: "P",
    });
    await expect(first.done).resolves.toMatchObject({ ok: false });
    for (let i = 0; i < 200 && useUploadQueueStore.getState().runningHosts[host]; i++)
      await new Promise((r) => setTimeout(r, 5));
    const again = useUploadQueueStore.getState().retryInstall(first.id);
    expect(again).not.toBeNull();
    await expect(again!.done).resolves.toEqual({ ok: true });
  });

  it("never writes a link to disk", async () => {
    registerInstallExecutor(() => new Promise(() => {}));
    useUploadQueueStore.getState().enqueueInstall({
      host,
      request: { via: "link", url: "https://cdn.example/x.pkg?token=SECRET", mode: "stream", insecureTls: false },
      displayName: "X",
    });
    await new Promise((r) => setTimeout(r, 450)); // past the 300 ms save debounce
    const calls = mockedQueueSave.mock.calls;
    const saved = JSON.stringify(calls[calls.length - 1]?.[0]);
    expect(saved).not.toContain("SECRET");
    expect(saved).not.toContain("cdn.example");
  });

  it("hydrate: an interrupted install is failed (not re-run) and a link item is dropped", async () => {
    const base = {
      addr: "10.0.0.2:9113", strategy: "overwrite", reconcileMode: "fast", excludes: [],
      mountAfterUpload: false, mountReadOnly: true, registerAfterUpload: false,
      txIdHex: "00", bytesSent: 0, totalBytes: 0, bytesPerSec: 0, filesFinalized: 0,
      filesFinalizingTotal: 0, mountedAt: null, registeredAs: null, mountWarnings: [],
      error: null, errorReason: null, errorDetail: null, addedAt: 1, startedAt: 1, completedAt: null,
      resolvedDest: "",
    };
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [
        { ...base, id: "i1", sourceKind: "install", sourcePath: "ps5:/p.pkg", displayName: "P",
          install: { via: "library", path: "/p.pkg" }, status: "running" },
        { ...base, id: "i2", sourceKind: "install", sourcePath: "url:", displayName: "L", status: "pending" },
      ],
    } as never);
    vi.stubGlobal("window", { isTauri: true });
    try {
      await useUploadQueueStore.getState().hydrate();
    } finally {
      vi.unstubAllGlobals();
    }
    const items = useUploadQueueStore.getState().items;
    expect(items.map((i) => i.id)).toEqual(["i1"]);
    expect(items[0].status).toBe("failed");
    expect(items[0].error).toMatch(/interrupted/i);
  });

  describe("an install running when the page went away", () => {
    const saved = (jobId: string) => ({
      continueOnFailure: false,
      items: [
        {
          addr: "10.0.0.2:9113", strategy: "overwrite", reconcileMode: "fast", excludes: [],
          mountAfterUpload: false, mountReadOnly: true, registerAfterUpload: false,
          txIdHex: "00", bytesSent: 0, totalBytes: 0, bytesPerSec: 0, filesFinalized: 0,
          filesFinalizingTotal: 0, mountedAt: null, registeredAs: null, mountWarnings: [],
          error: null, errorReason: null, errorDetail: null, addedAt: 1, startedAt: 1,
          completedAt: null, resolvedDest: "", id: "i1", sourceKind: "install",
          sourcePath: "stream:/p.pkg", displayName: "P",
          install: { via: "stream", source: "/p.pkg" }, status: "running", installJobId: jobId,
        },
      ],
    });
    const hydrateWith = async (jobId: string) => {
      vi.mocked(uploadQueueLoad).mockResolvedValueOnce(saved(jobId) as never);
      vi.stubGlobal("window", { isTauri: true });
      try {
        await useUploadQueueStore.getState().hydrate();
      } finally {
        vi.unstubAllGlobals();
      }
      await new Promise((r) => setTimeout(r, 10));
      return useUploadQueueStore.getState().items[0];
    };

    it("shows as done when its engine job finished, and is never re-run", async () => {
      const exec = vi.fn(async () => ({ ok: true }));
      registerInstallExecutor(exec);
      const asked: string[] = [];
      registerInstallJobResolver(async (job) => {
        asked.push(job);
        return { state: "finished", result: { ok: true } };
      });
      const it1 = await hydrateWith("job-7");
      expect(asked).toEqual(["job-7"]);
      expect(it1.status).toBe("done");
      expect(it1.installPhase).toBe("done");
      expect(exec).not.toHaveBeenCalled();
    });

    it("says it was interrupted when the engine no longer knows the job", async () => {
      registerInstallJobResolver(async () => ({ state: "gone" }));
      const it1 = await hydrateWith("job-8");
      expect(it1.status).toBe("failed");
      expect(it1.error).toMatch(/interrupted/i);
    });

    it("carries the engine's reason when the job failed", async () => {
      registerInstallJobResolver(async () => ({
        state: "finished",
        result: { ok: false, message: "The PS5 refused it." },
      }));
      const it1 = await hydrateWith("job-9");
      expect(it1.status).toBe("failed");
      expect(it1.error).toBe("The PS5 refused it.");
    });
  });

  it("[RF 5] works without Tauri persistence (browser build)", async () => {
    registerInstallExecutor(async () => ({ ok: true }));
    vi.stubGlobal("window", {}); // a browser: no Tauri → in-memory only
    try {
      await useUploadQueueStore.getState().hydrate();
    } finally {
      vi.unstubAllGlobals();
    }
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/w.pkg" }, displayName: "W",
    });
    await expect(q.done).resolves.toEqual({ ok: true });
  });
});

describe("Retry via upload", () => {
  const host = "10.0.0.2";
  beforeEach(() => {
    vi.useRealTimers();
    useUploadQueueStore.getState().stop();
    mockedEnsurePayload.mockReset().mockResolvedValue(undefined as never);
    useUploadQueueStore.setState({
      ...pristineQueue,
      items: [],
      runningHosts: {},
      running: false,
      continueOnFailure: true,
      loaded: true,
    });
  });
  afterEach(() => useUploadQueueStore.getState().stop());

  it("offers it on a stream install the PS5 never fetched, and re-adds the file as an upload", async () => {
    registerInstallExecutor(async () => ({
      ok: false,
      message: "unreachable",
      stagedFallbackRecommended: true,
    }));
    // Accepts the file (onDest), as addAndUpload does once its checks pass.
    const addAndUpload = vi.fn(
      async (_p: string, _h: string, o?: { onDest?: (d: string) => void }) => o?.onDest?.("/dest"),
    );
    pkgLibraryStore(host).setState({ addAndUpload } as never);
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/games/a.pkg" }, displayName: "A",
    });
    await q.done;
    const failed = useUploadQueueStore.getState().items.find((i) => i.id === q.id)!;
    expect(failed.fallbackToUpload).toBe(true);
    const r = await useUploadQueueStore.getState().retryInstallViaUpload(q.id);
    expect(r.ok).toBe(true);
    expect(useUploadQueueStore.getState().items.find((i) => i.id === q.id)).toBeUndefined();
    expect(addAndUpload).toHaveBeenCalledWith("/games/a.pkg", host, expect.anything());
  });

  it("is refused for a link", async () => {
    registerInstallExecutor(async () => ({ ok: false, stagedFallbackRecommended: true }));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host,
      request: { via: "link", url: "https://x.example/y.pkg", mode: "stream", insecureTls: false },
      displayName: "Y",
    });
    await q.done;
    expect(useUploadQueueStore.getState().items.find((i) => i.id === q.id)?.fallbackToUpload).toBeFalsy();
    await expect(useUploadQueueStore.getState().retryInstallViaUpload(q.id)).resolves.toMatchObject({
      ok: false,
      message: expect.stringMatching(/only for a file on this computer/),
    });
  });

  it("is not offered when the PS5 did fetch (a different failure)", async () => {
    registerInstallExecutor(async () => ({ ok: false, message: "rejected" }));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/games/b.pkg" }, displayName: "B",
    });
    await q.done;
    expect(useUploadQueueStore.getState().items.find((i) => i.id === q.id)?.fallbackToUpload).toBeFalsy();
  });
});

describe("isUploadItem", () => {
  it("is false for an install item, which reports its own task", () => {
    expect(isUploadItem({ sourceKind: "install" } as QueueItem)).toBe(false);
    expect(isUploadItem({ sourceKind: "pkg" } as QueueItem)).toBe(true);
    expect(isUploadItem({ sourceKind: "folder" } as QueueItem)).toBe(true);
  });
});

describe("review fixes", () => {
  const host = "10.0.0.2";
  beforeEach(() => {
    vi.useRealTimers();
    useUploadQueueStore.getState().stop();
    mockedEnsurePayload.mockReset().mockResolvedValue(undefined as never);
    mockedQueueSave.mockClear();
    useUploadQueueStore.setState({
      ...pristineQueue,
      items: [],
      runningHosts: {},
      running: false,
      continueOnFailure: false,
      loaded: true,
    });
  });
  afterEach(() => useUploadQueueStore.getState().stop());
  const until = async (cond: () => boolean) => {
    for (let i = 0; i < 600 && !cond(); i++) await new Promise((r) => setTimeout(r, 5));
    expect(cond()).toBe(true);
  };
  const row = (id: string) => useUploadQueueStore.getState().items.find((i) => i.id === id);

  it("Stop during a running install leaves it running (never re-queued), and it runs once", async () => {
    let calls = 0;
    let finish!: (r: InstallResult) => void;
    registerInstallExecutor(() => {
      calls++;
      return new Promise((r) => (finish = r));
    });
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/a.pkg" }, displayName: "A",
    });
    await until(() => calls === 1);
    useUploadQueueStore.getState().stopHost(host);
    expect(row(q.id)?.status).toBe("running");
    void useUploadQueueStore.getState().startHost(host);
    await new Promise((r) => setTimeout(r, 50));
    expect(calls).toBe(1);
    finish({ ok: true });
    await expect(q.done).resolves.toEqual({ ok: true });
    await until(() => row(q.id)?.status === "done");
  });

  it("a stopped install that then fails is marked failed and its waiter settles", async () => {
    let finish!: (r: InstallResult) => void;
    registerInstallExecutor(() => new Promise((r) => (finish = r)));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/b.pkg" }, displayName: "B",
    });
    await until(() => row(q.id)?.status === "running");
    useUploadQueueStore.getState().stopHost(host);
    finish({ ok: false, message: "nope" });
    await expect(q.done).resolves.toMatchObject({ ok: false, message: "nope" });
    await until(() => row(q.id)?.status === "failed");
  });

  it("Clear keeps an install that is still running", async () => {
    registerInstallExecutor(() => new Promise(() => {}));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/c.pkg" }, displayName: "C",
    });
    await until(() => row(q.id)?.status === "running");
    useUploadQueueStore.getState().clear();
    expect(row(q.id)?.status).toBe("running");
  });

  it("a queue item added before the saved queue loads is kept, and nothing is saved before the load", async () => {
    useUploadQueueStore.setState({ loaded: false });
    registerInstallExecutor(() => new Promise(() => {}));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/d.pkg" }, displayName: "D",
    });
    await new Promise((r) => setTimeout(r, 400));
    expect(mockedQueueSave).not.toHaveBeenCalled();
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [{ ...(row(q.id) as QueueItem), id: "old", sourceKind: "file", sourcePath: "/old", status: "pending" }],
    } as never);
    vi.stubGlobal("window", { isTauri: true });
    try {
      await useUploadQueueStore.getState().hydrate();
    } finally {
      vi.unstubAllGlobals();
    }
    const ids = useUploadQueueStore.getState().items.map((i) => i.id);
    expect(ids).toContain("old");
    expect(ids).toContain(q.id);
    expect(row(q.id)?.status).toBe("running");
  });

  it("reconnecting never re-runs a failed install", async () => {
    useUploadSettingsStore.setState({ autoResume: true });
    registerInstallExecutor(async () => ({ ok: false, message: "connection refused" }));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "library", path: "/e.pkg" }, displayName: "E",
    });
    await q.done;
    await until(() => !useUploadQueueStore.getState().runningHosts[host]);
    const n = await useUploadQueueStore.getState().resumeFailedRecoverable(host);
    expect(n).toBe(0);
    expect(row(q.id)?.status).toBe("failed");
  });

  it("one failed install does not stop the installs after it", async () => {
    registerInstallExecutor(async (req) =>
      req.via === "library" && req.path === "/f1.pkg" ? { ok: false, message: "x" } : { ok: true },
    );
    const s = useUploadQueueStore.getState();
    const a = s.enqueueInstall({ host, request: { via: "library", path: "/f1.pkg" }, displayName: "F1" });
    const b = s.enqueueInstall({ host, request: { via: "library", path: "/f2.pkg" }, displayName: "F2" });
    await a.done;
    await expect(b.done).resolves.toEqual({ ok: true });
  }, 10_000);

  it("Retry via upload keeps the old dialog's options and removes the row only once accepted", async () => {
    registerInstallExecutor(async () => ({ ok: false, stagedFallbackRecommended: true }));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "stream", source: "/g.pkg" }, displayName: "G",
    });
    await q.done;
    await until(() => !useUploadQueueStore.getState().runningHosts[host]);
    // Refused: the row stays.
    pkgLibraryStore(host).setState({
      error: "Couldn't read .pkg header",
      addAndUpload: vi.fn(async () => {}),
    } as never);
    const refused = await useUploadQueueStore.getState().retryInstallViaUpload(q.id);
    expect(refused.ok).toBe(false);
    expect(row(q.id)).toBeDefined();
    // Accepted: options passed, row removed.
    const addAndUpload = vi.fn(
      async (_p: string, _h: string, o?: { onDest?: (d: string) => void }) => o?.onDest?.("/dest"),
    );
    pkgLibraryStore(host).setState({ error: null, addAndUpload } as never);
    const ok = await useUploadQueueStore.getState().retryInstallViaUpload(q.id);
    expect(ok.ok).toBe(true);
    expect(addAndUpload).toHaveBeenCalledWith(
      "/g.pkg",
      host,
      expect.objectContaining({ installAfterUpload: true, selectVariant: true }),
    );
    expect(row(q.id)).toBeUndefined();
  });

  it("rest mode is not triggered by installs or by an upload waiting for its install", async () => {
    useRestAfterUploadStore.setState({ enabled: true });
    mockedStandby.mockClear();
    registerInstallExecutor(async () => ({ ok: true }));
    const q = useUploadQueueStore.getState().enqueueInstall({
      host, request: { via: "library", path: "/h.pkg" }, displayName: "H",
    });
    await q.done;
    await until(() => !useUploadQueueStore.getState().runningHosts[host]);
    expect(mockedStandby).not.toHaveBeenCalled();
    useRestAfterUploadStore.setState({ enabled: false });
  });
});

describe("libraryInstallStates", () => {
  it("maps a console's queued and running library installs by path", () => {
    const it = (id: string, addr: string, path: string, status: string) =>
      ({ id, addr, sourceKind: "install", install: { via: "library", path }, status }) as unknown as QueueItem;
    const m = libraryInstallStates(
      [
        it("a", "10.0.0.2:9113", "/lib/a.pkg", "pending"),
        it("b", "10.0.0.2:9113", "/lib/b.pkg", "running"),
        it("c", "10.0.0.2:9113", "/lib/c.pkg", "done"),
        it("d", "10.0.0.3:9113", "/lib/d.pkg", "pending"),
      ],
      "10.0.0.2",
    );
    expect(m.get("/lib/a.pkg")).toBe("queued");
    expect(m.get("/lib/b.pkg")).toBe("installing");
    expect(m.has("/lib/c.pkg")).toBe(false);
    expect(m.has("/lib/d.pkg")).toBe(false);
  });
});

describe("failedStreamInstallIds", () => {
  it("finds only failed stream installs of that source", async () => {
    const { failedStreamInstallIds } = await import("./uploadQueue");
    const row = (id: string, status: string, source: string) =>
      ({ id, status, sourceKind: "install", install: { via: "stream", source } }) as never;
    const items = [
      row("a", "failed", "/staged/x.pkg"),
      row("b", "pending", "/staged/x.pkg"),
      row("c", "failed", "/staged/y.pkg"),
    ];
    expect(failedStreamInstallIds(items, "/staged/x.pkg")).toEqual(["a"]);
  });
});

describe("an archive on a saved server", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    mockedJobStatus.mockReset().mockResolvedValue({ status: "done", bytes_sent: 1 } as Awaited<ReturnType<typeof jobStatus>>);
    materializeRemote.mockReset().mockResolvedValue("/tmp/copies/g.zip");
    releaseCopy.mockClear();
    useUploadQueueStore.setState({ items: [], running: false, runningHosts: {}, continueOnFailure: false, loaded: true });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("is copied here when its turn comes, uploaded from the copy, and the copy removed", async () => {
    useUploadQueueStore.getState().add({
      sourceKind: "archive",
      sourcePath: "remote://nas-1/dl/g.zip",
      displayName: "g.zip",
      resolvedDest: "/data/homebrew/g",
      addr: "192.168.1.10:9113",
      strategy: "overwrite",
      reconcileMode: "fast",
      excludes: [],
      mountAfterUpload: false,
      mountReadOnly: true,
      registerAfterUpload: false,
    });
    expect(materializeRemote).not.toHaveBeenCalled();
    const p = useUploadQueueStore.getState().start();
    await vi.advanceTimersByTimeAsync(10_000);
    await p;
    expect(materializeRemote).toHaveBeenCalledWith("remote://nas-1/dl/g.zip");
    const { startTransferZip } = await import("../api/ps5");
    const calls = vi.mocked(startTransferZip).mock.calls;
    expect(calls[calls.length - 1]?.[0]).toBe("/tmp/copies/g.zip");
    expect(releaseCopy).toHaveBeenCalledWith("/tmp/copies/g.zip");
  });
});

