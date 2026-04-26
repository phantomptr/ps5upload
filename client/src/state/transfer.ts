import { create } from "zustand";

import {
  startTransferFile,
  startTransferDir,
  startTransferDirReconcile,
  fsMount,
  jobStatus,
  resumeTxidLookup,
  resumeTxidRemember,
  resumeTxidForget,
  generateTxIdHex,
  type JobSnapshot,
  type PlannedFile,
  type ReconcileMode,
} from "../api/ps5";
import {
  computeRate,
  pushRateSample,
  type RateSample,
} from "../lib/rollingRate";
import type { SourceKind } from "./upload";

/** For folder sources the caller picks one of these strategies at the
 *  moment the dialog is confirmed. Files/exfat images always use
 *  `overwrite` (no reconcile notion for a single blob). */
export type UploadStrategy = "overwrite" | "resume";

/**
 * Upload lifecycle for the Upload screen.
 *
 * idle → starting → running → done | failed
 *
 * `starting` covers the initial POST to /api/transfer/*; once the
 * engine returns a `job_id` we flip to `running` and poll
 * `/api/jobs/{id}` every 500ms for bytes/shards updates. Terminal
 * states (done/failed) stop polling and stay on screen until the user
 * dismisses them (by clicking Upload again or Cancel).
 */
export type TransferPhase =
  | { kind: "idle" }
  | { kind: "starting" }
  | {
      kind: "running";
      jobId: string;
      /** Wall-clock ms when we entered `running`. */
      startedAtMs: number;
      /** Live bytes counter from the engine's 200 ms progress ticker. */
      bytesSent: number;
      /** Total expected bytes (pre-stat of source). May be 0 if the
       *  engine couldn't stat the source before sending began. */
      totalBytes: number;
      /** Bytes/sec, smoothed across the last ~1s of samples. 0 until
       *  we have at least 2 samples. */
      bytesPerSec: number;
      /** Planned file list — used to render the per-file status panel.
       *  Empty until the first tick with a populated list arrives. */
      files: PlannedFile[];
      /** Number of files in `files` whose cumulative byte range is
       *  below `bytesSent` — treat these as "done". Derived; lives
       *  in state so the UI doesn't recompute every render. */
      filesCompleted: number;
      /** Reconcile-mode counters. 0 for plain uploads. */
      skippedFiles: number;
      skippedBytes: number;
    }
  | {
      kind: "done";
      jobId: string;
      bytesSent: number;
      elapsedMs: number;
      dest: string;
      filesSent: number;
      skippedFiles: number;
      skippedBytes: number;
      mountedAt?: string;
    }
  | { kind: "failed"; error: string };

interface StartArgs {
  sourceKind: SourceKind;
  srcPath: string;
  dest: string;
  addr: string;
  /** For folder sources: "overwrite" = traditional re-send-everything,
   *  "resume" = use the reconcile endpoint (skips already-present files).
   *  Ignored for file/exfat sources. */
  strategy?: UploadStrategy;
  /** When strategy === "resume", which equality check to use. */
  reconcileMode?: ReconcileMode;
  /** Folder-only exclude patterns. Empty = upload everything. */
  excludes?: string[];
  /** Image-only: mount the uploaded disk image after the transfer commits. */
  mountAfterUpload?: boolean;
}

interface TransferState {
  phase: TransferPhase;
  start: (args: StartArgs) => Promise<void>;
  reset: () => void;
}

const POLL_INTERVAL_MS = 500;
const POLL_INITIAL_DELAY_MS = 200;

export const useTransferStore = create<TransferState>((set) => {
  // Generation counter: every start() bumps it; any in-flight poll
  // closure captures `thisRun` and pre-gates every state write on
  // `thisRun === runId`. This kills the race where a stale poll's
  // `await jobStatus()` resolves after a new run has started and
  // writes stale "running" over the new "starting" state.
  //
  // A simple boolean `cancel` is *not* enough: the old poll could read
  // cancel=false after start() re-armed it (cancel=true, cancel=false
  // happen synchronously with no awaits between) and keep writing.
  let runId = 0;
  // Handle of the most recently scheduled poll timer. `reset()` clears
  // it so a rapid start/cancel loop doesn't accumulate pending
  // timeouts — each orphan would fire once, hit the `isLive()` guard,
  // and close over the entire poll scope (jobId + samples array) until
  // the event loop processed it.
  let pollTimer: ReturnType<typeof setTimeout> | null = null;

  return {
    phase: { kind: "idle" },

    async start({
      sourceKind,
      srcPath,
      dest,
      addr,
      strategy = "overwrite",
      reconcileMode = "fast",
      excludes = [],
      mountAfterUpload = false,
    }) {
      const thisRun = ++runId;
      set({ phase: { kind: "starting" } });

      const isFolder = sourceKind === "folder" || sourceKind === "game-folder";

      // Helper: is this run still the live one? If the user cancelled
      // or kicked off another upload, we're stale and MUST NOT write
      // state (would clobber whatever the new run set up).
      const isLive = () => thisRun === runId;

      // ── Cross-session tx_id resolution ─────────────────────────────
      // For folder uploads we carry the tx_id ourselves so a Resume
      // click (even after an app restart, within the 24 h TTL) reuses
      // the same tx and the payload's journal can surface
      // last_acked_shard. For file uploads, engine mints a fresh tx_id
      // each time — no Resume UX on single files today, so persisting
      // a tx_id would just create surprise skip-behavior on re-upload.
      const host = hostFromAddr(addr);
      let txId: string | null = null;
      if (isFolder) {
        const persistedMode = strategy === "resume" ? "reconcile" : "dir";
        if (strategy === "overwrite") {
          // Override means "start from scratch" — drop any prior tx_id
          // so the next Resume click doesn't accidentally skip shards
          // from this fresh run's manifest.
          try {
            await resumeTxidForget(host, srcPath, dest);
          } catch (e) {
            // forget() failure is not worth failing the upload over
            // (worst case: an orphan record with a stale tx_id,
            // pruned by TTL within 24 h). But a persistent failure
            // here means cross-session Resume silently breaks
            // forever — log so a user grepping the dev console
            // can find it.
            console.warn("[transfer] resumeTxidForget failed:", e);
          }
          txId = generateTxIdHex();
        } else {
          // strategy === "resume": look up a prior tx_id for this
          // (host, src, dest). Missing or expired → fresh id.
          try {
            txId = await resumeTxidLookup(host, srcPath, dest);
          } catch (e) {
            console.warn(
              "[transfer] resumeTxidLookup failed; treating as fresh upload:",
              e,
            );
            txId = null;
          }
          if (!txId) txId = generateTxIdHex();
        }
        try {
          await resumeTxidRemember(host, srcPath, dest, txId, persistedMode);
        } catch (e) {
          // Persistence failure degrades UX (no cross-session
          // resume) but doesn't break the upload itself — still
          // pass the tx_id in-memory so within-session resume via
          // the engine's retry loop still fires. Log so a
          // permanent failure (corrupt store, perms) surfaces.
          console.warn("[transfer] resumeTxidRemember failed:", e);
        }
      }

      let jobId: string;
      try {
        if (isFolder && strategy === "resume") {
          jobId = await startTransferDirReconcile(
            srcPath,
            dest,
            addr,
            reconcileMode,
            txId,
            excludes,
          );
        } else if (isFolder) {
          jobId = await startTransferDir(srcPath, dest, addr, txId, excludes);
        } else {
          jobId = await startTransferFile(srcPath, dest, addr);
        }
      } catch (e) {
        if (!isLive()) return;
        set({
          phase: {
            kind: "failed",
            error: e instanceof Error ? e.message : String(e),
          },
        });
        return;
      }
      if (!isLive()) return;

      const startedAtMs = Date.now();
      set({
        phase: {
          kind: "running",
          jobId,
          startedAtMs,
          bytesSent: 0,
          totalBytes: 0,
          bytesPerSec: 0,
          files: [],
          filesCompleted: 0,
          skippedFiles: 0,
          skippedBytes: 0,
        },
      });

      // Trailing-window rate smoother. Shared with the queue runner via
      // lib/rollingRate so the two surfaces never disagree on what
      // "smoothed bytes/sec" means.
      const samples: RateSample[] = [{ ts: startedAtMs, bytes: 0 }];

      const poll = async () => {
        if (!isLive()) return;
        let snap: JobSnapshot;
        try {
          snap = await jobStatus(jobId);
        } catch (e) {
          if (!isLive()) return;
          set({
            phase: {
              kind: "failed",
              error: e instanceof Error ? e.message : String(e),
            },
          });
          return;
        }
        if (!isLive()) return;
        if (snap.status === "done") {
          let mountedAt: string | undefined;
          const finalDest = snap.dest ?? dest;
          if (sourceKind === "image" && mountAfterUpload) {
            try {
              const mounted = await fsMount(addr, finalDest);
              mountedAt = mounted.mount_point;
            } catch (e) {
              if (!isLive()) return;
              set({
                phase: {
                  kind: "failed",
                  error: `upload completed, but mount failed: ${
                    e instanceof Error ? e.message : String(e)
                  }`,
                },
              });
              return;
            }
          }
          // Tx is committed on the payload — the tx_id we persisted at
          // start can be evicted, since there's nothing to resume. A
          // lingering record would make a subsequent upload-of-same-
          // folder's Resume click look up a committed tx_id, which the
          // payload's journal may or may not still carry (depending on
          // eviction pressure). Forget proactively.
          if (isFolder) {
            void resumeTxidForget(host, srcPath, dest);
          }
          set({
            phase: {
              kind: "done",
              jobId,
              bytesSent: snap.bytes_sent ?? 0,
              elapsedMs: snap.elapsed_ms ?? 0,
              dest: finalDest,
              filesSent: snap.files_sent ?? 0,
              skippedFiles: snap.skipped_files ?? 0,
              skippedBytes: snap.skipped_bytes ?? 0,
              mountedAt,
            },
          });
        } else if (snap.status === "failed") {
          set({
            phase: {
              kind: "failed",
              error: snap.error ?? "upload failed",
            },
          });
        } else {
          const now = Date.now();
          const bytesSent = snap.bytes_sent ?? 0;
          pushRateSample(samples, now, bytesSent);
          const bytesPerSec = computeRate(samples, now);
          const files = snap.files ?? [];
          // Cumulative-sum the file sizes to find how many have been
          // "completed" — defined as: their cumulative end byte is at
          // or below bytesSent. Close enough for UI: on packed shards,
          // a burst of files all flip to ✓ at once when the shard
          // commits, which is honest.
          let cum = 0;
          let filesCompleted = 0;
          for (const f of files) {
            cum += f.size;
            if (cum <= bytesSent) filesCompleted++;
            else break;
          }
          set({
            phase: {
              kind: "running",
              jobId,
              startedAtMs,
              bytesSent,
              totalBytes: snap.total_bytes ?? 0,
              bytesPerSec,
              files,
              filesCompleted,
              skippedFiles: snap.skipped_files ?? 0,
              skippedBytes: snap.skipped_bytes ?? 0,
            },
          });
          pollTimer = setTimeout(poll, POLL_INTERVAL_MS);
        }
      };
      pollTimer = setTimeout(poll, POLL_INITIAL_DELAY_MS);
    },

    reset() {
      // Bump the generation so any in-flight poll's next state write
      // becomes a no-op. Engine job keeps running in the background —
      // the payload's single-client transfer port means a brand-new
      // upload will wait until that finishes. Wiring a real ABORT_TX
      // cancel is a separate follow-up.
      runId++;
      if (pollTimer !== null) {
        clearTimeout(pollTimer);
        pollTimer = null;
      }
      set({ phase: { kind: "idle" } });
    },
  };
});

/** Pull the host (no port) out of an addr like `192.168.1.2:9113` for
 *  use as the resume-txid cache key. We deliberately key by host, not
 *  full addr, so the port choice doesn't fragment records — a user who
 *  later lands on a payload with a different transfer port should still
 *  be able to resume, because the payload's tx journal is port-agnostic. */
function hostFromAddr(addr: string): string {
  const i = addr.lastIndexOf(":");
  return i < 0 ? addr : addr.slice(0, i);
}
