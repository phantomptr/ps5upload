import { create } from "zustand";

import {
  fsMount,
  generateTxIdHex,
  jobStatus,
  startTransferDir,
  startTransferDirReconcile,
  startTransferFile,
  startTransferZip,
  uploadQueueLoad,
  uploadQueueSave,
  UploadJobError,
  type ReconcileMode,
} from "../api/ps5";
import {
  moveItemDown,
  moveItemUp,
  nextPending,
  patchItem,
  removeItem,
  resetFailedToPending,
  resetRunningToPending,
  shouldContinueAfterFailure,
} from "../lib/queueOps";
import {
  averageRate,
  computeRate,
  pushRateSample,
  type RateSample,
} from "../lib/rollingRate";
import type { SourceKind } from "./upload";
import type { UploadStrategy } from "./transfer";
import { useUploadSettingsStore } from "./uploadSettings";
import { useRecentHostMetricsStore } from "./recentHostMetrics";
import { pushNotification } from "./notifications";
import { createRunGen } from "../lib/runGen";

/**
 * Sequential upload queue. Lives in its own Zustand store separate
 * from `useTransferStore` so a queued run doesn't fight with the
 * single-shot manual upload state on the same screen — the user can
 * keep eyeing the live transfer panel while the queue runs the next
 * item in the background.
 *
 * Persisted to a single Tauri JSON document (`upload_queue.json` in
 * app-data). Saves are debounced — a 300 ms idle window after the
 * last mutation collapses bursty reorders into one disk write.
 *
 * The runner is generation-counted: every `start()` bumps `runId`,
 * and the loop checks the live runId between every async await so
 * `stop()` (which just bumps runId) tears the loop down at the next
 * await boundary. Without that, a clicking-stop-mid-poll would still
 * mark the next pending item as running before noticing the cancel.
 */

export type QueueItemStatus = "pending" | "running" | "done" | "failed";

/** One queued upload. The shape is whatever the Upload screen
 *  captures at "Add to queue" time — source path, destination,
 *  strategy, exclude rules — plus runtime status that the runner
 *  updates as it processes the item. */
export interface QueueItem {
  id: string;
  sourceKind: SourceKind;
  sourcePath: string;
  /** Display-only basename so the list row doesn't re-derive it on
   *  every render. */
  displayName: string;
  /** Resolved final on-PS5 path (volume + subpath + basename). The
   *  user picked these on the Upload screen at queue-add time; the
   *  runner sends the file to this exact path. */
  resolvedDest: string;
  /** Transfer-port addr (e.g. `192.168.1.2:9113`). */
  addr: string;
  strategy: UploadStrategy;
  reconcileMode: ReconcileMode;
  excludes: string[];
  /** Image-only: mount the uploaded image after the transfer commits. */
  mountAfterUpload: boolean;
  /** Image-only: when mounting, mount read-only (default true — RO
   *  prevents the PS5 from silently writing save-data into the image
   *  and corrupting it on next mount). */
  mountReadOnly: boolean;
  /** Stable tx_id for this queue item, minted at add-time and
   *  persisted alongside the item. Used so a queue interrupted by
   *  app restart can resume against the payload's existing journal
   *  entry instead of orphaning the in-flight tx and starting fresh.
   *  Folder uploads use TX_FLAG_RESUME with this id; file uploads
   *  ignore it (single-file resume isn't wired payload-side today). */
  txIdHex: string;
  status: QueueItemStatus;
  /** Live progress while running, final count when done, 0 otherwise. */
  bytesSent: number;
  /** Total bytes the engine pre-stat'd for this source. 0 until first
   *  Running tick lands. */
  totalBytes: number;
  /** Smoothed bytes/sec while running (trailing 2 s window via
   *  `lib/rollingRate`); set to the wall-clock average bytes/sec on
   *  done; 0 when pending or failed. Persisted with the queue so the
   *  done-row average survives an app restart and stays comparable
   *  across runs. */
  bytesPerSec: number;
  /** P3 / v2.18.0 — apply-phase counters forwarded from JobSnapshot.
   *  Surface a "Finalized N of M files" pill on the queue row during
   *  the post-100% commit-apply phase. Both 0 outside the finalize
   *  phase and on pre-P3 payloads that don't emit APPLY_PROGRESS. */
  filesFinalized: number;
  filesFinalizingTotal: number;
  /** Mount path the runner produced when `mountAfterUpload` is true and
   *  the image upload + mount succeeded. Surfaced to the row so users
   *  see where the image landed without flipping to the Volumes tab. */
  mountedAt: string | null;
  /** Non-fatal warnings the post-upload mount surfaced — layout
   *  invalid (no sce_sys/param.json at root), kernel forced RO, etc.
   *  Pre-2.2.52 these warnings only appeared when the user mounted
   *  via the Library tab; the upload-then-mount path silently
   *  swallowed them so users with `mountAfterUpload` got no feedback
   *  about an image that mounted successfully but won't register. */
  mountWarnings: string[];
  error: string | null;
  /** Payload-side error category, when the failure originated from a
   *  PS5 protocol error frame (e.g. `direct_writer_io_error`,
   *  `fs_write_failed_errno_28`). Used by the UI to render a
   *  humanized hint via `humanizeJobErrorReason`. null for failures
   *  that didn't come from the payload (local I/O, connection refuse). */
  errorReason: string | null;
  /** Free-form human-readable detail string from the payload's error
   *  frame `"detail"` field. Shown as the secondary line under the
   *  humanized hint when present. */
  errorDetail: string | null;
  addedAt: number;
  startedAt: number | null;
  completedAt: number | null;
}

/** Subset of `QueueItem` that the caller supplies; the store fills in
 *  id + addedAt + status + counters. */
export type AddQueueItem = Pick<
  QueueItem,
  | "sourceKind"
  | "sourcePath"
  | "displayName"
  | "resolvedDest"
  | "addr"
  | "strategy"
  | "reconcileMode"
  | "excludes"
  | "mountAfterUpload"
  | "mountReadOnly"
>;

interface QueueState {
  items: QueueItem[];
  /** When false, runner stops at the first failure. When true, it
   *  marks the failed item and moves to the next pending. */
  continueOnFailure: boolean;
  /** True while the runner loop is active (between start() and the
   *  loop exiting either by completion or stop()). */
  running: boolean;
  /** True after the first hydrate() completes. Lets the UI distinguish
   *  "no items yet" from "still loading from disk." */
  loaded: boolean;

  hydrate: () => Promise<void>;
  add: (item: AddQueueItem) => void;
  remove: (id: string) => void;
  moveUp: (id: string) => void;
  moveDown: (id: string) => void;
  clear: () => void;
  retryFailed: () => void;
  setContinueOnFailure: (b: boolean) => void;
  start: () => Promise<void>;
  stop: () => void;
}

interface QueueDocument {
  items: QueueItem[];
  continueOnFailure: boolean;
}

const POLL_INTERVAL_MS = 500;
const SAVE_DEBOUNCE_MS = 300;

function newId(): string {
  // 32-char hex from crypto UUID (same trick as generateTxIdHex).
  return crypto.randomUUID().replace(/-/g, "");
}

export const useUploadQueueStore = create<QueueState>((set, get) => {
  // Generation counter: every start() bumps it. The runner loop
  // captures its own generation and bails between awaits when the
  // global runId moves on. Stop() just bumps the counter; running:false
  // is set by the loop when it notices.
  // 2.12.0: extracted to lib/runGen (shared with transfer.ts +
  // payloadPlaylists.ts). installQueue.ts intentionally keeps its
  // store-field runId because it needs atomic increment-and-claim
  // via functional set — different semantics.
  const gen = createRunGen();
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  /** Schedule a debounced whole-document save. Idempotent — multiple
   *  calls within 300 ms collapse into one fsync. The runner can
   *  legitimately fire a half-dozen patches per second (bytes_sent
   *  updates), and we don't want to round-trip Tauri/disk on each. */
  const scheduleSave = () => {
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      const { items, continueOnFailure } = get();
      const doc: QueueDocument = { items, continueOnFailure };
      void uploadQueueSave(doc).catch((e) => {
        // Persistence failure means the queue won't survive an app
        // restart. Log so it surfaces in the dev console + the
        // engine startup log on Windows; users debugging "my queue
        // disappeared" can find this. A toast-level UI surface is
        // future work — would need a new error channel that's
        // throttled (we save on every mutation, so a transient
        // disk-full would otherwise spam toasts).
        console.error("[upload-queue] save failed:", e);
      });
    }, SAVE_DEBOUNCE_MS);
  };

  /** Run a single queued item to terminal state. Returns when the
   *  engine job hits done; throws on failure (caller decides whether
   *  to continue or stop). The poll loop re-checks `isLive()` after
   *  every await — `stop()` mid-poll exits cleanly without writing
   *  stale state. */
  const runOne = async (
    item: QueueItem,
    isLive: () => boolean,
  ): Promise<{
    bytesSent: number;
    bytesPerSec: number;
    mountedAt: string | null;
    mountWarnings: string[];
  }> => {
    const isFolder =
      item.sourceKind === "folder" || item.sourceKind === "game-folder";
    const isArchive = item.sourceKind === "archive";

    let jobId: string;
    if (isArchive) {
      // A .zip is decompressed host-side and streamed in (lands extracted).
      // Carry the persisted tx_id for cross-session shard resume, just like
      // folders; there's no reconcile mode (no local tree to diff).
      const bandwidthCap =
        useUploadSettingsStore.getState().bandwidthCapMbps;
      jobId = await startTransferZip(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.txIdHex,
        item.excludes,
        bandwidthCap,
      );
    } else if (isFolder && item.strategy === "resume") {
      // Pass the persisted tx_id so a Resume after app restart
      // picks up the payload's existing journal entry instead of
      // minting a fresh tx and re-sending everything.
      const bandwidthCap =
        useUploadSettingsStore.getState().bandwidthCapMbps;
      jobId = await startTransferDirReconcile(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.reconcileMode,
        item.txIdHex,
        item.excludes,
        bandwidthCap,
      );
    } else if (isFolder) {
      const bandwidthCap =
        useUploadSettingsStore.getState().bandwidthCapMbps;
      jobId = await startTransferDir(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.txIdHex,
        item.excludes,
        bandwidthCap,
      );
    } else {
      // Single-file uploads now thread the persisted txIdHex through
      // too. The engine sets TX_FLAG_RESUME when a caller-supplied
      // tx_id is present, which is a no-op on the very first attempt
      // (payload doesn't know the id yet, falls through to fresh-
      // allocate) but lets a subsequent attempt for the SAME queue
      // item — typically after wifi-drop retries are exhausted and the
      // user clicks "Retry / Resume" — pick up from the payload's
      // last-acked shard instead of restarting from zero. Same pattern
      // as folder uploads.
      jobId = await startTransferFile(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
        item.txIdHex,
      );
    }

    // Trailing-window samples for the live bytes/sec readout. Closure-
    // scoped so a Stop + restart of the same item resets cleanly: the
    // next runOne builds a fresh array.
    const startedAtMs = Date.now();
    const samples: RateSample[] = [{ ts: startedAtMs, bytes: 0 }];

    while (isLive()) {
      const snap = await jobStatus(jobId);
      if (!isLive()) {
        throw new Error("queue stopped");
      }
      if (snap.status === "done") {
        let mountedAt: string | null = null;
        const mountWarnings: string[] = [];
        // Re-check liveness before initiating the mount. Without this,
        // a Stop click between the engine's done-snapshot arrival and
        // the fsMount call would let the mount happen on the PS5
        // anyway — start() then sees `!isLive()` after the await and
        // skips patching the item, leaving the row stuck at "pending"
        // while a real mount sits on /mnt/ps5upload/. Skip-and-return
        // here keeps the user's mental model consistent: Stop = stop.
        if (
          isLive() &&
          item.sourceKind === "image" &&
          item.mountAfterUpload
        ) {
          try {
            // Mount point lives next to the source file (same logic as
            // transfer.ts): strip the image extension from the resolved
            // destination so /data/homebrew/MyGame.ffpkg mounts at
            // /data/homebrew/MyGame/. Source + mount discoverable by
            // every PS5 manager that scans /data/homebrew/.
            const finalDest = snap.dest ?? item.resolvedDest;
            const mountPoint = finalDest.replace(/\.(exfat|ffpkg|ffpfs)$/i, "");
            const mounted = await fsMount(
              item.addr,
              finalDest,
              { mountPoint, readOnly: item.mountReadOnly },
            );
            mountedAt = mounted.mount_point;
            // Surface non-fatal mount diagnostics — same warnings the
            // Library row's Mount button shows. Pre-2.2.52 this path
            // dropped them silently, so an upload-then-mount user with
            // a misshapen .ffpkg got "all green" feedback for an image
            // that wouldn't register or wouldn't accept writes.
            if (mounted.layout_valid === false) {
              mountWarnings.push(
                "Image is missing sce_sys/param.json at root — Register/Launch will fail. Re-build the image with files at root (no extra folder).",
              );
            }
            if (mounted.kernel_ro && !item.mountReadOnly) {
              mountWarnings.push(
                "Kernel mounted this read-only despite the RW pick — common for UFS .ffpkg images on some firmwares. Reads work; writes through the mount will fail.",
              );
            }
          } catch (e) {
            const wrapped = new Error(
              `upload completed, but mount failed: ${
                e instanceof Error ? e.message : String(e)
              }`,
            );
            // Preserve the original error so downstream consumers can
            // inspect the underlying mount failure (eslint's
            // preserve-caught-error rule enforces this).
            (wrapped as Error & { cause?: unknown }).cause = e;
            throw wrapped;
          }
        }
        // Final readout = total bytes / total elapsed. Prefer the
        // engine's elapsed_ms (measured payload-side) over a wall-
        // clock diff so a slow first poll doesn't skew the average.
        const finalBytes = snap.bytes_sent ?? 0;
        const elapsedMs = snap.elapsed_ms ?? Date.now() - startedAtMs;
        // Persist this host's measured throughput so the next upload's
        // pre-flight ETA banner can use a sharper number. Same shape
        // as transfer.ts's recording site; queue uploads were the
        // primary user-reported pain point (huge folders) so this
        // path matters most. commitMsPerFile is left undefined until
        // P3's APPLY_PROGRESS frames give us the apply-time signal.
        //
        // Known bias (same as transfer.ts): `snap.elapsed_ms`
        // includes the post-100% PS5 commit phase, so a 50-min
        // upload that was 30 min transfer + 20 min apply records as
        // ~55 MiB/s instead of the actual ~90 MiB/s transfer rate.
        // The next banner estimate is then conservatively long, which
        // is the right direction for UX. Sharper accounting waits for
        // P3 APPLY_PROGRESS — see review notes surface C1.
        if (finalBytes > 0 && elapsedMs > 0) {
          const throughputMibps =
            finalBytes / 1024 / 1024 / (elapsedMs / 1000);
          useRecentHostMetricsStore.getState().record(item.addr, {
            throughputMibps,
            measuredAtMs: Date.now(),
          });
        }
        return {
          bytesSent: finalBytes,
          bytesPerSec: averageRate(finalBytes, elapsedMs),
          mountedAt,
          mountWarnings,
        };
      }
      if (snap.status === "failed") {
        // Throw the *structured* error so start()'s catch can lift
        // error_reason/error_detail onto the item — the queue row's
        // humanized hint (humanizeJobErrorReason, e.g. "PS5 ran out of
        // space — click Retry to resume") depends on these. A plain
        // Error here drops the reason, so the row showed only the raw
        // {"error":…,"detail":…} blob the single-shot path avoids.
        throw new UploadJobError(
          snap.error ?? "upload failed",
          snap.error_reason,
          snap.error_detail,
        );
      }
      // Still running — push live progress + smoothed rate into the
      // item so the row shows a moving bar + speed without an extra
      // round-trip from the renderer.
      const now = Date.now();
      const bytesSent = snap.bytes_sent ?? 0;
      pushRateSample(samples, now, bytesSent);
      const bytesPerSec = computeRate(samples, now);
      set((s) => ({
        items: patchItem(s.items, item.id, {
          bytesSent,
          totalBytes: snap.total_bytes ?? 0,
          bytesPerSec,
          // P3 / v2.18.0 — forward apply-phase counters so the
          // QueueRow's finalize pill can show "Finalized N of M files"
          // (analogous to transfer.ts's single-shot path). Defaults to
          // 0 when the engine doesn't surface them (pre-P3 payloads
          // OR outside the finalize phase).
          filesFinalized: snap.files_finalized ?? 0,
          filesFinalizingTotal: snap.files_finalizing_total ?? 0,
        }),
      }));
      await sleep(POLL_INTERVAL_MS);
    }
    throw new Error("queue stopped");
  };

  return {
    items: [],
    continueOnFailure: false,
    running: false,
    loaded: false,

    async hydrate() {
      // Browser-only dev/test contexts: Tauri invoke is unavailable.
      // Mark loaded with the empty in-memory state and skip the call,
      // otherwise every Upload screen mount logs an "invoke undefined"
      // error to the user-visible logs. In production (Tauri), this
      // guard is a no-op.
      const w = window as unknown as { isTauri?: boolean; __TAURI_INTERNALS__?: unknown };
      if (!w.isTauri && !w.__TAURI_INTERNALS__) {
        set({ loaded: true });
        return;
      }
      try {
        const doc = await uploadQueueLoad<Partial<QueueDocument>>();
        // Sanitise on load:
        // - any item "running" when the app closed is stranded
        //   (engine restarted with no memory of the job) — reset to
        //   pending so the user can re-Start the queue.
        // - back-fill txIdHex for items written by an older build
        //   (pre-fix); a missing tx_id on a folder upload would
        //   crash the runner. Mint a fresh one — those items lose
        //   resume continuity (acceptable since they pre-date the
        //   feature) but they won't crash.
        const items = (doc.items ?? []).map((it) => {
          const next = { ...it };
          if (next.status === "running") next.status = "pending";
          if (!next.txIdHex) next.txIdHex = generateTxIdHex();
          // Back-fill the bytes/sec field added in 2.2.22 — older
          // persisted docs don't carry it. Treat unknown as 0 so the
          // UI doesn't show NaN MiB/s on the first render after
          // upgrade.
          if (typeof next.bytesPerSec !== "number") next.bytesPerSec = 0;
          // Back-fill the mountWarnings field added in 2.2.52 — older
          // persisted docs don't carry it. Default to empty so the UI
          // can blindly read .mountWarnings.length without optional-
          // chaining at every site.
          if (!Array.isArray(next.mountWarnings)) next.mountWarnings = [];
          // Back-fill the structured-error fields added when payload
          // failure-reason surfacing landed: older docs only carry the
          // flat `error` string; null these so the UI doesn't read
          // undefined and crash on `.startsWith` etc.
          if (next.errorReason === undefined) next.errorReason = null;
          if (next.errorDetail === undefined) next.errorDetail = null;
          // Back-fill v2.18.0 apply-progress counters — pre-2.18 saves
          // don't carry these. Default to 0 so the QueueRow's
          // optional chain on the finalize pill stays safe.
          if (typeof next.filesFinalized !== "number") next.filesFinalized = 0;
          if (typeof next.filesFinalizingTotal !== "number") next.filesFinalizingTotal = 0;
          return next;
        });
        set({
          items,
          continueOnFailure: doc.continueOnFailure ?? false,
          loaded: true,
        });
      } catch (e) {
        // load_json_or_default returns {} on missing file, so this
        // catch only fires on real corruption (bad JSON, IO error,
        // mutex poison). Don't silently treat that as "empty" — the
        // user might have a recoverable file. Log so it shows up in
        // engine.log and surface a banner via runStatus alongside
        // the empty queue.
        console.error("[upload-queue] hydrate failed:", e);
        set({ loaded: true });
      }
    },

    add(input) {
      const item: QueueItem = {
        id: newId(),
        ...input,
        // Mint the tx_id at add time, not at start time, so the
        // value persists across app restarts. A queued item that
        // ran partway, app crashed, app reopens → next start of the
        // queue passes this same tx_id with TX_FLAG_RESUME and the
        // payload picks up from last_acked_shard.
        txIdHex: generateTxIdHex(),
        status: "pending",
        bytesSent: 0,
        totalBytes: 0,
        bytesPerSec: 0,
        filesFinalized: 0,
        filesFinalizingTotal: 0,
        mountedAt: null,
        mountWarnings: [],
        error: null,
        errorReason: null,
        errorDetail: null,
        addedAt: Date.now(),
        startedAt: null,
        completedAt: null,
      };
      set((s) => ({ items: s.items.concat(item) }));
      scheduleSave();
    },

    remove(id) {
      set((s) => ({ items: removeItem(s.items, id) }));
      scheduleSave();
    },

    moveUp(id) {
      set((s) => ({ items: moveItemUp(s.items, id) }));
      scheduleSave();
    },

    moveDown(id) {
      set((s) => ({ items: moveItemDown(s.items, id) }));
      scheduleSave();
    },

    clear() {
      // If an item is mid-transfer, the engine job + the real PS5-side
      // write keep running after we wipe the queue (the transfer port is
      // single-client, so the next upload will block behind it until it
      // finishes). Surface that instead of going silent — otherwise the
      // user clicks Clear, the UI empties, and a subsequent upload
      // mysteriously stalls behind the orphaned transfer. Mirrors the
      // documented reset() caveat in transfer.ts.
      const inFlight = get().items.find((it) => it.status === "running");
      if (inFlight) {
        pushNotification(
          "info",
          "Queue cleared — one upload is still finishing",
          {
            body: `"${inFlight.displayName}" is already transferring to the PS5 and will run to completion. The next upload waits until it's done.`,
          },
        );
      }
      // Bumps the gen counter so any in-flight run exits at next await.
      gen.next();
      set({ items: [], running: false });
      scheduleSave();
    },

    retryFailed() {
      set((s) => ({ items: resetFailedToPending(s.items) }));
      scheduleSave();
    },

    setContinueOnFailure(b) {
      set({ continueOnFailure: b });
      scheduleSave();
    },

    async start() {
      if (get().running) return;
      const myRun = gen.next();
      const isLive = () => gen.isLive(myRun);
      set({ running: true });
      try {
        while (isLive()) {
          const next = nextPending(get().items);
          if (!next) break;

          const startedAt = Date.now();
          set((s) => ({
            items: patchItem(s.items, next.id, {
              status: "running",
              startedAt,
              // Reset live counters so a previously-failed-then-retried
              // item starts the bar + speed readout from zero instead
              // of inheriting the stale terminal values.
              bytesSent: 0,
              totalBytes: 0,
              bytesPerSec: 0,
              error: null,
              errorReason: null,
              errorDetail: null,
            }),
          }));
          scheduleSave();

          try {
            const { bytesSent, bytesPerSec, mountedAt, mountWarnings } =
              await runOne(next, isLive);
            // Always flip to "done" once runOne returns success — the
            // upload + (optional) mount are committed PS5-side, and
            // resetting the row to "pending" via resetRunningToPending
            // would silently lie: the next Start would re-upload + try
            // to re-mount, hitting EBUSY at mount time and wasting the
            // bytes already on the console. Pre-2.2.52 a Stop landing
            // between runOne's success and this `set` produced exactly
            // that phantom-pending state. Honesty > liveness here:
            // record the committed work and let the user re-process
            // the queue if they want to skip the row.
            set((s) => ({
              items: patchItem(s.items, next.id, {
                status: "done",
                bytesSent,
                bytesPerSec,
                mountedAt,
                mountWarnings,
                completedAt: Date.now(),
              }),
            }));
            scheduleSave();
            if (!isLive()) return;
          } catch (e) {
            if (!isLive()) return;
            const message = e instanceof Error ? e.message : String(e);
            // Lift the structured payload error fields onto the item
            // if waitForJob's thrown error carries them. UI uses these
            // to render a humanized hint via `humanizeJobErrorReason`
            // — without the structured fields the user just sees the
            // raw chain (which often ends in {"error":"…","detail":"…"}
            // JSON that's hard to read in a queue row).
            const reason =
              e instanceof UploadJobError ? (e.reason ?? null) : null;
            const detail =
              e instanceof UploadJobError ? (e.detail ?? null) : null;
            set((s) => ({
              items: patchItem(s.items, next.id, {
                status: "failed",
                bytesPerSec: 0,
                error: message,
                errorReason: reason,
                errorDetail: detail,
                completedAt: Date.now(),
              }),
            }));
            scheduleSave();
            if (!shouldContinueAfterFailure(get().continueOnFailure)) {
              break;
            }
          }
        }
      } finally {
        if (isLive()) {
          set({ running: false });
        }
      }
    },

    stop() {
      // Bump generation; runner exits at the next await. Items left
      // in "running" state get reset to pending on the next hydrate
      // (or by a fresh start, which moves them to running again
      // before re-issuing the engine call — idempotent for the
      // payload because TX_FLAG_RESUME and same-tx_id semantics are
      // independent of queue state).
      gen.next();
      set((s) => ({
        running: false,
        items: resetRunningToPending(s.items),
      }));
      scheduleSave();
    },
  };
});

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
