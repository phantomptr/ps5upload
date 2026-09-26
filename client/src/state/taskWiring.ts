/**
 * Task wiring (v5 §10).
 *
 * Subscribes to the per-feature stores (transfer, uploadQueue, fsBulkOp,
 * fsDownloadOp) and mirrors begin/progress/end transitions into the
 * unified `useTaskStore`. This is the bridge between the existing v4
 * per-feature job tracking and the v5 unified Task envelope.
 *
 * Kept as a separate module (mirroring `activityWiring.ts`) so:
 *   - The per-feature stores stay focused and don't need to know about
 *     the Task store (no import cycle).
 *   - The Task store is the only place that needs to know the union of
 *     all op kinds — adding a new op kind here doesn't require editing
 *     the existing stores.
 *   - activityHistory and taskStore evolve independently — activity is
 *     append-only history; tasks have lifecycle (queued/paused/retry).
 *
 * Called once at app startup from layout/AppShell (same place as
 * installActivityWiring). Subscriptions are process-lifetime.
 */

import {
  useFsBulkOpStore,
  useFsDownloadOpStore,
  IDLE_BULK,
  IDLE_DOWNLOAD,
} from "./fsBulkOp";
import { installLibraryTaskBridge } from "./libraryTaskBridge";
import { useTransferStore, IDLE_PHASE } from "./transfer";
import { useUploadQueueStore } from "./uploadQueue";
import { useTaskStore, type TaskKind } from "./tasks";
import { hostOf } from "../lib/addr";

let installed = false;

export function installTaskWiring() {
  if (installed) return;
  installed = true;

  // ── Transfer (Upload screen — one-shot) ───────────────────────────
  // Per-host tracking: the transfer store is keyed by bare host in
  // phasesByHost. We track the active task id PER host so two consoles'
  // one-shot uploads don't share/clobber a single task.
  const transferTaskIds = new Map<string, string>();
  useTransferStore.subscribe((state, prev) => {
    if (state.phasesByHost === prev.phasesByHost) return;
    const hosts = new Set([
      ...Object.keys(state.phasesByHost),
      ...Object.keys(prev.phasesByHost),
    ]);
    for (const host of hosts) {
      const phase = state.phasesByHost[host] ?? IDLE_PHASE;
      const prevPhase = prev.phasesByHost[host] ?? IDLE_PHASE;
      if (phase === prevPhase) continue;
      const existingId = transferTaskIds.get(host) ?? null;

      if (phase.kind === "starting") {
        // Close any lingering prior task (abnormal running→starting).
        if (existingId !== null) {
          useTaskStore.getState().finishTask(existingId, "cancelled", {
            lastError: {
              code: "SUPERSEDED",
              message: "superseded by a new upload",
              recoverable: false,
            },
          });
        }
        const id = useTaskStore.getState().registerTask({
          kind: "upload-file",
          origin: "transfer.one-shot",
          label: "Upload starting…",
          consoleId: host,
          status: "running",
          control: { owner: "transfer", host },
        });
        transferTaskIds.set(host, id);
        continue;
      }

      if (phase.kind === "running" && existingId !== null) {
        const label =
          phase.files.length > 1
            ? `Uploading ${phase.files.length} files`
            : `Uploading ${phase.files[0]?.rel_path ?? ""}`.trim();
        useTaskStore.getState().updateTask(existingId, {
          label,
          progress: {
            current: phase.bytesSent,
            total: phase.totalBytes,
            unit: "bytes",
          },
          engineJobId: phase.jobId,
        });
        continue;
      }

      if (phase.kind === "done" && existingId !== null) {
        useTaskStore.getState().finishTask(existingId, "done", {
          progress: {
            current: phase.bytesSent,
            total: phase.bytesSent,
            unit: "bytes",
          },
          detail: phase.dest,
        });
        transferTaskIds.delete(host);
        continue;
      }

      if (phase.kind === "failed" && existingId !== null) {
        useTaskStore.getState().finishTask(existingId, "failed", {
          lastError: {
            code: "TRANSFER_ERROR",
            message: phase.error ?? "transfer failed",
            recoverable: true,
          },
        });
        transferTaskIds.delete(host);
        continue;
      }

      if (phase.kind === "idle" && existingId !== null) {
        // Returned to idle without done/failed — user clicked Stop.
        useTaskStore.getState().finishTask(existingId, "cancelled", {
          lastError: {
            code: "USER_CANCELLED",
            message: "stopped by user (engine job may continue)",
            recoverable: false,
          },
        });
        transferTaskIds.delete(host);
        continue;
      }
    }
  });

  // ── Upload Queue ──────────────────────────────────────────────────
  // Sequential queue semantics: at most one item is `running` per console
  // at a time. We track the active task by item id so the runner can
  // advance to the next item between renders.
  const queueTaskIds = new Map<string, string>();
  useUploadQueueStore.subscribe((state, prev) => {
    if (state.items === prev.items) return;
    const prevById = new Map(prev.items.map((it) => [it.id, it]));
    for (const item of state.items) {
      const prevItem = prevById.get(item.id);
      const wasRunning = prevItem?.status === "running";
      const isRunning = item.status === "running";

      if (isRunning && !wasRunning) {
        // Determine the kind: archive uploads (.zip/.7z/.rar) get a
        // distinct kind from plain file/folder uploads so the Tasks
        // tab can group them. SourceKind = "archive" covers all
        // archive formats; "folder"/"game-folder" → upload-dir;
        // everything else → upload-file.
        const kind: TaskKind =
          item.sourceKind === "archive"
            ? "upload-archive"
            : item.sourceKind === "folder" || item.sourceKind === "game-folder"
              ? "upload-dir"
              : "upload-file";
        const id = useTaskStore.getState().registerTask({
          kind,
          origin: "uploadQueue",
          label: `Queue: ${item.displayName}`,
          detail: item.resolvedDest,
          consoleId: hostOf(item.addr),
          status: "running",
          payload: {
            queueItemId: item.id,
            sourcePath: item.sourcePath,
            resolvedDest: item.resolvedDest,
          },
          control: {
            owner: "upload-queue",
            host: hostOf(item.addr),
            itemId: item.id,
          },
        });
        queueTaskIds.set(item.id, id);
        continue;
      }

      if (isRunning && wasRunning) {
        const taskId = queueTaskIds.get(item.id);
        if (
          taskId &&
          (item.bytesSent !== prevItem?.bytesSent ||
            item.totalBytes !== prevItem?.totalBytes)
        ) {
          useTaskStore.getState().updateTask(taskId, {
            progress: {
              current: item.bytesSent,
              total: item.totalBytes,
              unit: "bytes",
            },
          });
        }
        continue;
      }

      if (!isRunning && wasRunning) {
        const taskId = queueTaskIds.get(item.id);
        if (taskId) {
          // uploadQueue's stop() flips running items back to "pending"
          // (no explicit "cancelled" terminal), so map pending-after-
          // running to "cancelled".
          if (item.status === "done") {
            useTaskStore.getState().finishTask(taskId, "done", {
              progress: {
                current: item.bytesSent,
                total: item.totalBytes,
                unit: "bytes",
              },
            });
          } else if (item.status === "failed") {
            useTaskStore.getState().finishTask(taskId, "failed", {
              lastError: {
                code: "QUEUE_ERROR",
                message: item.errorReason ?? "upload failed",
                recoverable: true,
              },
            });
          } else {
            // pending-after-running = user clicked stop.
            useTaskStore.getState().finishTask(taskId, "cancelled", {
              lastError: {
                code: "USER_CANCELLED",
                message: "stopped by user",
                recoverable: false,
              },
            });
          }
          queueTaskIds.delete(item.id);
        }
      }
    }
    // Items removed entirely (clear()) — flush orphaned task ids.
    for (const [itemId, taskId] of queueTaskIds) {
      if (!state.items.some((it) => it.id === itemId)) {
        useTaskStore.getState().finishTask(taskId, "cancelled", {
          lastError: {
            code: "REMOVED",
            message: "removed from queue",
            recoverable: false,
          },
        });
        queueTaskIds.delete(itemId);
      }
    }
  });

  // ── FS bulk ops (delete / paste-copy / paste-move) ────────────────
  // Per-host tracking (byHost key = bare console host).
  const bulkTaskIds = new Map<string, string>();
  useFsBulkOpStore.subscribe((state, prev) => {
    if (state.byHost === prev.byHost) return;
    const hosts = new Set([
      ...Object.keys(state.byHost),
      ...Object.keys(prev.byHost),
    ]);
    for (const host of hosts) {
      const cur = state.byHost[host] ?? IDLE_BULK;
      const old = prev.byHost[host] ?? IDLE_BULK;
      if (cur === old) continue;
      const existingId = bulkTaskIds.get(host) ?? null;

      // Forward per-item byte progress.
      if (cur.op !== null && existingId !== null) {
        if (
          cur.currentBytesCopied !== old.currentBytesCopied ||
          cur.currentSize !== old.currentSize
        ) {
          useTaskStore.getState().updateTask(existingId, {
            progress: {
              current: cur.currentBytesCopied,
              total: cur.currentSize ?? 0,
              unit: "bytes",
            },
          });
        }
      }

      if (cur.op === old.op && cur.cancelRequested === old.cancelRequested)
        continue;

      if (cur.op !== null && old.op === null) {
        const kind: TaskKind =
          cur.op === "delete"
            ? "fs-delete"
            : cur.op === "paste-copy"
              ? "fs-copy"
              : "fs-move";
        const verb =
          cur.op === "delete"
            ? "Deleting"
            : cur.op === "paste-copy"
              ? "Copying"
              : "Moving";
        const id = useTaskStore.getState().registerTask({
          kind,
          origin: "files.bulk",
          label: `${verb} ${cur.total} item${cur.total === 1 ? "" : "s"}`,
          detail: cur.toPath || cur.fromPath || undefined,
          consoleId: host,
          status: "running",
          progress: {
            current: 0,
            total: cur.total,
            unit: "items",
          },
          control: { owner: "fs-bulk", host },
        });
        bulkTaskIds.set(host, id);
        continue;
      }

      if (cur.op === null && old.op !== null && existingId !== null) {
        // cancelRequested → cancelled; errorBanner → failed; else done.
        if (old.cancelRequested) {
          useTaskStore.getState().finishTask(existingId, "cancelled", {
            lastError: {
              code: "USER_CANCELLED",
              message: "cancelled by user",
              recoverable: false,
            },
          });
        } else if (cur.errorBanner) {
          useTaskStore.getState().finishTask(existingId, "failed", {
            lastError: {
              code: "FS_OP_ERROR",
              message: cur.errorBanner,
              recoverable: true,
            },
          });
        } else {
          useTaskStore.getState().finishTask(existingId, "done");
        }
        bulkTaskIds.delete(host);
      }
    }
  });

  // ── FS downloads ──────────────────────────────────────────────────
  // Per-host tracking (byHost key = bare console host).
  const downloadTaskIds = new Map<string, string>();
  useFsDownloadOpStore.subscribe((state, prev) => {
    if (state.byHost === prev.byHost) return;
    const hosts = new Set([
      ...Object.keys(state.byHost),
      ...Object.keys(prev.byHost),
    ]);
    for (const host of hosts) {
      const cur = state.byHost[host] ?? IDLE_DOWNLOAD;
      const old = prev.byHost[host] ?? IDLE_DOWNLOAD;
      if (cur === old) continue;
      const existingId = downloadTaskIds.get(host) ?? null;

      if (cur.active && !old.active) {
        const id = useTaskStore.getState().registerTask({
          kind: "download",
          origin: "files.download",
          label: `Downloading ${cur.rootName}`,
          detail: cur.destDir,
          consoleId: host,
          status: "running",
          payload: {
            fromPath: cur.rootSrcPath,
            toPath: cur.destDir,
          },
        });
        downloadTaskIds.set(host, id);
        continue;
      }

      if (cur.active && existingId !== null) {
        if (
          cur.bytesReceived !== old.bytesReceived ||
          cur.totalBytes !== old.totalBytes
        ) {
          useTaskStore.getState().updateTask(existingId, {
            progress: {
              current: cur.bytesReceived,
              total: cur.totalBytes,
              unit: "bytes",
            },
          });
        }
        continue;
      }

      if (!cur.active && old.active && existingId !== null) {
        // runId changed → user stopped; errorBanner → failed; else done.
        if (cur.errorBanner) {
          useTaskStore.getState().finishTask(existingId, "failed", {
            lastError: {
              code: "DOWNLOAD_ERROR",
              message: cur.errorBanner,
              recoverable: true,
            },
          });
        } else if (cur.runId !== old.runId) {
          useTaskStore.getState().finishTask(existingId, "cancelled", {
            lastError: {
              code: "USER_CANCELLED",
              message: "stopped by user",
              recoverable: false,
            },
          });
        } else {
          useTaskStore.getState().finishTask(existingId, "done");
        }
        downloadTaskIds.delete(host);
      }
    }
  });

  // ── Library actions (activity log only) ───────────────────────────
  installLibraryTaskBridge();
}
