import { create } from "zustand";

import { appDump, type AppDumpRequest, type AppDumpResult } from "../api/appDump";
import { fsOpCancel, fsOpStatus } from "../api/ps5";
import { transferAddr } from "../lib/addr";
import { beginTask } from "./trackTask";
import { trStatic } from "../lib/trStatic";

export type DumpState = "waiting" | "running" | "done" | "failed" | "cancelled";

export interface DumpJob {
  id: number;
  host: string;
  name: string;
  req: AppDumpRequest;
  state: DumpState;
  done: number;
  total: number;
  /** Set once copying has started (before that the game is being started). */
  copying: boolean;
  result?: AppDumpResult;
  error?: string;
  stopping?: boolean;
}

interface DumpStore {
  jobs: DumpJob[];
  add: (host: string, name: string, req: AppDumpRequest) => void;
  cancel: (id: number) => void;
  /** Drop one finished job from the list (a running or waiting one stays). */
  dismiss: (id: number) => void;
}

const patch = (id: number, p: Partial<DumpJob>) =>
  useAppDumpStore.setState((s) => ({ jobs: s.jobs.map((j) => (j.id === id ? { ...j, ...p } : j)) }));

/** One dump per console at a time (the helper runs one); the next waits its turn. */
async function runNext(host: string): Promise<void> {
  const { jobs } = useAppDumpStore.getState();
  if (jobs.some((j) => j.host === host && j.state === "running")) return;
  const job = jobs.find((j) => j.host === host && j.state === "waiting");
  if (!job) return;
  patch(job.id, { state: "running" });
  const addr = transferAddr(host);
  const task = beginTask({ kind: "app-dump", origin: "games", label: trStatic("task_dump", "Dump {name}", { name: job.name }), consoleId: host });
  let finished = false;
  const poller = (async () => {
    while (!finished) {
      try {
        const snap = await fsOpStatus(addr, job.id);
        if (snap.total_bytes > 0) {
          patch(job.id, { copying: true, done: snap.bytes_copied, total: snap.total_bytes });
          task.report({ progress: { current: snap.bytes_copied, total: snap.total_bytes, unit: "bytes" } });
        }
      } catch {
        // Not registered yet (the game is starting) or already finished.
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
  })();
  try {
    const result = await appDump(addr, job.req, job.id);
    patch(job.id, { state: "done", result, done: result.bytes, total: result.bytes, copying: true });
    task.done();
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    const cancelled = msg === "cancelled";
    patch(job.id, { state: cancelled ? "cancelled" : "failed", error: cancelled ? undefined : msg });
    if (cancelled) task.done();
    else task.fail(e);
  } finally {
    finished = true;
    await poller;
    void runNext(host);
  }
}

export const useAppDumpStore = create<DumpStore>(() => ({
  jobs: [],
  add(host, name, req) {
    const id = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    useAppDumpStore.setState((s) => ({
      jobs: [...s.jobs, { id, host, name, req, state: "waiting", done: 0, total: 0, copying: false }],
    }));
    void runNext(host);
  },
  cancel(id) {
    const job = useAppDumpStore.getState().jobs.find((j) => j.id === id);
    if (!job) return;
    if (job.state === "waiting") {
      patch(id, { state: "cancelled" });
      return;
    }
    if (job.state === "running") {
      patch(id, { stopping: true });
      void fsOpCancel(transferAddr(job.host), id).catch(() => {});
    }
  },
  dismiss(id) {
    useAppDumpStore.setState((s) => ({
      jobs: s.jobs.filter((j) => j.id !== id || j.state === "waiting" || j.state === "running"),
    }));
  },
}));

/** A helper's cause as the person reads it. */
export function dumpErrorKey(error: string): { key: string; fallback: string } {
  if (error.includes("dump_exists"))
    return { key: "dump_err_exists", fallback: "A dump of this game is already in that folder. Delete or rename it first." };
  if (error.includes("dump_no_space"))
    return { key: "dump_err_space", fallback: "Not enough free space on that drive." };
  if (error.includes("dump_game_closed"))
    return { key: "dump_err_closed", fallback: "The game closed before its files were copied. Keep it running (on its start screen) until the dump finishes." };
  if (error.includes("dump_not_running"))
    return { key: "dump_err_not_running", fallback: "The game did not start, so its files could not be read. Start it on the PS5 and try again." };
  if (error.includes("dump_busy") || error.includes("BUSY"))
    return { key: "dump_err_busy", fallback: "The PS5 is already dumping a game." };
  return { key: "dump_err_other", fallback: "The dump stopped." };
}
