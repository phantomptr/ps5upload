import { describe, expect, it } from "vitest";

import { routeForTask, shortLabel, summarize } from "./activitySummary";
import type { Task } from "./tasks";

const NOW = 1_000_000_000;
let n = 0;
function task(p: Partial<Task>): Task {
  n += 1;
  return {
    id: `t${n}`,
    kind: "upload-file",
    origin: "x",
    createdAt: new Date(NOW - 100_000 + n).toISOString(),
    status: "running",
    attempts: 0,
    maxAttempts: 1,
    consoleId: "h",
    payload: {},
    label: `Job ${n}`,
    updatedAtMs: NOW - 1000,
    endedAtMs: null,
    ...p,
  } as Task;
}
const opts = (seen: string[] = []) => ({ now: NOW, sessionStart: NOW - 3_600_000, seen: new Set(seen) });

describe("summarize", () => {
  it("names at most three running jobs and counts the rest", () => {
    const tasks = [
      ...Array.from({ length: 12 }, (_, i) =>
        task({ kind: "upload-file", progress: { current: i, total: 12, unit: "files" } }),
      ),
      task({ kind: "pkg-dpi-install" }),
    ];
    const s = summarize(tasks, opts());
    expect(s.running).toHaveLength(13);
    expect(s.headline).toHaveLength(3);
    expect(s.more).toBe(10);
  });

  it("marks a job with no update for a minute as stale", () => {
    const s = summarize(
      [task({ updatedAtMs: NOW - 125_000, progress: { current: 1, total: 9, unit: "bytes" } })],
      opts(),
    );
    expect(s.running[0].staleMin).toBe(2);
    expect(summarize([task({})], opts()).running[0].staleMin).toBeNull();
  });

  it("lists this session's finished jobs, newest first, five at most", () => {
    const done = Array.from({ length: 7 }, (_, i) =>
      task({ status: "done", endedAtMs: NOW - (i + 1) * 60_000 }),
    );
    const old = task({ status: "done", endedAtMs: NOW - 7_200_000 });
    const s = summarize([...done, old], opts());
    expect(s.finished).toHaveLength(5);
    expect(s.finished[0].agoMs).toBe(60_000);
  });

  it("badges failures until seen, but not interrupted jobs from a reload", () => {
    const failed = task({ status: "failed", endedAtMs: NOW - 10_000 });
    const interrupted = task({ status: "interrupted", endedAtMs: NOW - 10_000 });
    expect(summarize([failed, interrupted], opts()).failedUnseen).toBe(1);
    expect(summarize([failed, interrupted], opts([failed.id])).failedUnseen).toBe(0);
    expect(summarize([interrupted], opts()).running).toHaveLength(0);
    expect(summarize([interrupted], opts()).finished[0].outcome).toBe("interrupted");
  });

  it("flashes a job that ended in the last five seconds", () => {
    const s = summarize(
      [task({ kind: "backup-snapshot", status: "done", endedAtMs: NOW - 2000, label: "Backup PS5" })],
      opts(),
    );
    expect(s.flash).toEqual({ short: "Backup", label: "Backup PS5", outcome: "done" });
    expect(summarize([task({ status: "done", endedAtMs: NOW - 9000 })], opts()).flash).toBeNull();
  });

  it("shows no percent for a job without progress", () => {
    expect(summarize([task({ kind: "backup-snapshot" })], opts()).running[0].pct).toBeNull();
  });
});

describe("labels and routes", () => {
  it("routes each job to its screen", () => {
    expect(shortLabel("fpkg-convert")).toBe("Convert");
    expect(routeForTask(task({ kind: "fpkg-convert" }))).toBe("/convert");
    expect(routeForTask(task({ kind: "pkg-dpi-install" }))).toBe("/install-package");
    expect(routeForTask(task({ kind: "library-op" }))).toBe("/library");
  });

  it("never calls a job stale that has no progress to report", () => {
    // A backup or bug report runs as one silent await; a minute without news is normal for it.
    expect(summarize([task({ updatedAtMs: NOW - 125_000 })], opts()).running[0].staleMin).toBeNull();
  });

  it("lists jobs this start-up interrupted, as ended when the session began", () => {
    const sessionStart = NOW - 60_000;
    const cut = task({ status: "interrupted", updatedAtMs: NOW - 7_200_000, endedAtMs: NOW - 7_200_000 });
    const old = task({ status: "interrupted", endedAtMs: NOW - 7_300_000 });
    const s = summarize([cut, old], {
      now: NOW,
      sessionStart,
      seen: new Set(),
      interruptedAtLoad: new Set([cut.id]),
    });
    expect(s.finished.map((r) => r.id)).toEqual([cut.id]);
    expect(s.finished[0]).toMatchObject({ outcome: "interrupted", agoMs: 60_000 });
    expect(s.failedUnseen).toBe(0);
  });
});
