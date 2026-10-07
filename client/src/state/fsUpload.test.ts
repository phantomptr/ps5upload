import { beforeEach, describe, expect, it } from "vitest";

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
  restoreFsUploads,
} = await import("./fsUpload");
type Deps = import("./fsUpload").FsUploadDeps;
type Snap = Awaited<ReturnType<Deps["jobStatus"]>>;

const HOST = "192.168.0.5";

/** A fake engine: each started job answers the snapshots queued for it, then "done". */
function fakeEngine(script: Record<string, Snap[]> = {}) {
  const started: { kind: "file" | "dir"; src: string; dest: string }[] = [];
  const cancelled: string[] = [];
  let gate: Promise<void> | null = null;
  const deps: Deps = {
    pathKind: async (p) =>
      p.endsWith("/") || p.includes("folder") ? "folder" : "file",
    startFile: async (src, dest) => {
      started.push({ kind: "file", src, dest });
      return `job-${started.length}`;
    },
    startDir: async (src, dest) => {
      started.push({ kind: "dir", src, dest });
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
    expect(waits).toContain(5000);
    expect(
      fsUploadForHost(useFsUploadStore.getState(), HOST).stopped,
    ).toBeNull();
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
        sleep: async (ms) => {
          if (ms >= 5000)
            seen = fsUploadForHost(useFsUploadStore.getState(), HOST).active
              ?.retry;
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
});
