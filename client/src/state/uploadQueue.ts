import { create } from "zustand";

import {
  fsMount,
  generateTxIdHex,
  jobStatus,
  startTransferDir,
  startTransferDirReconcile,
  startTransferFile,
  uploadQueueLoad,
  uploadQueueSave,
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
  let runId = 0;
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

    let jobId: string;
    if (isFolder && item.strategy === "resume") {
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
      // Single-file uploads don't have a cross-session resume flow
      // payload-side today (the engine mints its own tx_id), so the
      // persisted txIdHex is unused here. Kept on the item anyway so
      // the schema stays uniform across kinds.
      jobId = await startTransferFile(
        item.sourcePath,
        item.resolvedDest,
        item.addr,
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
        return {
          bytesSent: finalBytes,
          bytesPerSec: averageRate(finalBytes, elapsedMs),
          mountedAt,
          mountWarnings,
        };
      }
      if (snap.status === "failed") {
        throw new Error(snap.error ?? "upload failed");
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
        mountedAt: null,
        mountWarnings: [],
        error: null,
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
      // Bumps runId so any in-flight run exits at the next await.
      runId++;
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
      const myRun = ++runId;
      const isLive = () => runId === myRun;
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
            set((s) => ({
              items: patchItem(s.items, next.id, {
                status: "failed",
                bytesPerSec: 0,
                error: message,
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
      runId++;
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
