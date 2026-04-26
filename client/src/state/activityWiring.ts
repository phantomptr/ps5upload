import { useFsBulkOpStore, useFsDownloadOpStore } from "./fsBulkOp";
import { useTransferStore } from "./transfer";
import {
  useActivityHistoryStore,
  type ActivityKind,
} from "./activityHistory";

/**
 * Subscribes to the per-feature stores (transfer, FS bulk, FS
 * download) and forwards begin/progress/end transitions into the
 * activity history store. Kept as a separate module rather than
 * embedded in each per-feature store so:
 *
 *   - The per-feature stores stay focused on their own concerns and
 *     don't need to know about the activity log (no import cycle).
 *   - The activity log is the only place that needs to know the union
 *     of all op kinds — adding a new op kind here doesn't require
 *     editing the existing stores.
 *
 * Called once at app startup from layout/AppShell. Subscriptions are
 * process-lifetime; there's no unmount sequence because the stores
 * themselves outlive any React tree.
 */

let installed = false;

export function installActivityWiring() {
  if (installed) return;
  installed = true;

  // ── Transfer (Upload screen) ──────────────────────────────────────
  // Tracks the running activity-id across phase transitions so we
  // can finish it correctly on done/failed/idle. `null` when no
  // upload is in flight.
  let transferActivityId: string | null = null;
  useTransferStore.subscribe((state, prev) => {
    if (state.phase === prev.phase) return;
    const phase = state.phase;
    const prevKind = prev.phase.kind;
    if (phase.kind === "starting" && prevKind === "idle") {
      transferActivityId = useActivityHistoryStore
        .getState()
        .start("upload", "Upload starting…");
      return;
    }
    if (phase.kind === "running" && transferActivityId !== null) {
      useActivityHistoryStore.getState().update(transferActivityId, {
        label: phase.files.length > 1
          ? `Uploading ${phase.files.length} files`
          : `Uploading ${phase.files[0]?.rel_path ?? ""}`.trim(),
        bytes: phase.bytesSent,
        totalBytes: phase.totalBytes,
      });
      return;
    }
    if (phase.kind === "done" && transferActivityId !== null) {
      useActivityHistoryStore.getState().finish(transferActivityId, "done", {
        bytes: phase.bytesSent,
        detail: phase.dest,
      });
      transferActivityId = null;
      return;
    }
    if (phase.kind === "failed" && transferActivityId !== null) {
      useActivityHistoryStore.getState().finish(transferActivityId, "failed", {
        error: phase.error,
      });
      transferActivityId = null;
      return;
    }
    if (phase.kind === "idle" && transferActivityId !== null) {
      // Returned to idle without going through done/failed — the
      // user clicked the Stop button on the running banner. The
      // engine job continues server-side; the UI just stopped
      // observing.
      useActivityHistoryStore.getState().finish(transferActivityId, "stopped", {
        error: "stopped by user (engine job may continue)",
      });
      transferActivityId = null;
      return;
    }
  });

  // ── FS bulk ops (delete / paste-copy / paste-move) ───────────────
  let bulkActivityId: string | null = null;
  useFsBulkOpStore.subscribe((state, prev) => {
    // Forward per-item byte progress (currentBytesCopied + currentSize)
    // into the activity entry so the OperationBar / Activity tab tick
    // in lockstep with the in-screen banner. Without this, FS pastes
    // showed "Running" with no bytes for the whole op duration.
    if (state.op !== null && bulkActivityId !== null) {
      if (
        state.currentBytesCopied !== prev.currentBytesCopied ||
        state.currentSize !== prev.currentSize
      ) {
        useActivityHistoryStore.getState().update(bulkActivityId, {
          bytes: state.currentBytesCopied,
          totalBytes: state.currentSize ?? 0,
        });
      }
    }
    if (state.op === prev.op && state.cancelRequested === prev.cancelRequested) return;
    if (state.op !== null && prev.op === null) {
      const kind: ActivityKind =
        state.op === "delete"
          ? "fs-delete"
          : state.op === "paste-copy"
            ? "fs-paste-copy"
            : "fs-paste-move";
      const verb =
        state.op === "delete"
          ? "Deleting"
          : state.op === "paste-copy"
            ? "Copying"
            : "Moving";
      bulkActivityId = useActivityHistoryStore
        .getState()
        .start(kind, `${verb} ${state.total} item${state.total === 1 ? "" : "s"}`, {
          fromPath: state.fromPath || undefined,
          toPath: state.toPath || undefined,
          files: state.total,
        });
      return;
    }
    if (state.op === null && prev.op !== null && bulkActivityId !== null) {
      const outcome = prev.cancelRequested
        ? "stopped"
        : state.errorBanner
          ? "failed"
          : "done";
      useActivityHistoryStore.getState().finish(bulkActivityId, outcome, {
        error: state.errorBanner ?? undefined,
      });
      bulkActivityId = null;
    }
  });

  // ── FS downloads ─────────────────────────────────────────────────
  let downloadActivityId: string | null = null;
  useFsDownloadOpStore.subscribe((state, prev) => {
    if (state.active === prev.active && state.bytesReceived === prev.bytesReceived) {
      return;
    }
    if (state.active && !prev.active) {
      downloadActivityId = useActivityHistoryStore
        .getState()
        .start("download", `Downloading ${state.rootName}`, {
          fromPath: state.rootSrcPath,
          toPath: state.destDir,
        });
      return;
    }
    if (state.active && downloadActivityId !== null) {
      useActivityHistoryStore.getState().update(downloadActivityId, {
        bytes: state.bytesReceived,
        totalBytes: state.totalBytes,
      });
      return;
    }
    if (!state.active && prev.active && downloadActivityId !== null) {
      const outcome = state.errorBanner ? "failed" : "done";
      useActivityHistoryStore.getState().finish(downloadActivityId, outcome, {
        bytes: prev.bytesReceived,
        totalBytes: prev.totalBytes,
        error: state.errorBanner ?? undefined,
      });
      downloadActivityId = null;
    }
  });
}
