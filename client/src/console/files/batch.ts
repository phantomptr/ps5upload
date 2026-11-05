/**
 * The page's copy/move/delete batch: one at a time, page-wide.
 *
 * A batch sends one item, waits until that item's job has left "running"
 * (read from /api/jobs, the list Home and Files poll), then sends the next:
 * the helper's job table holds 32 jobs for every owner (paired PCs too), and a
 * 40-item copy must not take it over. Stop cancels the job running and sends no
 * more items.
 *
 * The batch lives outside any component, so it keeps going while another
 * section is open and Files shows its progress again when it comes back. Only
 * one batch runs at a time.
 */
import { useEffect, useState } from "react";
import { ApiError } from "../api";
import { cancelJob, fetchJobs, type Job } from "../home/useJobs";
import type { BatchOptions, BatchResult, JobEnd } from "./move";

export type BatchOp = "copy" | "move" | "delete" | "chmod" | "rename" | "unpack" | "install" | "extract";

/** The batch running now: item `at` (1-based) of `total` is being sent or waited on. */
export interface BatchProgress {
  op: BatchOp;
  at: number;
  total: number;
  /** Stop was asked: no further item is sent. */
  stopping: boolean;
  /** What it reads and writes: its items and (copy/move) the destination folder. */
  paths?: string[];
  /** Bytes written so far of `bytesTotal` (a one-job batch such as unpack reports them). */
  bytesDone?: number;
  bytesTotal?: number;
  /** A one-job batch's own line (an install's phase: its job message). */
  phase?: string | null;
  /** Stop cancels the item running too, so it is offered for one item as well. */
  cancels?: boolean;
}

/** How often a running job is checked. */
export const BATCH_POLL_MS = 1000;
/** A job missing from this many reads in a row has ended (released from the list). */
export const BATCH_ABSENT_READS = 2;
/** After Cancel, the job is read this many more times for its end (it stops within a chunk). */
export const BATCH_CANCEL_READS = 10;

export interface WaiterDeps {
  fetchJobs: () => Promise<Job[]>;
  sleep: (ms: number) => Promise<void>;
  pollMs?: number;
  /** Ask the helper to stop a job. Without it Stop only lets go of the job, which runs on ("left"). */
  cancel?: (id: string) => Promise<void>;
}

/**
 * Wait for job `id` to leave "running": "done" / "failed" from the list, "gone" when it is no
 * longer listed. `stopped()` turning true cancels it, then waits (bounded) for its end: "cancelled",
 * "done" when it finished first, or "left" when it runs on after all. A cancelled copy leaves the
 * list at once and a cancelled operation ends "failed"; both read as "cancelled". A read that fails
 * is retried; a 401 (the token is gone) is thrown.
 */
export function createJobWaiter(deps: WaiterDeps) {
  const pollMs = deps.pollMs ?? BATCH_POLL_MS;
  return async (id: string, stopped?: () => boolean): Promise<JobEnd> => {
    let absent = 0;
    let cancelAsked = false;
    let after = 0;
    for (;;) {
      let jobs: Job[] | null = null;
      try {
        jobs = await deps.fetchJobs();
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) throw e;
      }
      if (jobs) {
        const j = jobs.find((x) => x.job === id);
        if (j) {
          absent = 0;
          if (j.state !== "running") return cancelAsked && j.state === "failed" ? "cancelled" : j.state;
        } else if (++absent >= BATCH_ABSENT_READS) {
          return cancelAsked ? "cancelled" : "gone";
        }
      }
      if (stopped?.()) {
        if (!deps.cancel) return "left";
        if (cancelAsked) {
          if (++after > BATCH_CANCEL_READS) return "left";
        } else {
          cancelAsked = true;
          await deps.cancel(id).catch((e: unknown) => {
            if (e instanceof ApiError && e.status === 401) throw e;
          });
        }
      }
      await deps.sleep(pollMs);
      if (!deps.cancel && stopped?.()) return "left";
    }
  };
}

export const waitJob = createJobWaiter({
  fetchJobs,
  cancel: cancelJob,
  sleep: (ms) => new Promise((res) => setTimeout(res, ms)),
});

/* ---- the one batch ---- */

let progress: BatchProgress | null = null;
let stopAsked = false;
const listeners = new Set<() => void>();

function emit(): void {
  listeners.forEach((fn) => fn());
}

/** The running batch, or null. */
export function currentBatch(): BatchProgress | null {
  return progress;
}

/** Hear every change of currentBatch(). Returns the unsubscribe function. */
export function onBatchChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** Stop the running batch before its next item. */
export function stopBatch(): void {
  if (!progress || progress.stopping) return;
  stopAsked = true;
  progress = { ...progress, stopping: true };
  emit();
}

export interface BatchControl {
  onProgress: (at: number, total: number) => void;
  /** Bytes done of total for the running item (shown with a percentage). */
  onBytes: (done: number, total: number) => void;
  /** The running job's message (an install's phase). */
  onPhase: (phase: string | null) => void;
  stopped: () => boolean;
}

/** The batch's control as the options copy, move, delete and paste take. */
export function batchOptions(ctl: BatchControl): BatchOptions {
  return {
    onProgress: ctl.onProgress,
    stopped: ctl.stopped,
    waitJob: (id, unstoppable) => waitJob(id, unstoppable ? undefined : ctl.stopped),
  };
}

/**
 * Run `work` as the page's batch. Null (nothing started) while another batch
 * runs. The batch is over when the promise settles.
 */
export function startBatch<R extends BatchResult = BatchResult>(
  op: BatchOp,
  total: number,
  work: (ctl: BatchControl) => Promise<R>,
  paths?: string[],
  cancels?: boolean,
): Promise<R> | null {
  if (progress) return null;
  stopAsked = false;
  progress = { op, at: 0, total, stopping: false, ...(paths ? { paths } : {}), ...(cancels ? { cancels } : {}) };
  emit();
  const ctl: BatchControl = {
    onProgress: (at, n) => {
      if (!progress) return;
      progress = { ...progress, at, total: n };
      emit();
    },
    onBytes: (done, total) => {
      if (!progress) return;
      if (progress.bytesDone === done && progress.bytesTotal === total) return;
      progress = { ...progress, bytesDone: done, bytesTotal: total };
      emit();
    },
    onPhase: (phase) => {
      if (!progress || progress.phase === phase) return;
      progress = { ...progress, phase };
      emit();
    },
    stopped: () => stopAsked,
  };
  let run: Promise<R>;
  try {
    run = work(ctl);
  } catch (e) {
    run = Promise.reject(e);
  }
  return run.finally(() => {
    progress = null;
    stopAsked = false;
    emit();
  });
}

/** currentBatch() as React state. */
export function useBatch(): BatchProgress | null {
  const [b, setB] = useState(currentBatch);
  useEffect(() => {
    setB(currentBatch());
    return onBatchChange(() => setB(currentBatch()));
  }, []);
  return b;
}
