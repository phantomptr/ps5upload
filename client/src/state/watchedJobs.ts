import { create } from "zustand";

import type { JobSnapshot } from "../api/ps5";
import { hostOf } from "../lib/addr";
import { beginTask } from "./trackTask";
import type { TaskKind } from "./tasks";

/**
 * One engine job started from a screen and watched to its end, kept outside the screen.
 *
 * A screen that polls its own job loses it when the user navigates away: the engine carries
 * on, but the progress, the Stop button and the result are gone on return. Here the poll loop
 * and the state belong to a key the screen can rebuild (for example a console and a file), so
 * a returning screen finds the run where it is, and it shows in Tasks from any screen.
 */

export type WatchedJobPhase = "running" | "done" | "failed" | "stopped";

export interface WatchedJob {
  phase: WatchedJobPhase;
  /** "" until the engine has answered with the job. */
  jobId: string;
  sent: number;
  total: number;
  /** Where the engine says the result landed, once done. */
  dest: string | null;
  /** The engine's error, raw, when failed. */
  error: string | null;
  stopRequested: boolean;
  startedAtMs: number;
}

interface WatchedJobStore {
  byKey: Record<string, WatchedJob>;
  put: (key: string, patch: Partial<WatchedJob>) => void;
  drop: (key: string) => void;
}

export const useWatchedJobStore = create<WatchedJobStore>((set) => ({
  byKey: {},
  put: (key, patch) =>
    set((s) => {
      const cur = s.byKey[key];
      if (!cur && patch.phase === undefined) return s;
      return {
        byKey: { ...s.byKey, [key]: { ...(cur as WatchedJob), ...patch } },
      };
    }),
  drop: (key) =>
    set((s) => {
      if (!(key in s.byKey)) return s;
      const next = { ...s.byKey };
      delete next[key];
      return { byKey: next };
    }),
}));

export function watchedJob(
  s: { byKey: Record<string, WatchedJob> },
  key: string,
): WatchedJob | null {
  return s.byKey[key] ?? null;
}

export interface WatchedJobDeps {
  jobStatus: (jobId: string) => Promise<JobSnapshot>;
  jobCancel: (jobId: string) => Promise<void>;
  sleep: (ms: number) => Promise<void>;
}

export interface WatchedJobInit {
  key: string;
  kind: TaskKind;
  origin: string;
  label: string;
  host: string;
  detail?: string;
  /** Where the result lands when the engine's final answer does not say. */
  fallbackDest?: string;
  /** false: no task of its own (the caller already shows the run somewhere). */
  track?: boolean;
}

/** For a caller that mirrors the run elsewhere. Called whichever screen is open. */
export interface WatchedJobHooks {
  onProgress?: (sent: number, total: number) => void;
  onEnd?: (job: WatchedJob) => void;
}

/** Starts the job and polls it to its end. Resolves when it ends, whichever way; the outcome
 *  is in the store under `init.key`. A key that is already running is left alone. */
export async function watchJob(
  init: WatchedJobInit,
  start: () => Promise<string>,
  deps: WatchedJobDeps,
  hooks: WatchedJobHooks = {},
): Promise<void> {
  const { key } = init;
  const store = useWatchedJobStore.getState();
  if (watchedJob(store, key)?.phase === "running") return;
  const stopRequested = () =>
    watchedJob(useWatchedJobStore.getState(), key)?.stopRequested === true;
  store.put(key, {
    phase: "running",
    jobId: "",
    sent: 0,
    total: 0,
    dest: null,
    error: null,
    stopRequested: false,
    startedAtMs: Date.now(),
  });
  const task =
    init.track === false
      ? null
      : beginTask({
          kind: init.kind,
          origin: init.origin,
          label: init.label,
          consoleId: hostOf(init.host),
          detail: init.detail,
        });
  const end = () => {
    const job = watchedJob(useWatchedJobStore.getState(), key);
    if (job) hooks.onEnd?.(job);
  };
  try {
    const jobId = await start();
    store.put(key, { jobId });
    // A Stop pressed before the job id arrived had nothing to end: end it now.
    if (stopRequested()) void deps.jobCancel(jobId).catch(() => {});
    for (;;) {
      const snap = await deps.jobStatus(jobId);
      const sent = snap.bytes_sent ?? 0;
      const total = snap.total_bytes ?? 0;
      if (snap.status === "done") {
        store.put(key, {
          phase: "done",
          sent,
          total,
          dest: snap.dest ?? init.fallbackDest ?? null,
        });
        task?.done();
        end();
        return;
      }
      if (snap.status === "failed") {
        if (stopRequested()) break;
        throw new Error(snap.error ?? "The job failed.");
      }
      store.put(key, { sent, total });
      hooks.onProgress?.(sent, total);
      task?.report({
        progress:
          total > 0 ? { current: sent, total, unit: "bytes" } : undefined,
      });
      await deps.sleep(500);
    }
    store.put(key, { phase: "stopped" });
    task?.fail(new Error("Stopped"));
    end();
  } catch (e) {
    store.put(key, {
      phase: "failed",
      error: e instanceof Error ? e.message : String(e),
    });
    task?.fail(e);
    end();
  }
}

/** Asks the engine to end the job; the run then ends "stopped". */
export function stopWatchedJob(
  key: string,
  deps: Pick<WatchedJobDeps, "jobCancel">,
): void {
  const s = useWatchedJobStore.getState();
  const cur = watchedJob(s, key);
  if (!cur || cur.phase !== "running") return;
  s.put(key, { stopRequested: true });
  if (cur.jobId) void deps.jobCancel(cur.jobId).catch(() => {});
}

/** Forgets a finished run (the screen showed its result). A running one stays. */
export function dismissWatchedJob(key: string): void {
  const s = useWatchedJobStore.getState();
  if (watchedJob(s, key)?.phase === "running") return;
  s.drop(key);
}
