import { beforeEach, describe, expect, it, vi } from "vitest";

// The task store persists through `window.localStorage`; vitest's node env has no window.
const mem = new globalThis.Map<string, string>();
(globalThis as { window?: unknown }).window = {
  localStorage: {
    getItem: (k: string) => (mem.has(k) ? (mem.get(k) as string) : null),
    setItem: (k: string, v: string) => void mem.set(k, String(v)),
    removeItem: (k: string) => void mem.delete(k),
    clear: () => mem.clear(),
  },
  addEventListener: () => {},
  removeEventListener: () => {},
};

const { useTaskStore } = await import("./tasks");
const {
  useFsUploadStore,
  fsUploadForHost,
  runFsUpload,
  cancelFsUpload,
  resumeFsUpload,
  dismissFsUploadStopped,
  discardFsUploadPartial,
  giveUpFsUploadStopped,
  restoreFsUploads,
} = await import("./fsUpload");
type Deps = import("./fsUpload").FsUploadDeps;
type FsUploadStep = import("./fsUploadPlan").FsUploadStep;
type Snap = Awaited<ReturnType<Deps["jobStatus"]>>;

const HOST = "192.168.0.5";
const ADDR = `${HOST}:9113`;

/** A fake engine: each started job answers the snapshots queued for it, then "done". */
function fakeEngine(script: Record<string, Snap[]> = {}) {
  const started: { kind: "file" | "dir"; src: string; dest: string }[] = [];
  const caps: number[] = [];
  const cancelled: string[] = [];
  let gate: Promise<void> | null = null;
  const deps: Deps = {
    pathKind: async (p) =>
      p.endsWith("/") || p.includes("folder") ? "folder" : "file",
    startFile: async (src, dest, _addr, cap) => {
      started.push({ kind: "file", src, dest });
      caps.push(cap);
      return `job-${started.length}`;
    },
    startDir: async (src, dest, _addr, cap) => {
      started.push({ kind: "dir", src, dest });
      caps.push(cap);
      return `job-${started.length}`;
    },
    jobStatus: async (id) => {
      if (gate) await gate;
      const q = script[id];
      if (q && q.length > 0) return q.shift() as Snap;
      return { status: "done" } as Snap;
    },
    jobCancel: async (id) => {
      cancelled.push(id);
    },
    sleep: async () => {},
  };
  return {
    deps,
    started,
    caps,
    cancelled,
    /** Holds every status poll until the returned function is called. */
    hold() {
      let release!: () => void;
      gate = new Promise<void>((r) => (release = r));
      return () => {
        gate = null;
        release();
      };
    },
  };
}

const run = (
  e: ReturnType<typeof fakeEngine>,
  srcs: string[],
  replace?: string,
) =>
  runFsUpload(
    {
      host: HOST,
      addr: HOST,
      destDir: "/data/x",
      srcPaths: srcs,
      replaceRemoteName: replace,
    },
    e.deps,
  );

describe("runFsUpload", () => {
  beforeEach(() => {
    mem.clear();
    useTaskStore.setState({ tasks: [] });
    useFsUploadStore.setState({ byHost: {} });
  });

  it("keeps its progress in the store while it runs, with no screen mounted", async () => {
    const e = fakeEngine({
      "job-1": [
        { status: "running", bytes_sent: 40, total_bytes: 100 } as Snap,
      ],
    });
    const release = e.hold();
    const p = run(e, ["/pc/a.bin"]);
    await new Promise((r) => setTimeout(r, 0));
    const mid = fsUploadForHost(useFsUploadStore.getState(), HOST);
    expect(mid.active?.name).toBe("a.bin");
    expect(mid.active?.jobId).toBe("job-1");
    release();
    const r = await p;
    expect(r.ok).toBe(true);
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).active,
    ).toBeNull();
  });

  it("uploads every picked item in order, a folder as a folder", async () => {
    const e = fakeEngine();
    await run(e, ["/pc/a.bin", "/pc/folder1", "/pc/b.bin"]);
    expect(e.started).toEqual([
      { kind: "file", src: "/pc/a.bin", dest: "/data/x/a.bin" },
      { kind: "dir", src: "/pc/folder1", dest: "/data/x/folder1" },
      { kind: "file", src: "/pc/b.bin", dest: "/data/x/b.bin" },
    ]);
  });

  it("passes the upload speed limit to every job it starts, files and folders", async () => {
    const e = fakeEngine();
    // Read as each job starts, so a limit changed mid-batch applies to the next item.
    let reads = 0;
    e.deps.bandwidthCapMbps = () => ++reads * 10;
    await run(e, ["/pc/a.bin", "/pc/folder1"]);
    expect(e.caps).toEqual([10, 20]);
  });

  it("starts jobs with no limit when none is configured", async () => {
    const e = fakeEngine();
    await run(e, ["/pc/a.bin"]);
    expect(e.caps).toEqual([0]);
  });

  it("writes a replacement over the named file and never treats it as a folder", async () => {
    const e = fakeEngine();
    await run(e, ["/pc/folder-named.bin"], "eboot.bin");
    expect(e.started).toEqual([
      { kind: "file", src: "/pc/folder-named.bin", dest: "/data/x/eboot.bin" },
    ]);
  });

  it("shows as one task that ends done", async () => {
    const e = fakeEngine();
    await run(e, ["/pc/a.bin", "/pc/b.bin"]);
    const tasks = useTaskStore.getState().tasks;
    expect(tasks).toHaveLength(1);
    expect(tasks[0].status).toBe("done");
    expect(tasks[0].consoleId).toBe(HOST);
  });

  it("stops the batch at a failed file and keeps what is left, to resume", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "failed", error: "disk full" } as Snap],
    });
    const r = await run(e, ["/pc/a.bin", "/pc/b.bin"]);
    expect(r.ok).toBe(false);
    expect(e.started).toHaveLength(1);
    const s = fsUploadForHost(useFsUploadStore.getState(), HOST);
    expect(s.active).toBeNull();
    expect(s.stopped).toMatchObject({
      error: "disk full",
      destDir: "/data/x",
      srcPaths: ["/pc/a.bin", "/pc/b.bin"],
      doneCount: 0,
      count: 2,
    });
    expect(useTaskStore.getState().tasks[0].status).toBe("failed");
  });

  it("resume uploads only what is left, starting with the file that failed", async () => {
    const e = fakeEngine({
      "job-2": [{ status: "failed", error: "link dropped" } as Snap],
    });
    await run(e, ["/pc/a.bin", "/pc/b.bin", "/pc/c.bin"]);
    const r = await resumeFsUpload(HOST, e.deps);
    expect(r.ok).toBe(true);
    expect(e.started.map((x) => x.src)).toEqual([
      "/pc/a.bin",
      "/pc/b.bin",
      "/pc/b.bin",
      "/pc/c.bin",
    ]);
    const s = fsUploadForHost(useFsUploadStore.getState(), HOST);
    expect(s.stopped).toBeNull();
    expect(
      useTaskStore
        .getState()
        .tasks.map((t) => t.status)
        .sort(),
    ).toEqual(["done", "failed"]);
  });

  it("retries a failure the app can recover from by itself, the same file again", async () => {
    const e = fakeEngine({
      "job-1": [
        {
          status: "failed",
          error: "connection reset",
          error_reason: "ava1_unreachable",
        } as Snap,
      ],
    });
    const waits: number[] = [];
    const r = await runFsUpload(
      {
        host: HOST,
        addr: HOST,
        destDir: "/data/x",
        srcPaths: ["/pc/a.bin", "/pc/b.bin"],
      },
      {
        ...e.deps,
        shouldRetry: () => true,
        sleep: async (ms) => void waits.push(ms),
      },
    );
    expect(r.ok).toBe(true);
    expect(e.started.map((x) => x.src)).toEqual([
      "/pc/a.bin",
      "/pc/a.bin",
      "/pc/b.bin",
    ]);
    // The first retry's wait, in slices Cancel can end.
    expect(waits.reduce((a, b) => a + b, 0)).toBeGreaterThanOrEqual(5000);
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toBeNull();
  });

  it("Cancel ends the wait before a retry instead of leaving the row busy", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "failed", error: "connection reset" } as Snap],
    });
    const waits: number[] = [];
    const r = await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      {
        ...e.deps,
        shouldRetry: () => true,
        sleep: async (ms) => {
          waits.push(ms);
          if (waits.length === 3) cancelFsUpload(HOST, e.deps);
        },
      },
    );
    expect(r.ok).toBe(false);
    expect(waits.reduce((a, b) => a + b, 0)).toBeLessThan(5000);
    expect(e.started).toHaveLength(1);
  });

  it("shows that it is waiting to retry, and which try", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "failed", error: "connection reset" } as Snap],
    });
    let seen: unknown = null;
    await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      {
        ...e.deps,
        shouldRetry: () => true,
        sleep: async () => {
          seen ??=
            fsUploadForHost(useFsUploadStore.getState(), HOST).active?.retry ??
            null;
        },
      },
    );
    expect(seen).toEqual({ attempt: 1, of: 3 });
  });

  it("stops after three tries that land nothing, with the last error", async () => {
    const failing = (): Snap[] => [
      { status: "failed", error: "connection reset" } as Snap,
    ];
    const e = fakeEngine({
      "job-1": failing(),
      "job-2": failing(),
      "job-3": failing(),
      "job-4": failing(),
    });
    const r = await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      { ...e.deps, shouldRetry: () => true },
    );
    expect(r.ok).toBe(false);
    expect(e.started).toHaveLength(4);
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped?.error,
    ).toBe("connection reset");
  });

  it("a status read that fails keeps watching the same job, with no second job", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "running", bytes_sent: 1, total_bytes: 2 } as Snap],
    });
    let reads = 0;
    const r = await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      {
        ...e.deps,
        shouldRetry: () => true,
        jobStatus: async (id) => {
          if (++reads <= 2) throw new Error("fetch failed");
          return e.deps.jobStatus(id);
        },
      },
    );
    expect(r.ok).toBe(true);
    expect(e.started).toHaveLength(1);
  });

  it("a retry after status reads kept failing watches the job still running, not a second one", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "running", bytes_sent: 1, total_bytes: 2 } as Snap],
    });
    let reads = 0;
    const r = await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      {
        ...e.deps,
        shouldRetry: () => true,
        jobStatus: async (id) => {
          if (++reads <= 6) throw new Error("fetch failed");
          return e.deps.jobStatus(id);
        },
      },
    );
    expect(r.ok).toBe(true);
    expect(e.started).toHaveLength(1);
    expect(e.cancelled).toEqual([]);
  });

  it("a retry starts the step again once the engine no longer knows the last job", async () => {
    const e = fakeEngine();
    const r = await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      {
        ...e.deps,
        shouldRetry: () => true,
        jobStatus: async (id) => {
          if (id === "job-1") throw new Error("job not found");
          return e.deps.jobStatus(id);
        },
      },
    );
    expect(r.ok).toBe(true);
    expect(e.started.map((x) => x.src)).toEqual(["/pc/a.bin", "/pc/a.bin"]);
  });

  it("does not retry a failure a retry cannot fix", async () => {
    const e = fakeEngine({
      "job-1": [
        {
          status: "failed",
          error: "disk full",
          error_reason: "ava1_no_space",
        } as Snap,
      ],
    });
    let asked = "";
    await runFsUpload(
      { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      {
        ...e.deps,
        shouldRetry: (reason) => {
          asked = reason ?? "";
          return false;
        },
      },
    );
    expect(asked).toBe("ava1_no_space");
    expect(e.started).toHaveLength(1);
  });

  it("dismiss forgets a stopped upload", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "failed", error: "disk full" } as Snap],
    });
    await run(e, ["/pc/a.bin"]);
    dismissFsUploadStopped(HOST);
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toBeNull();
  });

  it("cancel ends the engine job and starts no further file", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "running", bytes_sent: 1, total_bytes: 9 } as Snap],
    });
    const release = e.hold();
    const p = run(e, ["/pc/a.bin", "/pc/b.bin"]);
    await new Promise((r) => setTimeout(r, 0));
    cancelFsUpload(HOST, e.deps);
    // The engine reports a cancelled job as failed: that is not an error to show.
    (e as unknown as { deps: Deps }).deps.jobStatus = async () =>
      ({ status: "failed", error: "cancelled" }) as Snap;
    release();
    const r = await p;
    expect(e.cancelled).toEqual(["job-1"]);
    expect(e.started).toHaveLength(1);
    expect(r.ok).toBe(false);
    // Stopped on purpose is still resumable: what is left, and no error to show.
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toMatchObject({
      why: "user",
      error: "",
      srcPaths: ["/pc/a.bin", "/pc/b.bin"],
      doneCount: 0,
      count: 2,
    });
  });

  it("a stopped run is saved, so it is still there after the app restarts", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "failed", error: "disk full" } as Snap],
    });
    await run(e, ["/pc/a.bin", "/pc/b.bin"]);
    useFsUploadStore.setState({ byHost: {} });
    restoreFsUploads();
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toMatchObject({
      why: "failed",
      error: "disk full",
      srcPaths: ["/pc/a.bin", "/pc/b.bin"],
    });
  });

  it("a run the app died in the middle of comes back as resumable", async () => {
    const e = fakeEngine({
      "job-2": [{ status: "running", bytes_sent: 1, total_bytes: 9 } as Snap],
    });
    const release = e.hold();
    // a.bin finishes, b.bin is mid-flight when the app goes away.
    let polls = 0;
    const realStatus = e.deps.jobStatus;
    e.deps.jobStatus = async (id) => {
      polls += 1;
      if (id === "job-1") return { status: "done" } as Snap;
      return realStatus(id);
    };
    void run(e, ["/pc/a.bin", "/pc/b.bin", "/pc/c.bin"]);
    for (let i = 0; i < 50 && e.started.length < 2; i++)
      await new Promise((r) => setTimeout(r, 1));
    expect(polls).toBeGreaterThan(0);
    // The app restarts: nothing in memory, only what was saved.
    useFsUploadStore.setState({ byHost: {} });
    restoreFsUploads();
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toMatchObject({
      why: "interrupted",
      srcPaths: ["/pc/b.bin", "/pc/c.bin"],
      doneCount: 1,
      count: 3,
      jobId: "job-2",
    });
    release();
  });

  it("resuming an interrupted run picks its engine job back up when that is still running", async () => {
    const e = fakeEngine({
      "job-7": [{ status: "running", bytes_sent: 5, total_bytes: 9 } as Snap],
    });
    useFsUploadStore.getState().patch(HOST, {
      stopped: {
        why: "interrupted",
        error: "",
        reason: null,
        addr: HOST,
        destDir: "/data/x",
        srcPaths: ["/pc/b.bin", "/pc/c.bin"],
        doneCount: 1,
        count: 3,
        name: "b.bin",
        jobId: "job-7",
      },
    });
    const r = await resumeFsUpload(HOST, e.deps);
    expect(r.ok).toBe(true);
    // b.bin was watched on its old job; only c.bin needed starting.
    expect(e.started.map((x) => x.src)).toEqual(["/pc/c.bin"]);
  });

  it("dismissing or finishing leaves nothing saved", async () => {
    const e = fakeEngine({
      "job-1": [{ status: "failed", error: "disk full" } as Snap],
    });
    await run(e, ["/pc/a.bin"]);
    dismissFsUploadStopped(HOST);
    useFsUploadStore.setState({ byHost: {} });
    restoreFsUploads();
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toBeNull();
  });

  it("refuses a second run on a console that already has one", async () => {
    const e = fakeEngine();
    const release = e.hold();
    const first = run(e, ["/pc/a.bin"]);
    await new Promise((r) => setTimeout(r, 0));
    const second = await run(e, ["/pc/b.bin"]);
    expect(second.ok).toBe(false);
    release();
    await first;
    expect(e.started).toHaveLength(1);
  });

  describe("a pick of several files as ONE file-list job", () => {
    const TX = "ab".repeat(16);
    const listStep = (names: string[]) => ({
      kind: "list" as const,
      txId: TX,
      files: names.map((n) => ({ src: `/pc/${n}`, name: n })),
    });
    /** The fake engine plus a file-list route that records each start. */
    function withList(e: ReturnType<typeof fakeEngine>) {
      const lists: { destRoot: string; files: { src: string; dest: string }[]; txId: string }[] = [];
      e.deps.startList = async (destRoot, files, _addr, txId) => {
        lists.push({ destRoot, files, txId });
        e.started.push({ kind: "file", src: `list:${files.length}`, dest: destRoot });
        return `job-${e.started.length}`;
      };
      return lists;
    }
    const runSteps = (
      e: ReturnType<typeof fakeEngine>,
      steps: import("./fsUploadPlan").FsUploadStep[],
    ) =>
      runFsUpload(
        { host: HOST, addr: HOST, destDir: "/data/x", srcPaths: [], steps },
        e.deps,
      );

    it("starts one job for the whole list, each file at its (possibly renamed) name", async () => {
      const e = fakeEngine();
      const lists = withList(e);
      const step = listStep(["a.bin", "b (2).bin"]);
      const r = await runSteps(e, [step]);
      expect(r.ok).toBe(true);
      expect(lists).toEqual([
        {
          destRoot: "/data/x",
          txId: TX,
          files: [
            { src: "/pc/a.bin", dest: "/data/x/a.bin" },
            { src: "/pc/b (2).bin", dest: "/data/x/b (2).bin" },
          ],
        },
      ]);
      const tasks = useTaskStore.getState().tasks;
      expect(tasks).toHaveLength(1);
      expect(tasks[0].status).toBe("done");
      expect(tasks[0].label).toBe("a.bin +1");
    });

    it("shows overall bytes, the file the sender is on, and how many are finished", async () => {
      const e = fakeEngine({
        "job-1": [
          {
            status: "running",
            bytes_sent: 30,
            total_bytes: 90,
            files_processing: 2,
            files_finalized: 1,
          } as Snap,
        ],
      });
      withList(e);
      const release = e.hold();
      const p = runSteps(e, [listStep(["c", "a", "b"])]);
      await new Promise((r) => setTimeout(r, 0));
      release();
      // One poll later the running snapshot has been applied.
      let seen: unknown = null;
      e.deps.sleep = async () => {
        seen = fsUploadForHost(useFsUploadStore.getState(), HOST).active;
      };
      await p;
      expect(seen).toMatchObject({ name: "b", index: 1, count: 3, sent: 30, total: 90 });
    });

    it("a failure keeps the list whole under its id, and says which files did not land", async () => {
      const e = fakeEngine({
        "job-1": [
          { status: "running", bytes_sent: 1, total_bytes: 9, files_count: 3 } as Snap,
          { status: "failed", error: "disk full", error_reason: "ava1_no_space" } as Snap,
        ],
      });
      const lists = withList(e);
      e.deps.jobFiles = async () => [
        { rel_path: "a", size: 1 },
        { rel_path: "b", size: 2 },
        { rel_path: "c", size: 3 },
      ];
      e.deps.listDir = async () => [
        { name: "a", kind: "file", size: 1 },
        { name: "b", kind: "file", size: 0 },
      ];
      const step = listStep(["a", "b", "c"]);
      const r = await runSteps(e, [step, { kind: "one", src: "/pc/after", name: "after" }]);
      expect(r.ok).toBe(false);
      const st = fsUploadForHost(useFsUploadStore.getState(), HOST).stopped;
      expect(st).toMatchObject({
        why: "failed",
        doneCount: 1,
        stepsDone: 0,
        count: 4,
        name: "b",
        notLanded: ["b", "c"],
      });
      expect(st?.steps?.[0]).toEqual(step);
      // Resume: the same list under the same id (the console's journal skips "a").
      await resumeFsUpload(HOST, e.deps);
      expect(lists.map((l) => l.txId)).toEqual([TX, TX]);
      expect(lists[1].files).toEqual(lists[0].files);
      expect(e.started.map((x) => x.src)).toEqual(["list:3", "list:3", "/pc/after"]);
    });

    it("a dropped connection retries the same list under the same id", async () => {
      const e = fakeEngine({
        "job-1": [{ status: "failed", error: "connection reset" } as Snap],
      });
      const lists = withList(e);
      e.deps.shouldRetry = () => true;
      const r = await runSteps(e, [listStep(["a", "b"])]);
      expect(r.ok).toBe(true);
      expect(lists.map((l) => l.txId)).toEqual([TX, TX]);
    });

    it("cancel ends the one job; the stop is resumable and names what is missing", async () => {
      const e = fakeEngine({
        "job-1": [{ status: "running", bytes_sent: 1, total_bytes: 9 } as Snap],
      });
      withList(e);
      e.deps.listDir = async () => [{ name: "a", kind: "file", size: 4 }];
      const release = e.hold();
      const p = runSteps(e, [listStep(["a", "b"])]);
      await new Promise((r) => setTimeout(r, 0));
      cancelFsUpload(HOST, e.deps);
      e.deps.jobStatus = async () => ({ status: "failed", error: "cancelled" }) as Snap;
      release();
      const r = await p;
      expect(r.ok).toBe(false);
      expect(e.cancelled).toEqual(["job-1"]);
      expect(fsUploadForHost(useFsUploadStore.getState(), HOST).stopped).toMatchObject({
        why: "user",
        error: "",
        doneCount: 1,
        notLanded: ["b"],
      });
    });

    it("an engine without the file-list route: the list goes file by file, names kept", async () => {
      const e = fakeEngine();
      e.deps.startList = async () => {
        throw new Error("engine HTTP 404 Not Found: (empty body)");
      };
      const r = await runSteps(e, [listStep(["a.bin", "b (2).bin"])]);
      expect(r.ok).toBe(true);
      expect(e.started).toEqual([
        { kind: "file", src: "/pc/a.bin", dest: "/data/x/a.bin" },
        { kind: "file", src: "/pc/b (2).bin", dest: "/data/x/b (2).bin" },
      ]);
      // Never asked whether a list file is a folder: it is a file by construction.
    });

    it("no file-list support wired at all: file by file too", async () => {
      const e = fakeEngine();
      const r = await runSteps(e, [listStep(["a", "b"])]);
      expect(r.ok).toBe(true);
      expect(e.started.map((x) => x.dest)).toEqual(["/data/x/a", "/data/x/b"]);
    });

    it("tells a missing route from a real error", async () => {
      const { listUnsupported } = await import("./fsUpload");
      expect(listUnsupported(new Error("engine HTTP 404 Not Found: (empty body)"))).toBe(true);
      expect(listUnsupported(new Error("engine HTTP 405"))).toBe(true);
      expect(listUnsupported(new Error("engine HTTP 500: helper said 404"))).toBe(false);
      expect(listUnsupported(new Error("command transfer_file_list not found"))).toBe(true);
      expect(listUnsupported(new Error("engine returned invalid JSON: expected value"))).toBe(false);
      expect(listUnsupported(new Error("source not found (404 on the server)"))).toBe(false);
      expect(listUnsupported(new Error("engine HTTP 400: files list is empty"))).toBe(false);
      expect(listUnsupported(new Error("console unreachable"))).toBe(false);
    });

    it("a real start error is not taken for a missing route", async () => {
      const e = fakeEngine();
      e.deps.startList = async () => {
        throw new Error("source not found: /pc/a");
      };
      const r = await runSteps(e, [listStep(["a", "b"])]);
      expect(r.ok).toBe(false);
      expect(e.started).toEqual([]);
    });

    it("an interrupted list comes back whole after a restart, and resumes on its job", async () => {
      const e = fakeEngine({
        "job-1": [{ status: "running", bytes_sent: 1, total_bytes: 9 } as Snap],
      });
      withList(e);
      const release = e.hold();
      void runSteps(e, [listStep(["a", "b"])]);
      await new Promise((r) => setTimeout(r, 0));
      useFsUploadStore.setState({ byHost: {} });
      restoreFsUploads();
      const st = fsUploadForHost(useFsUploadStore.getState(), HOST).stopped;
      expect(st).toMatchObject({ why: "interrupted", jobId: "job-1", count: 2 });
      expect(st?.steps?.[0]).toMatchObject({ kind: "list", txId: TX });
      release();
    });
  });
});

describe("Files uploads run under a job id of their own (R2-1 F25)", () => {
  beforeEach(() => {
    useFsUploadStore.setState({ byHost: {} });
    mem.clear();
  });
  /** A run stopped on its first step, the way the app saves it. */
  const stopped = () => fsUploadForHost(useFsUploadStore.getState(), HOST).stopped;

  function withIds() {
    const e = fakeEngine();
    const ids: (string | undefined)[] = [];
    let minted = 0;
    e.deps.newTxId = () => `tx-${++minted}`;
    const start = e.deps.startFile;
    e.deps.startFile = async (src, dest, addr, cap, txId) => {
      ids.push(txId);
      return start(src, dest, addr, cap, txId);
    };
    e.deps.startDir = e.deps.startFile;
    return { e, ids };
  }

  it("uses the planned id on every try and on a resume of a file", async () => {
    const { e, ids } = withIds();
    e.deps.shouldRetry = () => true;
    let fails = 1;
    const status = e.deps.jobStatus;
    e.deps.jobStatus = async (id) =>
      fails-- > 0 ? ({ status: "failed", error: "connection lost" } as Snap) : status(id);
    await runFsUpload(
      {
        host: HOST,
        addr: ADDR,
        destDir: "/data/x",
        srcPaths: [],
        steps: [{ kind: "one", src: "/pc/a.bin", name: "a.bin", txId: "planned" }],
      },
      e.deps,
    );
    expect(ids).toEqual(["planned", "planned"]);
  });

  it("mints an id for a step saved without one, and keeps it for the resume", async () => {
    const { e, ids } = withIds();
    const failing = e.deps.jobStatus;
    let fail = true;
    e.deps.jobStatus = async (id) =>
      fail ? ({ status: "failed", error: "disk full" } as Snap) : failing(id);
    await runFsUpload(
      { host: HOST, addr: ADDR, destDir: "/data/x", srcPaths: ["/pc/a.bin"] },
      e.deps,
    );
    expect(stopped()?.steps?.[0]).toMatchObject({ kind: "one", txId: "tx-1" });
    fail = false;
    await resumeFsUpload(HOST, e.deps);
    expect(ids).toEqual(["tx-1", "tx-1"]);
  });

  it("resumes a folder under its id while the staged tree is there, and starts over when it is gone", async () => {
    const run = async (listing: Record<string, { name: string; kind: string; size: number }[]>) => {
      const { e, ids } = withIds();
      e.deps.listDir = async (_a, dir) => {
        if (!listing[dir]) throw new Error("fs_list_dir_not_found");
        return listing[dir];
      };
      await runFsUpload(
        {
          host: HOST,
          addr: ADDR,
          destDir: "/data/x",
          srcPaths: [],
          steps: [{ kind: "one", src: "/pc/folder", name: "G", txId: "old" }],
          doneBefore: 0,
        },
        e.deps,
      );
      return ids;
    };
    const file = (name: string) => ({ name, kind: "dir", size: 0 });
    expect(await run({ "/data/x": [file("G"), file("G.ava-part")] })).toEqual(["old"]);
    expect(await run({ "/data/x": [file("G")], "/data/x/G": [] })).toEqual(["tx-1"]);
    expect(await run({})).toEqual(["old"]); // no answer: never a reason to start over
  });

  it("does not ask the console on a first run", async () => {
    const { e, ids } = withIds();
    e.deps.listDir = vi.fn(async () => []);
    await runFsUpload(
      {
        host: HOST,
        addr: ADDR,
        destDir: "/data/x",
        srcPaths: [],
        steps: [{ kind: "one", src: "/pc/folder", name: "G", txId: "first" }],
      },
      e.deps,
    );
    expect(ids).toEqual(["first"]);
    expect(e.deps.listDir).not.toHaveBeenCalled();
  });
});

describe("a list job's unreadable files (R2-1 F25)", () => {
  beforeEach(() => {
    useFsUploadStore.setState({ byHost: {} });
    mem.clear();
  });
  it("are kept for the screen, and a run that follows clears them", async () => {
    const e = fakeEngine();
    e.deps.startList = async () => ({ jobId: "list-1", unreadable: ["/pc/locked.bin"] });
    const steps: FsUploadStep[] = [
      {
        kind: "list",
        txId: "t",
        files: [
          { src: "/pc/a.bin", name: "a.bin" },
          { src: "/pc/locked.bin", name: "locked.bin" },
        ],
      },
    ];
    const r = await runFsUpload({ host: HOST, addr: ADDR, destDir: "/data/x", srcPaths: [], steps }, e.deps);
    expect(r.ok).toBe(true);
    const get = () => fsUploadForHost(useFsUploadStore.getState(), HOST).unreadable;
    expect(get()).toEqual(["/pc/locked.bin"]);
    e.deps.startList = async () => "list-2";
    await runFsUpload({ host: HOST, addr: ADDR, destDir: "/data/x", srcPaths: [], steps }, e.deps);
    expect(get()).toEqual([]);
  });
});

describe("giving a stopped run up (R2-1 F10)", () => {
  beforeEach(() => {
    useFsUploadStore.setState({ byHost: {} });
    mem.clear();
  });
  const part = "/data/x/G.ava-part";

  function console_(folders: Record<string, string[]>, files: string[] = []) {
    const removed: string[] = [];
    return {
      removed,
      listDir: vi.fn(async (_a: string, p: string) => {
        if (!(p in folders)) throw new Error("fs_list_dir_not_found");
        return folders[p].map((name) => ({ name, kind: "file", size: 1 }));
      }),
      deleteRemote: vi.fn(async (_a: string, p: string) => {
        if (!(p in folders) && !files.includes(p)) throw new Error("not found");
        removed.push(p);
        delete folders[p];
      }),
    };
  }
  const base = { addr: ADDR, destDir: "/data/x" };
  const deps = (c: ReturnType<typeof console_>, over: Partial<Deps> = {}) => ({
    ...fakeEngine().deps,
    listDir: c.listDir,
    deleteRemote: c.deleteRemote,
    ...over,
  });

  it("deletes a folder step's staging tree and its empty place-holder", async () => {
    const c = console_({ [part]: ["eboot.bin"], "/data/x/G": [] });
    await discardFsUploadPartial({ ...base, steps: [{ kind: "one", src: "/pc/G", name: "G" }] }, deps(c));
    expect(c.removed).toEqual([part, "/data/x/G"]);
  });

  it("deletes only the first step's partial: the others never started", async () => {
    const c = console_({}, ["/data/x/a.bin.ava-part", "/data/x/b.bin.ava-part"]);
    await discardFsUploadPartial(
      {
        ...base,
        steps: [
          { kind: "one", src: "/pc/a.bin", name: "a.bin" },
          { kind: "one", src: "/pc/b.bin", name: "b.bin" },
        ],
      },
      deps(c),
    );
    expect(c.removed).toEqual(["/data/x/a.bin.ava-part"]);
  });

  it("deletes a list step's parts only, by name, from one listing", async () => {
    const c = console_({ "/data/x": ["a.bin.ava-part", "b.bin", "other.ava-part"] }, [
      "/data/x/a.bin.ava-part",
    ]);
    await discardFsUploadPartial(
      {
        ...base,
        steps: [
          {
            kind: "list",
            txId: "t",
            files: [
              { src: "/pc/a.bin", name: "a.bin" },
              { src: "/pc/b.bin", name: "b.bin" },
            ],
          },
        ],
      },
      deps(c),
    );
    expect(c.removed).toEqual(["/data/x/a.bin.ava-part"]);
  });

  it("stops a job the closed app left running before deleting under it", async () => {
    const c = console_({}, ["/data/x/a.bin.ava-part"]);
    const order: string[] = [];
    let running = true;
    const d = deps(c, {
      jobStatus: async () => ({ status: running ? "running" : "failed" }) as Snap,
      jobCancel: async () => {
        order.push("cancel");
        running = false;
      },
    });
    c.deleteRemote.mockImplementation(async () => void order.push("delete"));
    await discardFsUploadPartial(
      { ...base, jobId: "j", steps: [{ kind: "one", src: "/pc/a.bin", name: "a.bin" }] },
      d,
    );
    expect(order).toEqual(["cancel", "delete"]);
  });

  it("the Dismiss button's call clears the card and deletes", async () => {
    const e = fakeEngine();
    e.deps.jobStatus = async () => ({ status: "failed", error: "disk full" }) as Snap;
    e.deps.newTxId = () => "t";
    await runFsUpload({ host: HOST, addr: ADDR, destDir: "/data/x", srcPaths: ["/pc/a.bin"] }, e.deps);
    expect(fsUploadForHost(useFsUploadStore.getState(), HOST).stopped).not.toBeNull();
    const c = console_({}, ["/data/x/a.bin.ava-part"]);
    giveUpFsUploadStopped(HOST, deps(c));
    expect(fsUploadForHost(useFsUploadStore.getState(), HOST).stopped).toBeNull();
    await vi.waitFor(() => expect(c.removed).toEqual(["/data/x/a.bin.ava-part"]));
  });
});
