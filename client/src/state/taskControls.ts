import { cancelStaging } from "../lib/browserStaging";
import { cancelConsoleTaskRow } from "./consoleTasks";
import { useFpkgConversion } from "./fpkgConversion";
import { useFsBulkOpStore } from "./fsBulkOp";
import { retryInstallReverify } from "./pkgLibrary";
import { useTransferStore } from "./transfer";
import { useUploadQueueStore } from "./uploadQueue";
import { isTerminal, type Task } from "./tasks";
import { invoke } from "../lib/invokeLogged";
import { trStatic } from "../lib/trStatic";
import { useToastStore } from "./toasts";

export type TaskCommand = "cancel" | "retry";

export interface TaskCapabilities {
  canCancel: boolean;
  canRetry: boolean;
  canPause: false;
  canResume: false;
}

/** Derive controls from the live owning store, not the facade status alone. */
export function taskCapabilities(task: Task): TaskCapabilities {
  const none: TaskCapabilities = {
    canCancel: false,
    canRetry: false,
    canPause: false,
    canResume: false,
  };
  const control = task.control;
  if (!control) return none;

  if (control.owner === "transfer") {
    const phase = useTransferStore.getState().phasesByHost[control.host];
    return { ...none, canCancel: phase?.kind === "running" || phase?.kind === "starting" };
  }
  if (control.owner === "fs-bulk") {
    const op = useFsBulkOpStore.getState().byHost[control.host];
    return { ...none, canCancel: op?.op != null && !op.cancelRequested };
  }
  if (control.owner === "link-download") {
    return { ...none, canCancel: task.status === "running" || task.status === "queued" };
  }
  if (control.owner === "remote-fetch" || control.owner === "browser-staging") {
    return { ...none, canCancel: task.status === "running" };
  }
  if (control.owner === "console-task") {
    return { ...none, canCancel: task.status === "running" && control.cancellable };
  }
  if (control.owner === "fpkg-convert") {
    // Only the run that owns this row: an old Convert row must never stop a newer conversion.
    const p = useFpkgConversion.getState().pipeline;
    return {
      ...none,
      canCancel:
        !isTerminal(task.status) && p.phase === "running" && p.taskId === task.id && p.jobId != null,
    };
  }
  if (control.owner === "pkg-install") {
    // An install Sony accepted but we could not confirm. Nothing to cancel —
    // the install is the console's — but the user can ask us to look again
    // (the Recheck action), which is what "retry" means for this owner.
    return { ...none, canRetry: task.status === "awaiting" };
  }

  const item = useUploadQueueStore.getState().items.find(
    (candidate) => candidate.id === control.itemId,
  );
  if (!item) return none;
  return {
    ...none,
    canCancel: item.status === "running" || item.status === "pending",
    canRetry: item.status === "failed",
  };
}

/** Execute a command against the feature that owns the work. Task status is
 * intentionally not touched here; taskWiring observes the owner's resulting
 * state and updates the facade after the command takes effect. */
export async function commandTask(task: Task, command: TaskCommand): Promise<boolean> {
  const control = task.control;
  if (!control) return false;

  if (command === "cancel") {
    if (control.owner === "transfer") {
      useTransferStore.getState().cancel(control.host);
      return true;
    }
    if (control.owner === "fs-bulk") {
      useFsBulkOpStore.getState().requestCancel(control.host);
      return true;
    }
    if (control.owner === "pkg-install") return false;
    if (control.owner === "fpkg-convert") {
      if (!taskCapabilities(task).canCancel) return false;
      await useFpkgConversion.getState().cancel();
      return true;
    }
    if (control.owner === "link-download") {
      await invoke("pkg_remote_download_cancel", { id: control.downloadId });
      return true;
    }
    if (control.owner === "browser-staging") return cancelStaging(control.stagingId);
    if (control.owner === "remote-fetch") {
      // The fetch stops at its next chunk; materializeRemote then settles the task as cancelled.
      await invoke("job_cancel", { jobId: control.jobId });
      return true;
    }
    if (control.owner === "console-task") {
      try {
        await cancelConsoleTaskRow(control.host, control.taskId);
        return true;
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        useToastStore.getState().push({
          tone: "warn",
          message: trStatic(
            "engine_job_cancel_failed",
            "Couldn't stop it: {msg}. It is still running; try again.",
            { msg },
          ),
        });
        return false;
      }
    }
    useUploadQueueStore.getState().cancelItem(control.itemId);
    return true;
  }

  if (control.owner === "pkg-install") {
    return retryInstallReverify(task);
  }
  if (control.owner !== "upload-queue") return false;
  const retried = useUploadQueueStore.getState().retryItem(control.itemId);
  if (retried) {
    await useUploadQueueStore.getState().startHost(control.host, { onlyIds: [control.itemId] });
  }
  return retried;
}
