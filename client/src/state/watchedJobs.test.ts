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
  useWatchedJobStore,
  watchedJob,
  watchJob,
  stopWatchedJob,
  dismissWatchedJob,
} = await import("./watchedJobs");
type Deps = import("./watchedJobs").WatchedJobDeps;
type Snap = Awaited<ReturnType<Deps["jobStatus"]>>;

const KEY = "linkdl:192.168.0.5:https://x/y.bin";
const INIT = {
  key: KEY,
  kind: "download" as const,
  origin: "install",
  label: "y.bin",
  host: "192.168.0.5",
};

function engine(snaps: Snap[]) {
  const cancelled: string[] = [];
  let gate: Promise<void> | null = null;
  const deps: Deps = {
    jobStatus: async () => {
      if (gate) await gate;
      return snaps.length > 0
        ? (snaps.shift() as Snap)
        : ({ status: "done" } as Snap);
    },
    jobCancel: async (id) => void cancelled.push(id),
    sleep: async () => {},
  };
  return {
    deps,
    cancelled,
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

const state = () => watchedJob(useWatchedJobStore.getState(), KEY);
const tick = () => new Promise((r) => setTimeout(r, 0));

describe("watchJob", () => {
  beforeEach(() => {
    useTaskStore.setState({ tasks: [] });
    useWatchedJobStore.setState({ byKey: {} });
  });

  it("is running, with the job id, before the job ends and with no screen mounted", async () => {
    const e = engine([
      { status: "running", bytes_sent: 5, total_bytes: 10 } as Snap,
    ]);
    const release = e.hold();
    const p = watchJob(INIT, async () => "job-9", e.deps);
    await tick();
    expect(state()?.phase).toBe("running");
    expect(state()?.jobId).toBe("job-9");
    release();
    await p;
  });

  it("ends done with where the file landed and the bytes moved", async () => {
    const e = engine([
      { status: "running", bytes_sent: 5, total_bytes: 10 } as Snap,
      {
        status: "done",
        bytes_sent: 10,
        total_bytes: 10,
        dest: "/data/y.bin",
      } as Snap,
    ]);
    await watchJob(INIT, async () => "job-9", e.deps);
    expect(state()).toMatchObject({
      phase: "done",
      sent: 10,
      total: 10,
      dest: "/data/y.bin",
    });
    expect(useTaskStore.getState().tasks[0].status).toBe("done");
  });

  it("ends failed with the engine's error", async () => {
    const e = engine([{ status: "failed", error: "no route" } as Snap]);
    await watchJob(INIT, async () => "job-9", e.deps);
    expect(state()).toMatchObject({ phase: "failed", error: "no route" });
    expect(useTaskStore.getState().tasks[0].status).toBe("failed");
  });

  it("ends failed when the job cannot be started", async () => {
    const e = engine([]);
    await watchJob(
      INIT,
      async () => {
        throw new Error("engine unreachable");
      },
      e.deps,
    );
    expect(state()).toMatchObject({
      phase: "failed",
      error: "engine unreachable",
    });
  });

  it("stop cancels the engine job and the run ends stopped, not failed", async () => {
    const e = engine([
      { status: "running", bytes_sent: 1, total_bytes: 9 } as Snap,
    ]);
    const release = e.hold();
    const p = watchJob(INIT, async () => "job-9", e.deps);
    await tick();
    stopWatchedJob(KEY, e.deps);
    e.deps.jobStatus = async () =>
      ({ status: "failed", error: "cancelled" }) as Snap;
    release();
    await p;
    expect(e.cancelled).toEqual(["job-9"]);
    expect(state()?.phase).toBe("stopped");
  });

  it("does not start the same key twice while it runs", async () => {
    const e = engine([]);
    const release = e.hold();
    let starts = 0;
    const start = async () => `job-${++starts}`;
    const first = watchJob(INIT, start, e.deps);
    await tick();
    await watchJob(INIT, start, e.deps);
    release();
    await first;
    expect(starts).toBe(1);
  });

  it("can run without a task of its own and reports progress and the end to its caller", async () => {
    const e = engine([
      { status: "running", bytes_sent: 5, total_bytes: 10 } as Snap,
      {
        status: "done",
        bytes_sent: 10,
        total_bytes: 10,
        dest: "/pc/y",
      } as Snap,
    ]);
    const seen: string[] = [];
    await watchJob({ ...INIT, track: false }, async () => "job-9", e.deps, {
      onProgress: (sent, total) => seen.push(`${sent}/${total}`),
      onEnd: (job) => seen.push(`end:${job.phase}:${job.dest}`),
    });
    expect(useTaskStore.getState().tasks).toHaveLength(0);
    expect(seen).toEqual(["5/10", "end:done:/pc/y"]);
  });

  it("tells its caller about a failure too", async () => {
    const e = engine([{ status: "failed", error: "boom" } as Snap]);
    let end = "";
    await watchJob(INIT, async () => "job-9", e.deps, {
      onEnd: (job) => (end = `${job.phase}:${job.error}`),
    });
    expect(end).toBe("failed:boom");
  });

  it("dismiss forgets a finished run and leaves a running one alone", async () => {
    const e = engine([]);
    const release = e.hold();
    const p = watchJob(INIT, async () => "job-9", e.deps);
    await tick();
    dismissWatchedJob(KEY);
    expect(state()?.phase).toBe("running");
    release();
    await p;
    dismissWatchedJob(KEY);
    expect(state()).toBeNull();
  });
});
