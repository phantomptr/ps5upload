import { useFsDownloadOpStore } from "./fsBulkOp";
import { useTransferStore } from "./transfer";
import { useUploadQueueStore } from "./uploadQueue";
import { anyAwakeWork } from "../lib/awakeWork";
import { useTaskStore } from "./tasks";

/** A transfer, download or install is going on: anything the app must not be closed under. */
export function anyWorkRunning(): boolean {
  const phases = useTransferStore.getState().phasesByHost;
  if (Object.values(phases).some((p) => p.kind === "starting" || p.kind === "running")) return true;
  if (Object.values(useFsDownloadOpStore.getState().byHost).some((d) => d.active)) return true;
  if (useUploadQueueStore.getState().items.some((it) => it.status === "running")) return true;
  // Installs (stream, link, archive, upload), copies and builds: every one registers a task.
  return anyAwakeWork(useTaskStore.getState().tasks);
}

/** A name for what is running, to say what would stop; null when there is no good one. */
export function runningWorkName(): string | null {
  return useUploadQueueStore.getState().items.find((it) => it.status === "running")?.displayName ?? null;
}
