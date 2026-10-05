// Final review #7: two open web-UI tabs must not both run the queue, and a fresh upload must
// never start for an item whose engine job is still running.
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
  };
});

import {
  jobCancel,
  jobStatus,
  startTransferFile,
  uploadQueueLoad,
  uploadQueueSave,
} from "../api/ps5";
import { LEADER_KEY } from "../lib/queueLeader";
import { useUploadQueueStore } from "./uploadQueue";

const base = {
  addr: "10.0.0.2:9120",
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

/** A localStorage that holds the lease another tab wrote `ageMs` ago. */
function browserWithOtherTab(ageMs: number | null) {
  const m = new Map<string, string>();
  if (ageMs !== null) {
    m.set(LEADER_KEY, JSON.stringify({ id: "the-other-tab", ts: Date.now() - ageMs }));
  }
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  });
  return m;
}

describe("only one tab runs the queue", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("window", {}); // a browser: no Tauri
    useUploadQueueStore.setState({
      items: [],
      running: false,
      runningHosts: {},
      loaded: false,
      isLeader: true,
    });
  });
  afterEach(() => {
    useUploadQueueStore.getState().stop();
    vi.unstubAllGlobals();
  });

  it("a second tab shows the queue read-only: it adopts nothing, starts nothing and saves nothing", async () => {
    browserWithOtherTab(500); // the other tab's lease is fresh
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [
        { ...base, id: "run", status: "running", jobId: "J1", bytesSent: 5, totalBytes: 10 },
        { ...base, id: "wait", status: "pending", sourcePath: "/src/b.bin" },
      ],
    } as never);

    const s = useUploadQueueStore.getState();
    await s.hydrate();

    const st = useUploadQueueStore.getState();
    expect(st.isLeader).toBe(false);
    expect(st.loaded).toBe(true);
    // shown as the runner saved it: still running, with its progress
    expect(row("run")?.status).toBe("running");
    expect(row("run")?.bytesSent).toBe(5);
    expect(st.runningHosts["10.0.0.2"]).toBe(true);

    // actions do nothing
    await st.startHost("10.0.0.2");
    await st.start();
    st.stop();
    st.remove("wait");
    st.clear();
    st.add({ ...base, id: "x" } as never);
    const r = st.enqueueInstall({ host: "10.0.0.2" } as never);
    expect((await r.done).ok).toBe(false);
    expect(st.retryItem("run")).toBe(false);
    await new Promise((r) => setTimeout(r, 400)); // past the save debounce
    expect(useUploadQueueStore.getState().items.map((i) => i.id)).toEqual(["run", "wait"]);
    expect(startTransferFile).not.toHaveBeenCalled();
    expect(jobStatus).not.toHaveBeenCalled();
    expect(jobCancel).not.toHaveBeenCalled();
    expect(uploadQueueSave).not.toHaveBeenCalled();
  });

  it("takes the queue over when the other tab's lease has expired, and adopts the running job", async () => {
    browserWithOtherTab(60_000);
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [{ ...base, id: "a", status: "running", jobId: "J1" }],
    } as never);
    let polls = 0;
    vi.mocked(jobStatus).mockImplementation((async () => {
      polls += 1;
      return polls < 3
        ? { status: "running", bytes_sent: 5, total_bytes: 10 }
        : { status: "done", bytes_sent: 10, dest: "/data/big.bin" };
    }) as never);

    await useUploadQueueStore.getState().hydrate();
    expect(useUploadQueueStore.getState().isLeader).toBe(true);
    await until(() => row("a")?.status === "done");
    expect(startTransferFile).not.toHaveBeenCalled();
  });

  it("never starts a fresh upload while the engine job of a reloaded item is still being looked up", async () => {
    browserWithOtherTab(null);
    vi.mocked(uploadQueueLoad).mockResolvedValueOnce({
      continueOnFailure: false,
      items: [{ ...base, id: "a", status: "running", jobId: "J1" }],
    } as never);
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    let polls = 0;
    vi.mocked(jobStatus).mockImplementation((async () => {
      polls += 1;
      if (polls === 1) await gate; // the hydrate probe is slow
      return polls < 4 ? { status: "running" } : { status: "done", dest: "/data/big.bin" };
    }) as never);

    await useUploadQueueStore.getState().hydrate();
    // Something else starts the console (the package bridge, a retry) during the probe.
    await useUploadQueueStore.getState().startHost("10.0.0.2");
    expect(startTransferFile).not.toHaveBeenCalled();
    expect(row("a")?.status).toBe("pending");

    release();
    await until(() => row("a")?.status === "done");
    expect(startTransferFile).not.toHaveBeenCalled();
    expect(row("a")?.jobId).toBe("J1");
  });

  it("adopts a still-running job of an item that is started again, instead of uploading twice", async () => {
    browserWithOtherTab(null);
    useUploadQueueStore.setState({ loaded: true });
    useUploadQueueStore.setState({
      items: [{ ...base, id: "p", status: "pending", jobId: "OLD" }] as never,
    });
    let polls = 0;
    vi.mocked(jobStatus).mockImplementation((async (id: string) => {
      expect(id).toBe("OLD");
      polls += 1;
      return polls < 3 ? { status: "running" } : { status: "done", dest: "/data/big.bin" };
    }) as never);
    await useUploadQueueStore.getState().startHost("10.0.0.2");
    await until(() => row("p")?.status === "done");
    expect(startTransferFile).not.toHaveBeenCalled();
  });

  it("starts fresh when the earlier job is over or unknown", async () => {
    browserWithOtherTab(null);
    useUploadQueueStore.setState({
      loaded: true,
      items: [{ ...base, id: "q", status: "pending", jobId: "DEAD" }] as never,
    });
    vi.mocked(jobStatus).mockImplementation((async (id: string) => {
      if (id === "DEAD") return { status: "failed", error: "x" };
      return { status: "done", dest: "/data/big.bin" };
    }) as never);
    await useUploadQueueStore.getState().startHost("10.0.0.2");
    await until(() => row("q")?.status === "done");
    expect(startTransferFile).toHaveBeenCalledTimes(1);
    expect(row("q")?.jobId).toBe("fresh-job");
  });
});
