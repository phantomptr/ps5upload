import {
  useFsBulkOpStore,
  useFsDownloadOpStore,
  IDLE_BULK,
  IDLE_DOWNLOAD,
} from "./fsBulkOp";
import { useTransferStore, IDLE_PHASE } from "./transfer";
import { useUploadQueueStore } from "./uploadQueue";
import {
  useActivityHistoryStore,
  type ActivityKind,
  type ActivityPhase,
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
    const phases = useTransferStore.getState().phasesByHost;
    if (
      Object.values(phases).some(
        (p) => p.kind === "starting" || p.kind === "running",
      )
    )
      return true;
    if (
      Object.values(useFsDownloadOpStore.getState().byHost).some(
        (d) => d.active,
      )
    )
      return true;
    if (
      useUploadQueueStore.getState().items.some((it) => it.status === "running")
    )
      return true;
    return false;
  };
  const reconcileKeepAwake = () => setTransferKeepAwake(anyTransferActive());
  useTransferStore.subscribe(reconcileKeepAwake);
  useFsDownloadOpStore.subscribe(reconcileKeepAwake);
  useUploadQueueStore.subscribe(reconcileKeepAwake);
  // Subscriptions only fire on CHANGE, so reconcile once now in case a
  // transfer is already in flight when wiring installs (e.g. the upload
  // queue auto-resumed from a hydrate before this ran).
  reconcileKeepAwake();

  // ── Transfer (Upload screen) ──────────────────────────────────────
  // The one-shot transfer store is per-console (phasesByHost), so track the
  // running activity-id PER host (mirrors the upload-queue subscriber's
  // per-item map). Without this, two consoles' one-shots would share a single
  // id and one would overwrite/misattribute the other's bytes.
  const transferActivityIds = new Map<string, string>();
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
      const existingId = transferActivityIds.get(host) ?? null;
      if (phase.kind === "starting") {
        // Open a fresh entry on ANY transition into "starting" — not just
        // idle→starting. A re-upload to the same console after a finished run
        // arrives as done→starting / failed→starting (the phase never returns
        // to idle in between), so the old `prevKind === "idle"` guard created
        // NO entry: the ActivityBar/Activity tab stayed blank with no Stop and
        // no history. The terminal branches already deleted the prior id; if one
        // still lingers (abnormal running→starting), close it as stopped first
        // so it isn't orphaned.
        if (existingId !== null) {
          useActivityHistoryStore.getState().finish(existingId, "stopped", {
            error: "superseded by a new upload",
          });
        }
        transferActivityIds.set(
          host,
          // `addr: host` — the phasesByHost key IS the bare console host.
          // Without it, the entry renders with no console chip and a
          // two-console session can't tell whose upload this is.
          useActivityHistoryStore
            .getState()
            .start("upload", "Upload starting…", { addr: host }),
        );
        continue;
      }
      if (phase.kind === "running" && existingId !== null) {
        // "All bytes on wire, engine still working" = finalize phase.
        // PS5 is committing the manifest / fsyncing inodes; can take
        // many minutes for large file counts. Renderers use this to
        // swap "Uploading 100%" for "Finalizing on PS5…". Gate on
        // totalBytes > 0 so a not-yet-stat'd transfer (where the
        // denominator hasn't arrived) doesn't false-positive as
        // finalizing on its very first tick.
        const newPhase: ActivityPhase =
          phase.totalBytes > 0 && phase.bytesSent >= phase.totalBytes
            ? "finalizing"
            : "uploading";
        useActivityHistoryStore.getState().update(existingId, {
          label:
            phase.files.length > 1
              ? `Uploading ${phase.files.length} files`
              : `Uploading ${phase.files[0]?.rel_path ?? ""}`.trim(),
          bytes: phase.bytesSent,
          totalBytes: phase.totalBytes,
          phase: newPhase,
          // P3 / v2.18.0 — forward the apply-phase counters so the
          // ActivityRow's finalize pill can render "Finalized N of M files".
          // 0/0 on pre-P3 payloads and outside the finalize phase — the
          // ActivityRow already guards on phase === "finalizing" + both
          // values being meaningful, so this is safe to forward unconditionally.
          filesFinalized: phase.filesFinalized,
          filesFinalizingTotal: phase.filesFinalizingTotal,
        });
        continue;
      }
      if (phase.kind === "done" && existingId !== null) {
        useActivityHistoryStore.getState().finish(existingId, "done", {
          bytes: phase.bytesSent,
          detail: phase.dest,
        });
        transferActivityIds.delete(host);
        continue;
      }
      if (phase.kind === "failed" && existingId !== null) {
        useActivityHistoryStore.getState().finish(existingId, "failed", {
          error: phase.error,
        });
        transferActivityIds.delete(host);
        continue;
      }
      if (phase.kind === "idle" && existingId !== null) {
        // Returned to idle without going through done/failed — the
        // user clicked the Stop button on the running banner. The
        // engine job continues server-side; the UI just stopped
        // observing.
        useActivityHistoryStore.getState().finish(existingId, "stopped", {
          error: "stopped by user (engine job may continue)",
        });
        transferActivityIds.delete(host);
        continue;
      }
    }
  });

  // ── FS bulk ops (delete / paste-copy / paste-move) ───────────────
  // PER CONSOLE: the bulk-op store is keyed by host (byHost), so track the
  // running activity-id per host. Without this, two consoles' concurrent
  // bulk ops would share one id and misattribute each other's progress /
  // completion (mirrors transferActivityIds above).
  const bulkActivityIds = new Map<string, string>();
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
      const existingId = bulkActivityIds.get(host) ?? null;
      // Forward per-item byte progress (currentBytesCopied + currentSize)
      // into the activity entry so the ActivityBar / Activity tab tick in
      // lockstep with the in-screen banner.
      if (cur.op !== null && existingId !== null) {
        if (
          cur.currentBytesCopied !== old.currentBytesCopied ||
          cur.currentSize !== old.currentSize
        ) {
          useActivityHistoryStore.getState().update(existingId, {
            bytes: cur.currentBytesCopied,
            totalBytes: cur.currentSize ?? 0,
          });
        }
      }
      if (cur.op === old.op && cur.cancelRequested === old.cancelRequested)
        continue;
      if (cur.op !== null && old.op === null) {
        const kind: ActivityKind =
          cur.op === "delete"
            ? "fs-delete"
            : cur.op === "paste-copy"
              ? "fs-paste-copy"
              : "fs-paste-move";
        const verb =
          cur.op === "delete"
            ? "Deleting"
            : cur.op === "paste-copy"
              ? "Copying"
              : "Moving";
        bulkActivityIds.set(
          host,
          useActivityHistoryStore
            .getState()
            .start(
              kind,
              `${verb} ${cur.total} item${cur.total === 1 ? "" : "s"}`,
              {
                fromPath: cur.fromPath || undefined,
                toPath: cur.toPath || undefined,
                files: cur.total,
                // The byHost key IS the bare console host.
                addr: host || undefined,
              },
            ),
        );
        continue;
      }
      if (cur.op === null && old.op !== null && existingId !== null) {
        const outcome = old.cancelRequested
          ? "stopped"
          : cur.errorBanner
            ? "failed"
            : "done";
        useActivityHistoryStore.getState().finish(existingId, outcome, {
          error: cur.errorBanner ?? undefined,
        });
        bulkActivityIds.delete(host);
      }
    }
  });

  // ── FS downloads ─────────────────────────────────────────────────
  // PER CONSOLE: downloads are keyed by host (byHost), so track the running
  // activity-id per host. Without this, starting a download on console B
  // while console A's was active would reuse A's single id — B's bytes got
  // credited to A's entry and B's download had no record at all.
  const downloadActivityIds = new Map<string, string>();
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
      const existingId = downloadActivityIds.get(host) ?? null;
      if (cur.active && !old.active) {
        downloadActivityIds.set(
          host,
          useActivityHistoryStore
            .getState()
            .start("download", `Downloading ${cur.rootName}`, {
              fromPath: cur.rootSrcPath,
              toPath: cur.destDir,
              // The byHost key IS the bare console host.
              addr: host || undefined,
            }),
        );
        continue;
      }
      if (cur.active && existingId !== null) {
        if (
          cur.bytesReceived !== old.bytesReceived ||
          cur.totalBytes !== old.totalBytes
        ) {
          useActivityHistoryStore.getState().update(existingId, {
            bytes: cur.bytesReceived,
            totalBytes: cur.totalBytes,
          });
        }
        continue;
      }
      if (!cur.active && old.active && existingId !== null) {
        // requestStop() (the Stop button) bumps this host's runId; a natural
        // end() leaves it unchanged. Without this, a user-stopped download
        // would show the same green "done" chip as a completed one, since
        // requestStop() also clears errorBanner.
        const outcome = cur.errorBanner
          ? "failed"
          : cur.runId !== old.runId
            ? "stopped"
            : "done";
        useActivityHistoryStore.getState().finish(existingId, outcome, {
          bytes: old.bytesReceived,
          totalBytes: old.totalBytes,
          error: cur.errorBanner ?? undefined,
        });
        downloadActivityIds.delete(host);
      }
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
            // Which console this queue item targets — drives the console
            // chip on ActivityBar/Activity rows. Same `ip:9113` shape the
            // Activity screen's fsOpCancel path expects (port-tolerant).
            addr: item.addr,
          });
        uploadQueueActivityIds.set(item.id, id);
        continue;
      }
      // Still running — update byte progress so the ActivityBar
      // shows a live byte counter, and derive the finalize-phase
      // signal so the row can flip to "Finalizing on PS5…" once
      // bytes peg at 100%. Same derivation as the single-shot path
      // above; same totalBytes > 0 gate (avoid a stat-pending false
      // positive on the first tick).
      if (isRunning && wasRunning) {
        const activityId = uploadQueueActivityIds.get(item.id);
        const newPhase: ActivityPhase =
          item.totalBytes > 0 && item.bytesSent >= item.totalBytes
            ? "finalizing"
            : "uploading";
        const phaseChanged = prevItem
          ? newPhase !==
            (prevItem.totalBytes > 0 &&
            prevItem.bytesSent >= prevItem.totalBytes
              ? "finalizing"
              : "uploading")
          : true;
        if (
          activityId &&
          (item.bytesSent !== prevItem?.bytesSent ||
            item.totalBytes !== prevItem?.totalBytes ||
            phaseChanged)
        ) {
          useActivityHistoryStore.getState().update(activityId, {
            bytes: item.bytesSent,
            totalBytes: item.totalBytes,
            phase: newPhase,
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
}
