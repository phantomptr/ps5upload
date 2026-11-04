import { useTaskStore } from "../state/tasks";
import { useTransferStore } from "../state/transfer";
import { useUploadQueueStore } from "../state/uploadQueue";
import { hostOf } from "./addr";

/**
 * How many bytes this app has moved to or from `key` (a bare console host) in the work running now:
 * a one-shot upload, the queue's running items, and any task that reports bytes (Files uploads,
 * downloads, copies). Null when nothing running reports a count.
 *
 * Only its changing means anything: bytes moving prove the helper is alive far better than a status
 * probe, which is what the status poll uses this for.
 */
export function bytesMoved(key: string): number | null {
  let total: number | null = null;
  const add = (n: number) => {
    total = (total ?? 0) + n;
  };
  const phase = useTransferStore.getState().phasesByHost[key];
  if (phase?.kind === "running") add(phase.bytesSent);
  for (const it of useUploadQueueStore.getState().items) {
    if (it.status === "running" && hostOf(it.addr) === key) add(it.bytesSent);
  }
  for (const t of useTaskStore.getState().tasks) {
    if (
      t.status === "running" &&
      t.progress?.unit === "bytes" &&
      hostOf(t.consoleId) === key
    ) {
      add(t.progress.current);
    }
  }
  return total;
}
