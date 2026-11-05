/**
 * Tasks started on the console page (PS5Upload Minion) as rows in this app's Activity (#658).
 *
 * There is no poller here: AppShell's status tick calls `readConsoleTasks` for a console whose
 * helper just answered. It never adds traffic during a transfer to that console (the tick skips
 * it, and so does this), reads nothing while the window is hidden (no ticks), and backs off from a
 * helper older than the list.
 *
 * The console only ever lists what its page started, never a job an app started, so an app's own
 * transfer or NAS copy has one row. Closing the app does not stop any of this, which is why these
 * rows do not count as the app's own running work (lib/awakeWork) and do not outlive a restart as
 * "interrupted" (state/tasks): the next read finds them again if they still run.
 */
import { cancelConsoleTask, ConsoleTaskError, fetchConsoleTasks, type ConsoleTaskInfo } from "../api/consoleTasks";
import { hostOf } from "../lib/addr";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { trStatic } from "../lib/trStatic";
import { useTaskStore, isTerminal, type Task, type TaskProgress } from "./tasks";

export const CONSOLE_TASK_KIND = "console-task" as const;
/** A helper without the list is asked again this much later: it may have been updated. */
export const UNSUPPORTED_RETRY_MS = 5 * 60_000;
/** A finished task this app never saw running gets a row only if it ended this recently: an app
 *  just opened should not report what ended before it looked. */
export const FRESH_END_S = 60;

const ACTIVITY_VERB: Record<string, [string, string]> = {
  copy: ["console_task_copy", "Copying"],
  move: ["console_task_move", "Moving"],
  delete: ["console_task_delete", "Deleting"],
  chmod: ["console_task_chmod", "Changing permissions"],
  verify: ["console_task_verify", "Checking"],
  unpack: ["console_task_unpack", "Unpacking"],
  extract: ["console_task_extract", "Extracting"],
  pkg_install: ["console_task_pkg_install", "Installing"],
};

export function consoleTaskLabel(t: Pick<ConsoleTaskInfo, "kind" | "name">): string {
  const [key, fallback] = ACTIVITY_VERB[t.kind] ?? ["console_task_other", "Working"];
  const verb = trStatic(key, fallback);
  return t.name ? `${verb} · ${t.name}` : verb;
}

export function consoleTaskProgress(t: ConsoleTaskInfo): TaskProgress | undefined {
  if (t.bytes_total > 0) return { current: t.bytes_done, total: t.bytes_total, unit: "bytes" };
  if (t.files_total > 0) return { current: t.files_done, total: t.files_total, unit: "files" };
  return undefined;
}

const taskIdOf = (t: Task): unknown => t.payload.consoleTaskId;

/** Ids given a row in this session, per console: a row the user dismissed stays dismissed for as
 *  long as the console keeps listing how that task ended. */
const noted = new Map<string, Set<string>>();

function notedFor(key: string): Set<string> {
  let s = noted.get(key);
  if (!s) {
    s = new Set();
    noted.set(key, s);
  }
  return s;
}

function finish(rowId: string, info: ConsoleTaskInfo): void {
  const store = useTaskStore.getState();
  const progress = consoleTaskProgress(info);
  const extras = progress ? { progress } : {};
  if (info.state === "done") store.finishTask(rowId, "done", extras);
  else if (info.state === "cancelled") store.finishTask(rowId, "cancelled", extras);
  // "gone": it ended and nobody saw how. Not a failure, and not a success to claim.
  else if (info.state === "gone") store.finishTask(rowId, "unverified", extras);
  else store.finishTask(rowId, "failed", extras);
}

/** Brings this console's rows in line with what it lists. */
export function applyConsoleTasks(host: string, list: readonly ConsoleTaskInfo[]): void {
  const key = hostOf(host);
  const store = useTaskStore.getState();
  const rows = store.tasks.filter((t) => t.kind === CONSOLE_TASK_KIND && t.consoleId === key);
  const seen = notedFor(key);
  const listed = new Set<string>();
  for (const info of list) {
    listed.add(info.id);
    const row = rows.find((t) => taskIdOf(t) === info.id);
    const running = info.state === "running";
    if (row) {
      if (isTerminal(row.status)) continue;
      if (running) {
        store.updateTask(row.id, {
          label: consoleTaskLabel(info),
          progress: consoleTaskProgress(info),
          control: { owner: "console-task", host: key, taskId: info.id, cancellable: info.cancellable },
        });
      } else {
        finish(row.id, info);
      }
      continue;
    }
    if (seen.has(info.id)) continue;
    if (!running && info.age_s > FRESH_END_S) continue;
    // Left the job table with nobody reading (no page open, no app running): nothing to say about it.
    if (info.state === "gone") continue;
    seen.add(info.id);
    const id = store.registerTask({
      kind: CONSOLE_TASK_KIND,
      origin: "console",
      label: consoleTaskLabel(info),
      detail: trStatic("console_task_started_on_ps5", "Started on the PS5"),
      consoleId: key,
      payload: { consoleTaskId: info.id },
      control: { owner: "console-task", host: key, taskId: info.id, cancellable: info.cancellable },
      progress: consoleTaskProgress(info),
    });
    if (!running) finish(id, info);
  }
  // No longer listed while it ran: the helper restarted, or this app did not read for longer than
  // the console keeps a task's ending. It is over, and how is not known.
  for (const row of rows) {
    if (!isTerminal(row.status) && !listed.has(String(taskIdOf(row)))) {
      store.finishTask(row.id, "unverified");
    }
  }
}

const inFlight = new Set<string>();
const unsupportedUntil = new Map<string, number>();

/** One read for AppShell's status tick (and after a cancel). Quiet about a failure: the next tick
 *  reads again, and the tick itself reports a console that stopped answering. */
export async function readConsoleTasks(host: string, now: () => number = Date.now): Promise<void> {
  const key = hostOf(host);
  if (!key || inFlight.has(key) || transferScreenBusy(host)) return;
  if ((unsupportedUntil.get(key) ?? 0) > now()) return;
  inFlight.add(key);
  try {
    const { supported, tasks } = await fetchConsoleTasks(host);
    if (!supported) {
      unsupportedUntil.set(key, now() + UNSUPPORTED_RETRY_MS);
      return;
    }
    unsupportedUntil.delete(key);
    applyConsoleTasks(key, tasks);
  } catch {
    // leave the rows as they are
  } finally {
    inFlight.delete(key);
  }
}

/** Cancel from a row. "Already over" is no failure: the read right after says how it ended. */
export async function cancelConsoleTaskRow(host: string, taskId: string): Promise<void> {
  try {
    await cancelConsoleTask(host, taskId);
  } catch (e) {
    if (!(e instanceof ConsoleTaskError && (e.reason === "unknown_job" || e.reason === "ended"))) throw e;
  } finally {
    await readConsoleTasks(host);
  }
}

/** Tests only. */
export function resetConsoleTasksState(): void {
  noted.clear();
  inFlight.clear();
  unsupportedUntil.clear();
}
