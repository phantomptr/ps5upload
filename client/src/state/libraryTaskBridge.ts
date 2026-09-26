// Library actions (mount, move, delete, …) report only to the activity log. Mirror them into the
// task store so the activity bar sees them. Only `library-*` kinds are bridged: uploads and file
// operations already reach the task store through taskWiring, and bridging them would count them
// twice.

import { useActivityHistoryStore, type ActivityEntry } from "./activityHistory";
import { useTaskStore, type TaskKind } from "./tasks";

const SPECIFIC: ReadonlySet<string> = new Set([
  "library-mount",
  "library-register",
  "library-unregister",
  "library-launch",
]);

// A library install already registers its own install task (runPkgInstall); mirroring its log
// entry too would count every install twice.
const SKIP: ReadonlySet<string> = new Set(["library-install"]);

function kindFor(kind: string): TaskKind {
  if (kind === "library-download") return "download";
  return (SPECIFIC.has(kind) ? kind : "library-op") as TaskKind;
}

let installed = false;

export function installLibraryTaskBridge(): void {
  if (installed) return;
  installed = true;
  // activity entry id → task id
  const linked = new Map<string, string>();
  const seen = new Map<string, ActivityEntry>();

  const sync = (entries: ActivityEntry[]) => {
    const tasks = useTaskStore.getState();
    for (const e of entries) {
      if (!e.kind.startsWith("library-") || SKIP.has(e.kind)) continue;
      const prev = seen.get(e.id);
      if (prev === e) continue;
      seen.set(e.id, e);
      let taskId = linked.get(e.id);
      if (!taskId) {
        // An entry first seen already finished was restored from a previous run; the Activity
        // screen owns that history.
        if (e.outcome !== "running") continue;
        taskId = tasks.registerTask({
          kind: kindFor(e.kind),
          origin: "library",
          label: e.label,
          detail: e.detail,
          consoleId: e.addr ?? "",
        });
        linked.set(e.id, taskId);
      }
      if (e.outcome === "running") {
        if (e.totalBytes && e.totalBytes > 0) {
          tasks.updateTask(taskId, {
            progress: { current: e.bytes ?? 0, total: e.totalBytes, unit: "bytes" },
          });
        }
        continue;
      }
      if (e.outcome === "done") tasks.finishTask(taskId, "done");
      else if (e.outcome === "stopped") tasks.finishTask(taskId, "cancelled");
      else
        tasks.finishTask(taskId, "failed", {
          lastError: {
            code: "LIBRARY_OP_FAILED",
            message: e.error ?? "failed",
            recoverable: false,
          },
        });
      linked.delete(e.id);
    }
  };

  sync(useActivityHistoryStore.getState().entries);
  useActivityHistoryStore.subscribe((s) => sync(s.entries));
}
