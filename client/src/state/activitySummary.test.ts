import { describe, expect, it } from "vitest";

import {
  needsClock,
  pickPrimary,
  routeForTask,
  shortLabel,
  shortLabelKey,
  summarize,
  type QueuedRow,
} from "./activitySummary";
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
  it("names one job and counts every other live one, queued work included", () => {
    const tasks = [
      ...Array.from({ length: 12 }, (_, i) =>
        task({ kind: "upload-file", progress: { current: i, total: 12, unit: "files" } }),
      ),
      task({ kind: "pkg-dpi-install" }),
    ];
    const queued: QueuedRow[] = [
      { id: "q1", owner: "upload-queue", kind: "pkg-install", label: "Game", consoleId: "h", route: "/send", sizeBytes: null },
    ];
    const s = summarize(tasks, { ...opts(), queued });
    expect(s.running).toHaveLength(13);
    expect(s.primary?.id).toBe(tasks[0].id);
    expect(s.queued).toEqual(queued);
    expect(s.others).toBe(13);
    expect(summarize([], opts())).toMatchObject({ primary: null, others: 0, queued: [] });
  });

  it("keeps the strip on a job that is moving, not one that went quiet or only waits", () => {
    const quiet = task({ updatedAtMs: NOW - 300_000, progress: { current: 1, total: 9, unit: "bytes" } });
    const waiting = task({ status: "queued" });
    const silent = task({ kind: "backup-snapshot" });
    const moving = task({ progress: { current: 5, total: 10, unit: "bytes" } });
    const s = summarize([quiet, waiting, silent, moving], opts());
    expect(s.primary?.id).toBe(moving.id);
    // Without one that reports progress, the oldest live job that still answers.
    expect(summarize([quiet, waiting, silent], opts()).primary?.id).toBe(silent.id);
    expect(pickPrimary([])).toBeNull();
  });

  it("marks a job with no update for a minute as stale", () => {
    const s = summarize(
      [task({ updatedAtMs: NOW - 125_000, progress: { current: 1, total: 9, unit: "bytes" } })],
      opts(),
    );
    expect(s.running[0].staleMin).toBe(2);
    expect(summarize([task({})], opts()).running[0].staleMin).toBeNull();
  });

  it("lists this session's finished jobs, newest first, eight at most", () => {
    const done = Array.from({ length: 10 }, (_, i) =>
      task({ status: "done", endedAtMs: NOW - (i + 1) * 60_000 }),
    );
    const old = task({ status: "done", endedAtMs: NOW - 7_200_000 });
    const s = summarize([...done, old], opts());
    expect(s.finished).toHaveLength(8);
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
    expect(routeForTask(task({ kind: "fpkg-convert" }))).toBe("/workshop");
    expect(routeForTask(task({ kind: "pkg-dpi-install" }))).toBe("/send");
    expect(routeForTask(task({ kind: "library-op" }))).toBe("/games");
    expect(routeForTask(task({ kind: "fs-delete" }))).toBe("/files");
    expect(routeForTask(task({ kind: "backport-patch" }))).toBe("/games");
    expect(routeForTask(task({ kind: "library-move" }))).toBe("/games");
    expect(routeForTask(task({ kind: "helper-send" }))).toBe("/connection");
    expect(routeForTask(task({ kind: "payload-playlist" }))).toBe("/payloads");
    expect(routeForTask(task({ kind: "app-dump" }))).toBe("/games");
    expect(shortLabel("library-move")).toBe("Move");
    expect(shortLabelKey("pkg-dpi-install")).toBe("activity_kind_pkg_dpi_install");
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

describe("needsClock", () => {
  const ended = task({ status: "done", endedAtMs: NOW - 3_600_000, updatedAtMs: NOW - 3_600_000 });
  it("ticks while the panel is open, so an old finished job does not read 'just now'", () => {
    expect(needsClock([ended], NOW - 3_590_000, ended.updatedAtMs, true)).toBe(true);
    expect(needsClock([], 0, 0, true)).toBe(true);
  });
  it("stops when closed and nothing runs or has just ended", () => {
    expect(needsClock([ended], NOW - 3_590_000, ended.updatedAtMs, false)).toBe(false);
  });
  it("ticks while a job runs, and for a few seconds after one ends", () => {
    expect(needsClock([task({})], 0, NOW, false)).toBe(true);
    const just = task({ status: "done", endedAtMs: NOW, updatedAtMs: NOW });
    expect(needsClock([just], NOW + 2000, NOW, false)).toBe(true);
    expect(needsClock([just], NOW + 7000, NOW, false)).toBe(false);
  });
});
