import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import {
  cancelJob,
  createJobsPoller,
  fetchJobsAll,
  JOBS_FAST_MS,
  JOBS_SLOW_MS,
  nextJobsDelay,
  SMB_DONE_KEEP_S,
  smbJobToJob,
  type Job,
} from "./useJobs";

const mockCall = vi.mocked(call);

const job = (state: Job["state"], over: Partial<Job> = {}): Job => ({
  job: "0123456789abcdef0123456789abcdef",
  kind: "copy",
  state,
  files_done: 1,
  files_total: 4,
  bytes_done: 100,
  bytes_total: 400,
  mine: true,
  ...over,
});

const jobsCalls = () => mockCall.mock.calls.filter((c) => c[0] === "/api/jobs").length;

describe("useJobs poller", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    mockCall.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("polls_fast_while_running_then_slow", async () => {
    const replies = [[job("running")], [job("done")], [job("done")]];
    mockCall.mockImplementation(async () => ({ jobs: replies.shift() ?? [] }));
    const seen: Job[][] = [];
    const p = createJobsPoller({
      onJobs: (j) => seen.push(j),
      onError: () => {},
    });
    p.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(jobsCalls()).toBe(1);
    expect(seen[0][0].state).toBe("running");

    // something runs: the next read comes after JOBS_FAST_MS
    await vi.advanceTimersByTimeAsync(JOBS_FAST_MS - 1);
    expect(jobsCalls()).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(jobsCalls()).toBe(2);

    // nothing runs any more: the poller backs off to JOBS_SLOW_MS
    await vi.advanceTimersByTimeAsync(JOBS_FAST_MS);
    expect(jobsCalls()).toBe(2);
    await vi.advanceTimersByTimeAsync(JOBS_SLOW_MS - JOBS_FAST_MS);
    expect(jobsCalls()).toBe(3);
    p.stop();
  });

  it("stops polling once stopped (the section unmounted)", async () => {
    mockCall.mockResolvedValue({ jobs: [job("running")] });
    const p = createJobsPoller({ onJobs: () => {}, onError: () => {} });
    p.start();
    await vi.advanceTimersByTimeAsync(0);
    p.stop();
    await vi.advanceTimersByTimeAsync(JOBS_SLOW_MS * 3);
    expect(jobsCalls()).toBe(1);
  });

  it("pauses while the page is hidden and reads at once when it shows again", async () => {
    mockCall.mockResolvedValue({ jobs: [job("running")] });
    const doc = { hidden: false, listeners: new Set<() => void>() };
    const p = createJobsPoller({
      onJobs: () => {},
      onError: () => {},
      doc: {
        get hidden() {
          return doc.hidden;
        },
        addEventListener: (_t, fn) => doc.listeners.add(fn),
        removeEventListener: (_t, fn) => doc.listeners.delete(fn),
      },
    });
    p.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(jobsCalls()).toBe(1);
    doc.hidden = true;
    doc.listeners.forEach((fn) => fn());
    await vi.advanceTimersByTimeAsync(JOBS_FAST_MS * 5);
    expect(jobsCalls()).toBe(1);
    p.refresh();
    await vi.advanceTimersByTimeAsync(JOBS_FAST_MS);
    expect(jobsCalls()).toBe(1);
    doc.hidden = false;
    doc.listeners.forEach((fn) => fn());
    await vi.advanceTimersByTimeAsync(0);
    expect(jobsCalls()).toBe(2);
    await vi.advanceTimersByTimeAsync(JOBS_FAST_MS);
    expect(jobsCalls()).toBe(3);
    p.stop();
    expect(doc.listeners.size).toBe(0);
  });

  it("reports errors and backs off; stops for good on 401", async () => {
    mockCall.mockRejectedValueOnce(new ApiError(0, "offline")).mockRejectedValueOnce(new ApiError(401, "token"));
    const errors: unknown[] = [];
    const p = createJobsPoller({
      onJobs: () => {},
      onError: (e) => errors.push(e),
    });
    p.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(errors).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(JOBS_FAST_MS);
    expect(jobsCalls()).toBe(1); // an error backs off, it does not hammer
    await vi.advanceTimersByTimeAsync(JOBS_SLOW_MS);
    expect(jobsCalls()).toBe(2);
    expect(errors).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(JOBS_SLOW_MS * 3);
    expect(jobsCalls()).toBe(2); // the token is gone: every read would 401
  });

  it("refresh reads at once without overlapping a read in flight", async () => {
    let release: (v: unknown) => void = () => {};
    mockCall.mockImplementationOnce(() => new Promise((r) => (release = r)));
    mockCall.mockResolvedValue({ jobs: [] });
    const p = createJobsPoller({ onJobs: () => {}, onError: () => {} });
    p.start();
    await vi.advanceTimersByTimeAsync(0);
    p.refresh();
    await vi.advanceTimersByTimeAsync(0);
    expect(jobsCalls()).toBe(1); // still waiting for the first answer
    release({ jobs: [] });
    await vi.advanceTimersByTimeAsync(0);
    expect(jobsCalls()).toBe(2); // the refresh ran right after it
    p.stop();
  });

  it("delay: fast only while some job is running", () => {
    expect(nextJobsDelay([])).toBe(JOBS_SLOW_MS);
    expect(nextJobsDelay([job("done"), job("failed")])).toBe(JOBS_SLOW_MS);
    expect(nextJobsDelay([job("done"), job("running")])).toBe(JOBS_FAST_MS);
    expect(JOBS_FAST_MS).toBe(1000);
    expect(JOBS_SLOW_MS).toBe(5000);
  });

  it("cancelJob posts the job id", async () => {
    mockCall.mockResolvedValue({});
    await cancelJob("abc");
    expect(mockCall).toHaveBeenCalledWith("/api/jobs/cancel", { job: "abc" });
  });
});

describe("network-share jobs in Tasks (#658)", () => {
  beforeEach(() => {
    mockCall.mockReset();
  });
  const smb = (over: Record<string, unknown> = {}) => ({
    job: "7",
    kind: "download",
    state: "running",
    bytes_done: 50,
    bytes_total: 200,
    files_done: 0,
    files_total: 1,
    started: 1000,
    src: "Games/PPSA01234",
    dest: "/data/homebrew/PPSA01234",
    ...over,
  });

  it("lists a NAS copy as this console's copy, named after what it copies", () => {
    expect(smbJobToJob(smb(), 1010)).toEqual({
      job: "smb:7",
      kind: "copy",
      state: "running",
      bytes_done: 50,
      bytes_total: 200,
      files_done: 0,
      files_total: 1,
      mine: true,
      name: "PPSA01234",
    });
    expect(smbJobToJob(smb({ kind: "upload", src: "/data/saves/a.zip", dest: "backup/a.zip" }), 1010)?.name).toBe("a.zip");
    expect(smbJobToJob(smb({ kind: "remove", src: "old" }), 1010)?.kind).toBe("op");
    expect(smbJobToJob(smb({ state: "cancelled" }), 1010)?.state).toBe("failed");
  });

  it("keeps a long network-share copy that just ended (from when it ended, not when it started)", () => {
    const now = 100_000;
    const long = smb({ state: "done", started: 1000, ended: now - 5 });
    expect(smbJobToJob(long, now)?.state).toBe("done");
    expect(smbJobToJob(smb({ state: "done", started: 1000, ended: 2000 }), now)).toBeNull();
    expect(smbJobToJob(smb({ state: "done", started: 1000 }), now)).toBeNull();
  });

  it("leaves out library scans, junk and long-finished jobs", () => {
    expect(smbJobToJob(smb({ kind: "scan" }), 1010)).toBeNull();
    expect(smbJobToJob({ kind: "download" }, 1010)).toBeNull();
    expect(smbJobToJob(smb({ state: "done" }), 1000 + SMB_DONE_KEEP_S + 1)).toBeNull();
    expect(smbJobToJob(smb({ state: "done" }), 1000 + 60)?.state).toBe("done");
    // A running one stays however long it takes.
    expect(smbJobToJob(smb(), 1000 + SMB_DONE_KEEP_S * 10)?.state).toBe("running");
  });

  it("merges them after the helper's jobs, and a dead daemon costs nothing", async () => {
    mockCall.mockImplementation(async (route: string) =>
      route === "/api/jobs" ? { jobs: [job("running")], uploading: [] } : { jobs: [smb({ started: Date.now() / 1000 })] },
    );
    const { jobs } = await fetchJobsAll();
    expect(jobs.map((j) => j.job)).toEqual(["0123456789abcdef0123456789abcdef", "smb:7"]);
    mockCall.mockImplementation(async (route: string) => {
      if (route === "/api/jobs") return { jobs: [job("running")] };
      throw new ApiError(502, "smb_daemon_gone");
    });
    expect((await fetchJobsAll()).jobs).toHaveLength(1);
  });

  it("cancels a network-share job through the share daemon", async () => {
    mockCall.mockResolvedValue({ ok: true });
    await cancelJob("smb:7");
    expect(mockCall).toHaveBeenCalledWith("/api/smb/cancel", { job: "7" });
  });
});
