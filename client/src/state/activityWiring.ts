import { useFsBulkOpStore, useFsDownloadOpStore } from "./fsBulkOp";
import { useTransferStore } from "./transfer";
import { useUploadQueueStore } from "./uploadQueue";
import { useInstallQueue } from "./installQueue";
import {
  useActivityHistoryStore,
  type ActivityKind,
} from "./activityHistory";
import { setTransferKeepAwake } from "../lib/keepAwakeHold";

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

  // ── Keep-awake while transfers run ───────────────────────────────
  // A long upload/download/install must not die to the computer idle-
  // sleeping mid-stream. Derive a single "is any transfer in flight"
  // signal straight from store state (not the activity-history
  // bookkeeping below, so the two concerns stay independent) and
  // reconcile the OS sleep inhibitor on every relevant change. The hold
  // is edge-collapsed + best-effort in `setTransferKeepAwake`, and the
  // Rust side refcounts it separately from the manual Settings toggle.
  const anyTransferActive = (): boolean => {
    const phase = useTransferStore.getState().phase.kind;
    if (phase === "starting" || phase === "running") return true;
    if (useFsDownloadOpStore.getState().active) return true;
    if (useUploadQueueStore.getState().items.some((it) => it.status === "running"))
      return true;
    if (useInstallQueue.getState().items.some((it) => it.status === "running"))
      return true;
    return false;
  };
  const reconcileKeepAwake = () => setTransferKeepAwake(anyTransferActive());
  useTransferStore.subscribe(reconcileKeepAwake);
  useFsDownloadOpStore.subscribe(reconcileKeepAwake);
  useUploadQueueStore.subscribe(reconcileKeepAwake);
  useInstallQueue.subscribe(reconcileKeepAwake);
  // Subscriptions only fire on CHANGE, so reconcile once now in case a
  // transfer is already in flight when wiring installs (e.g. the upload
  // queue auto-resumed from a hydrate before this ran).
  reconcileKeepAwake();

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
    // into the activity entry so the ActivityBar / Activity tab tick
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

  // ── Upload Queue (2.11.0 — was missing) ─────────────────────────
  //
  // Before this subscription, clicking Start on the Upload Queue
  // panel kicked off a real transfer in the engine but the
  // ActivityBar at the bottom of the app stayed dark — the only
  // activityHistory subscriber for FTX2 uploads was useTransferStore
  // (the single-shot Upload-screen flow). Users had to look in the
  // Upload-screen QueuePanel to see queue progress; navigating away
  // hid it entirely. Now the per-item running state forwards into
  // activityHistory so the ActivityBar lights up regardless of which
  // surface kicked off the upload.
  //
  // Sequential queue semantics: at most one item is `running` at a
  // time. We track the active item's activity id by item id (not a
  // single global ref) because the queue runner can advance to the
  // next item between renders and we need to clean up the prior id
  // before starting a new one.
  const uploadQueueActivityIds = new Map<string, string>();
  useUploadQueueStore.subscribe((state, prev) => {
    if (state.items === prev.items) return;
    const prevById = new Map(prev.items.map((it) => [it.id, it]));
    for (const item of state.items) {
      const prevItem = prevById.get(item.id);
      const wasRunning = prevItem?.status === "running";
      const isRunning = item.status === "running";
      // Started running this tick — open an activity entry.
      if (isRunning && !wasRunning) {
        const id = useActivityHistoryStore
          .getState()
          .start("upload-queue", `Queue: ${item.displayName}`, {
            fromPath: item.sourcePath,
            toPath: item.resolvedDest,
          });
        uploadQueueActivityIds.set(item.id, id);
        continue;
      }
      // Still running — update byte progress so the ActivityBar
      // shows a live byte counter.
      if (isRunning && wasRunning) {
        const activityId = uploadQueueActivityIds.get(item.id);
        if (
          activityId &&
          (item.bytesSent !== prevItem?.bytesSent ||
            item.totalBytes !== prevItem?.totalBytes)
        ) {
          useActivityHistoryStore.getState().update(activityId, {
            bytes: item.bytesSent,
            totalBytes: item.totalBytes,
          });
        }
        continue;
      }
      // Transitioned out of running — finish with the right outcome.
      if (!isRunning && wasRunning) {
        const activityId = uploadQueueActivityIds.get(item.id);
        if (activityId) {
          // uploadQueue has no "cancelled" terminal state — stop()
          // flips running items back to "pending" via runId bump.
          // So we only see done/failed/back-to-pending here. Map
          // pending-after-running to "stopped" (user clicked stop).
          const outcome: "done" | "failed" | "stopped" =
            item.status === "done"
              ? "done"
              : item.status === "failed"
                ? "failed"
                : "stopped";
          useActivityHistoryStore.getState().finish(activityId, outcome, {
            bytes: item.bytesSent,
            error: item.errorReason ?? undefined,
          });
          uploadQueueActivityIds.delete(item.id);
        }
      }
    }
    // Items removed entirely (clear()) — flush any orphaned activity
    // ids as "stopped" so the ActivityBar doesn't show a phantom
    // running entry.
    for (const [itemId, activityId] of uploadQueueActivityIds) {
      if (!state.items.some((it) => it.id === itemId)) {
        useActivityHistoryStore.getState().finish(activityId, "stopped", {
          error: "removed from queue",
        });
        uploadQueueActivityIds.delete(itemId);
      }
    }
  });

  // ── Install Queue (2.11.0 — was missing) ────────────────────────
  //
  // Same shape as Upload Queue subscription above. Different store,
  // different item type (InstallQueueItem has bytesDownloaded vs
  // QueueItem's bytesSent), but the begin/update/finish lifecycle is
  // identical. A user starting an install batch with the Upload
  // screen open never saw ActivityBar updates for the install
  // progress — same root cause as the upload-queue gap. NPXS system
  // installs jump immediately to `done` because their actual install
  // happens off-app (Sony's notification panel), so they show as a
  // brief "Install: <title>" entry in the ActivityBar that
  // immediately finishes — which is the correct signal.
  const installQueueActivityIds = new Map<string, string>();
  useInstallQueue.subscribe((state, prev) => {
    if (state.items === prev.items) return;
    const prevById = new Map(prev.items.map((it) => [it.id, it]));
    for (const item of state.items) {
      const prevItem = prevById.get(item.id);
      const wasRunning = prevItem?.status === "running";
      const isRunning = item.status === "running";
      if (isRunning && !wasRunning) {
        const id = useActivityHistoryStore
          .getState()
          .start("install-queue", `Install: ${item.displayName}`, {
            fromPath: item.pkgPath,
            files: 1,
          });
        installQueueActivityIds.set(item.id, id);
        continue;
      }
      if (isRunning && wasRunning) {
        const activityId = installQueueActivityIds.get(item.id);
        if (
          activityId &&
          (item.bytesDownloaded !== prevItem?.bytesDownloaded ||
            item.totalBytes !== prevItem?.totalBytes)
        ) {
          useActivityHistoryStore.getState().update(activityId, {
            bytes: item.bytesDownloaded,
            totalBytes: item.totalBytes,
          });
        }
        continue;
      }
      if (!isRunning && wasRunning) {
        const activityId = installQueueActivityIds.get(item.id);
        if (activityId) {
          const outcome =
            item.status === "done"
              ? "done"
              : item.status === "cancelled"
                ? "stopped"
                : "failed";
          useActivityHistoryStore.getState().finish(activityId, outcome, {
            bytes: item.bytesDownloaded,
            error: item.errMessage ?? undefined,
          });
          installQueueActivityIds.delete(item.id);
        }
      }
    }
    for (const [itemId, activityId] of installQueueActivityIds) {
      if (!state.items.some((it) => it.id === itemId)) {
        useActivityHistoryStore.getState().finish(activityId, "stopped", {
          error: "removed from queue",
        });
        installQueueActivityIds.delete(itemId);
      }
    }
  });
}
