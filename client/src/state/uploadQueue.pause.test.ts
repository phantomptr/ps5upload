import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Pause / Resume / Send now on the Upload & Install queue. Same engine mocks as
// uploadQueue.test.ts: the runner is driven by jobStatus answers, never a real engine.
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
    startTransferFile: vi.fn(async () => "job"),
    jobStatus: vi.fn(async () => ({ status: "running" })),
    jobCancel: vi.fn(async () => {}),
    powerStandby: vi.fn(async () => ({ ok: true })),
  };
});

import { jobCancel, jobStatus, startTransferFile, uploadQueueLoad } from "../api/ps5";
import {
  canPauseItem,
  distinctPendingHosts,
  nextPendingForHost,
  useUploadQueueStore,
  type AddQueueItem,
  type QueueItem,
} from "./uploadQueue";
import { useUploadSettingsStore } from "./uploadSettings";

const HOST = "192.168.1.10";
const mockedJobStatus = vi.mocked(jobStatus);
const mockedStartFile = vi.mocked(startTransferFile);
type Snap = Awaited<ReturnType<typeof jobStatus>>;

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

function addItem(name: string, addr = `${HOST}:9113`): string {
  const input: AddQueueItem = {
    sourceKind: "file",
    sourcePath: `/src/${name}`,
    displayName: name,
    resolvedDest: `/data/${name}`,
    addr,
    strategy: "resume",
    reconcileMode: "fast",
    excludes: [],
    mountAfterUpload: false,
    mountReadOnly: false,
    registerAfterUpload: false,
  };
  return useUploadQueueStore.getState().add(input);
}

const byName = (name: string) =>
  useUploadQueueStore.getState().items.find((i) => i.displayName === name)!;
const running = () =>
  useUploadQueueStore.getState().items.filter((i) => i.status === "running").map((i) => i.displayName);

/** Jobs are named after their file; each runs until `finish` holds its name. A cancelled job
 *  reads "failed" from then on, the way the engine ends it. */
const finish = new Set<string>();
const failing = new Set<string>();
function engine() {
  mockedStartFile.mockReset().mockImplementation(async (src: string) => `job-${src.split("/").pop()}`);
  mockedJobStatus.mockReset().mockImplementation(async (jobId: string) => {
    const name = jobId.replace(/^job-/, "");
    if (vi.mocked(jobCancel).mock.calls.some((c) => c[0] === jobId))
      return { status: "failed", error: "cancelled" } as Snap;
    if (failing.has(name)) return { status: "failed", error: "disk full" } as Snap;
    return { status: finish.has(name) ? "done" : "running", bytes_sent: 1, total_bytes: 10 } as Snap;
  });
}
/** Names of the files whose upload was started, in order. */
const started = () => mockedStartFile.mock.calls.map((c) => String(c[0]).split("/").pop());
/** A job that ends after being cancelled must not answer "cancelled" again when started anew. */
function forgetCancels() {
  vi.mocked(jobCancel).mockClear();
}

describe("pure picking", () => {
  const item = (id: string, extra: Partial<QueueItem> = {}): QueueItem =>
    ({ id, addr: HOST, status: "pending", sourceKind: "file", resolvedDest: `/data/${id}`, ...extra }) as QueueItem;

  it("skips a paused row", () => {
    const items = [item("a", { paused: "user" }), item("b")];
    expect(nextPendingForHost(items, HOST)?.id).toBe("b");
    expect(nextPendingForHost([item("a", { paused: "sendNow" })], HOST)).toBeNull();
  });

  it("runs rows sent now first, in click order", () => {
    const items = [item("a"), item("b", { sendNowAt: 20 }), item("c", { sendNowAt: 10 })];
    expect(nextPendingForHost(items, HOST)?.id).toBe("c");
  });

  it("a console with only paused rows has nothing to start", () => {
    expect(distinctPendingHosts([item("a", { paused: "user" })])).toEqual([]);
  });
});

describe("pause / resume / send now", () => {
  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    finish.clear();
    failing.clear();
    // A failed row stays failed here (no automatic retry of the connection kind).
    useUploadSettingsStore.setState({ autoResume: false });
    forgetCancels();
    engine();
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      limitedHosts: {},
      continueOnFailure: false,
      loaded: true,
      isLeader: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("Pause stops the running upload, keeps its job id and runs the next row", async () => {
    addItem("big");
    addItem("small");
    const tx = byName("big").txIdHex;
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    expect(running()).toEqual(["big"]);

    useUploadQueueStore.getState().pauseItem(byName("big").id);
    expect(byName("big").paused).toBe("user");
    expect(byName("big").status).toBe("pending");
    expect(byName("big").txIdHex).toBe(tx);
    expect(vi.mocked(jobCancel)).toHaveBeenCalledWith("job-big");

    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
    finish.add("small");
    await vi.advanceTimersByTimeAsync(3_000);
    // The paused row is never picked: the console goes idle.
    expect(byName("small").status).toBe("done");
    expect(byName("big").status).toBe("pending");
    expect(useUploadQueueStore.getState().runningHosts).toEqual({});
    expect(started()).toEqual(["big", "small"]);
  });

  it("Resume on an idle console starts the row at once, under the same tx id", async () => {
    addItem("big");
    const tx = byName("big").txIdHex;
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().pauseItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual([]);
    forgetCancels();

    useUploadQueueStore.getState().resumeItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(50);
    expect(byName("big").paused).toBeNull();
    expect(running()).toEqual(["big"]);
    // Same tx id: the console's journal skips what it already has.
    const calls = mockedStartFile.mock.calls;
    expect(calls[calls.length - 1]?.[3]).toBe(tx);
  });

  it("Resume while another row runs puts it back in line at its place", async () => {
    addItem("big");
    addItem("small");
    addItem("later");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().pauseItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
    forgetCancels();

    useUploadQueueStore.getState().resumeItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(50);
    // Still one transfer at a time: small keeps going.
    expect(running()).toEqual(["small"]);
    finish.add("small");
    await vi.advanceTimersByTimeAsync(3_000);
    // big is ahead of later in the list, so it goes next.
    expect(running()).toEqual(["big"]);
  });

  it("Send now pauses the running upload, runs the row, then resumes the paused one by itself", async () => {
    addItem("big");
    addItem("other");
    addItem("small");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    expect(running()).toEqual(["big"]);

    useUploadQueueStore.getState().sendNow(byName("small").id);
    expect(byName("big").paused).toBe("sendNow");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
    forgetCancels();

    finish.add("small");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(byName("small").status).toBe("done");
    expect(byName("small").sendNowAt).toBeNull();
    expect(byName("big").paused).toBeNull();
    // big resumes before other: it is ahead of it in line.
    expect(running()).toEqual(["big"]);
  });

  it("Send now resumes the paused upload even when the sent row fails", async () => {
    addItem("big");
    addItem("small");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().sendNow(byName("small").id);
    await vi.advanceTimersByTimeAsync(3_000);
    forgetCancels();
    failing.add("small");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(byName("small").status).toBe("failed");
    expect(running()).toEqual(["big"]);
  });

  it("stacked Send nows run in click order; the paused upload resumes after the last", async () => {
    addItem("big");
    addItem("one");
    addItem("two");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().sendNow(byName("two").id);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["two"]);
    forgetCancels();
    // The second click does not stop "two" (it was sent now too); it goes after it.
    useUploadQueueStore.getState().sendNow(byName("one").id);
    await vi.advanceTimersByTimeAsync(50);
    expect(running()).toEqual(["two"]);
    expect(vi.mocked(jobCancel)).not.toHaveBeenCalled();

    finish.add("two");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["one"]);
    expect(byName("big").paused).toBe("sendNow");
    finish.add("one");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["big"]);
    expect(byName("big").paused).toBeNull();
  });

  it("a row the person paused is never resumed by a Send now", async () => {
    addItem("big");
    addItem("small");
    addItem("next");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().pauseItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
    forgetCancels();
    useUploadQueueStore.getState().sendNow(byName("next").id);
    // small (not sent now) is paused by the Send now; big stays the person's pause.
    expect(byName("small").paused).toBe("sendNow");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["next"]);
    forgetCancels();
    finish.add("next");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
    expect(byName("big").paused).toBe("user");
    finish.add("small");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(byName("big").status).toBe("pending");
    expect(byName("big").paused).toBe("user");
  });

  it("Send now on an idle console just runs the row", async () => {
    addItem("a");
    addItem("b");
    useUploadQueueStore.getState().sendNow(byName("b").id);
    await vi.advanceTimersByTimeAsync(50);
    expect(running()).toEqual(["b"]);
  });

  it("cancelling the row sent now resumes the upload it paused", async () => {
    addItem("big");
    addItem("small");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().sendNow(byName("small").id);
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
    useUploadQueueStore.getState().cancelItem(byName("small").id);
    expect(byName("big").paused).toBeNull();
    forgetCancels();
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["big"]);
  });

  it("Pause and Send now leave a pkg that is installing alone", async () => {
    addItem("big");
    addItem("small");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    // Its upload has finished and Sony is installing it.
    useUploadQueueStore.setState((s) => ({
      items: s.items.map((it) =>
        it.displayName === "big" ? { ...it, sourceKind: "pkg", installPhase: "installing" } : it,
      ),
    }));
    expect(canPauseItem(byName("big"))).toBe(false);
    useUploadQueueStore.getState().pauseItem(byName("big").id);
    expect(byName("big").paused ?? null).toBeNull();
    expect(byName("big").status).toBe("running");
    useUploadQueueStore.getState().sendNow(byName("small").id);
    expect(byName("big").paused ?? null).toBeNull();
    expect(vi.mocked(jobCancel)).not.toHaveBeenCalled();
    // small goes right after it.
    finish.add("big");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["small"]);
  });

  it("an upload whose job has finished can no longer be paused", () => {
    const row = { status: "running", sourceKind: "file", installPhase: null } as const;
    expect(canPauseItem(row)).toBe(true);
    expect(canPauseItem({ ...row, uploadDone: true })).toBe(false);
    expect(canPauseItem({ ...row, sourceKind: "install" })).toBe(false);
    expect(canPauseItem({ ...row, status: "pending" })).toBe(false);
  });

  it("paused rows survive a reload", async () => {
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      items: [
        { ...savedItem("big"), paused: "user", interrupted: true },
        { ...savedItem("waits"), paused: "sendNow" },
        { ...savedItem("sent"), status: "running", sendNowAt: 5 },
      ],
      continueOnFailure: false,
    });
    vi.useRealTimers();
    useUploadQueueStore.setState({ loaded: false });
    vi.stubGlobal("window", { isTauri: true });
    try {
      await useUploadQueueStore.getState().hydrate();
    } finally {
      vi.unstubAllGlobals();
    }
    expect(byName("big").paused).toBe("user");
    expect(byName("waits").paused).toBe("sendNow");
    // The row that was running when the app closed waits, still first in line.
    expect(byName("sent").status).toBe("pending");
    expect(nextPendingForHost(useUploadQueueStore.getState().items, HOST)?.displayName).toBe("sent");
  });
});

function savedItem(name: string): QueueItem {
  return {
    id: `id-${name}`,
    sourceKind: "file",
    sourcePath: `/src/${name}`,
    displayName: name,
    resolvedDest: `/data/${name}`,
    addr: HOST,
    strategy: "resume",
    reconcileMode: "fast",
    excludes: [],
    mountAfterUpload: false,
    mountReadOnly: false,
    registerAfterUpload: false,
    txIdHex: `tx-${name}`,
    status: "pending",
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
    startedAt: null,
    completedAt: null,
  };
}

describe("pause / send now while the stopped upload is still ending", () => {
  /** Engine whose cancelled jobs keep running for SLOW ms, the way a real one ends at its next
   *  shard boundary. Counts every start made while another started job was still running. */
  const SLOW = 4_000;
  const cancelledAt = new Map<string, number>();
  let overlaps: string[] = [];
  const statusOf = (jobId: string): string => {
    const name = jobId.replace(/^job-/, "");
    const at = cancelledAt.get(jobId);
    if (at !== undefined) return Date.now() < at + SLOW ? "running" : "failed";
    return finish.has(name) ? "done" : "running";
  };
  const live = new Set<string>();

  beforeEach(() => {
    installLocalStorageStub();
    vi.useFakeTimers();
    finish.clear();
    failing.clear();
    cancelledAt.clear();
    live.clear();
    overlaps = [];
    useUploadSettingsStore.setState({ autoResume: false });
    vi.mocked(jobCancel)
      .mockReset()
      .mockImplementation(async (jobId: string) => {
        if (!cancelledAt.has(jobId)) cancelledAt.set(jobId, Date.now());
      });
    mockedStartFile.mockReset().mockImplementation(async (src: string) => {
      const jobId = `job-${src.split("/").pop()}`;
      for (const j of live) if (statusOf(j) === "running") overlaps.push(`${jobId} beside ${j}`);
      // A new job under the same name: not the cancelled one.
      cancelledAt.delete(jobId);
      live.add(jobId);
      return jobId;
    });
    mockedJobStatus.mockReset().mockImplementation(async (jobId: string) => {
      const status = statusOf(jobId);
      if (status !== "running") live.delete(jobId);
      return { status, bytes_sent: 1, total_bytes: 10 } as Snap;
    });
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      limitedHosts: {},
      continueOnFailure: false,
      loaded: true,
      isLeader: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.useRealTimers();
  });

  it("stacked Send nows wait for the stopped upload, run in click order, then resume it", async () => {
    addItem("big");
    addItem("one");
    addItem("two");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    expect(running()).toEqual(["big"]);

    useUploadQueueStore.getState().sendNow(byName("two").id);
    await vi.advanceTimersByTimeAsync(100);
    // big is still ending on the console: the second click must not start a transfer now.
    useUploadQueueStore.getState().sendNow(byName("one").id);
    await vi.advanceTimersByTimeAsync(100);
    expect(started()).toEqual(["big"]);

    await vi.advanceTimersByTimeAsync(SLOW + 2_000);
    expect(running()).toEqual(["two"]);
    finish.add("two");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["one"]);
    expect(byName("big").paused).toBe("sendNow");
    finish.add("one");
    await vi.advanceTimersByTimeAsync(3_000);
    expect(running()).toEqual(["big"]);
    expect(byName("big").paused).toBeNull();
    expect(started()).toEqual(["big", "two", "one", "big"]);
    expect(overlaps).toEqual([]);
  });

  it("a Send now in a limited run joins the restart instead of starting at once", async () => {
    addItem("big");
    addItem("other");
    addItem("next");
    void useUploadQueueStore.getState().startHost(HOST, { onlyIds: [byName("big").id] });
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().pauseItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(100);
    useUploadQueueStore.getState().sendNow(byName("next").id);
    await vi.advanceTimersByTimeAsync(100);
    expect(started()).toEqual(["big"]);
    await vi.advanceTimersByTimeAsync(SLOW + 2_000);
    expect(running()).toEqual(["next"]);
    expect(overlaps).toEqual([]);
  });

  it("a quick Pause then Resume waits for the old job before running it again", async () => {
    addItem("big");
    addItem("small");
    void useUploadQueueStore.getState().startHost(HOST);
    await vi.advanceTimersByTimeAsync(50);
    useUploadQueueStore.getState().pauseItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(100);
    useUploadQueueStore.getState().resumeItem(byName("big").id);
    await vi.advanceTimersByTimeAsync(100);
    expect(started()).toEqual(["big"]);
    expect(running()).toEqual([]);

    await vi.advanceTimersByTimeAsync(SLOW + 2_000);
    // big is ahead of small in line.
    expect(running()).toEqual(["big"]);
    finish.add("big");
    await vi.advanceTimersByTimeAsync(3_000);
    // The console's whole run went on: small was not left behind.
    expect(running()).toEqual(["small"]);
    expect(overlaps).toEqual([]);
  });
});
