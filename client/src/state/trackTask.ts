// One-shot operations (a backup, a save restore, a bug report…) show in the activity bar as a
// task: running while they run, then done, or failed with the error's message.
//
// `trackTask` wraps a single await. `beginTask` is for a handler whose work is one long try block:
// begin once the user has committed (after any file picker), `fail(e)` in the catch, `done()` in
// the finally. The first call settles the task, so the finally cannot undo a failure and an early
// return cannot leave a task running.

import { useTaskStore, type TaskKind, type TaskProgress } from "./tasks";

export type TaskReport = (patch: { stage?: string; progress?: TaskProgress }) => void;

export interface TaskInit {
  kind: TaskKind;
  origin: string;
  label: string;
  consoleId?: string;
  detail?: string;
}

export interface TaskHandle {
  report: TaskReport;
  done: () => void;
  fail: (error: unknown) => void;
}

export function beginTask(init: TaskInit): TaskHandle {
  const id = useTaskStore.getState().registerTask({ ...init, consoleId: init.consoleId ?? "" });
  let settled = false;
  return {
    report: (patch) => {
      if (!settled) useTaskStore.getState().updateTask(id, patch);
    },
    done: () => {
      if (settled) return;
      settled = true;
      useTaskStore.getState().finishTask(id, "done");
    },
    fail: (error) => {
      if (settled) return;
      settled = true;
      useTaskStore.getState().finishTask(id, "failed", {
        lastError: {
          code: "OP_FAILED",
          message: error instanceof Error ? error.message : String(error),
          recoverable: false,
        },
      });
    },
  };
}

/** Run `op` as a task in the activity bar: registered running, finished done on success,
 *  failed with the error's message when it throws (and rethrown). */
export async function trackTask<T>(
  init: TaskInit,
  op: (report: TaskReport) => Promise<T>,
): Promise<T> {
  const task = beginTask(init);
  try {
    const value = await op(task.report);
    task.done();
    return value;
  } catch (error) {
    task.fail(error);
    throw error;
  }
}
