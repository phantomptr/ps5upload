import { create } from "zustand";

/**
 * In-flight FileSystem bulk-operation state, lifted out of
 * `FileSystemScreen` so it survives navigation.
 *
 * Why a Zustand store instead of component-local state:
 * the async `runPaste` / `runDelete` / `runDownload` loops in the
 * screen are not tied to component lifecycle — they keep iterating
 * even when the user clicks Volumes mid-run. With local state, the
 * unmount loses the progress card, the operation finishes invisibly,
 * and a remount shows the post-paste view as if nothing had happened.
 * With a store, the screen's progress card hydrates from whatever the
 * still-running async task set last, so the in-flight op is visible
 * on every visit.
 *
 * Single in-flight op at a time (we never start a second paste
 * mid-paste); represented as `op | null`.
 */

/** FS bulk-op kinds that drive the BulkOpBanner. Downloads are
 *  tracked in their own store (`useFsDownloadOpStore`) since they
 *  have a separate progress shape (bytes vs file-count) and can
 *  run concurrently with bulk delete/move/copy. */
export type BulkOpKind = "delete" | "paste-move" | "paste-copy";

export interface BulkOpState {
  /** Kind of op currently in flight, or null when idle. */
  op: BulkOpKind | null;
  /** Total items the op will process. */
  total: number;
  /** Items completed so far. */
  done: number;
  /** Path of the item the op is currently working on. */
  currentPath: string;
  /** Display name of the item the op is currently working on. */
  currentName: string;
  /** Size in bytes of the current item, or null when unknown
   *  (e.g. dir size on the PS5 isn't surfaced via list_dir). */
  currentSize: number | null;
  /** Source path the op is reading from — useful for cross-mount
   *  moves where the from/to distinction matters to the user. */
  fromPath: string;
  /** Destination path the op is writing to. */
  toPath: string;
  /** Wall-clock ms when the op started. UI uses this to render
   *  elapsed time without subscribing to a tick. */
  startedAtMs: number;
  /** When non-null, the op is broadcasting an error message (used
   *  for partial failures after the op has moved past the failed
   *  item). Cleared by the screen when surfaced or dismissed. */
  errorBanner: string | null;
  /** Set by the Stop button. For paste-copy / paste-move the loop
   *  also forwards the cancel to the payload via fsOpCancel so
   *  the in-flight cp_rf bails within one 4 MiB buffer; for delete
   *  the granularity is "between items" because fs_delete is
   *  single-shot and not interruptible. */
  cancelRequested: boolean;
  /** Bytes copied for the *current* item, updated by the paste-loop
   *  poller from FS_OP_STATUS. 0 when the op kind doesn't expose
   *  byte-level progress (i.e. delete) or the poller hasn't seen a
   *  reply yet. The banner uses this + `currentSize` to render a
   *  per-item progress bar and speed indicator. */
  currentBytesCopied: number;
  /** When the current item is being processed via an op_id-tracked
   *  RPC (paste-copy / paste-move), this is the unique 64-bit id
   *  the loop generated. Stop button reads it to call fsOpCancel.
   *  null when no op_id-tracked item is in flight. */
  currentOpId: number | null;
}

interface BulkOpActions {
  begin: (params: {
    op: BulkOpKind;
    total: number;
    fromPath?: string;
    toPath?: string;
  }) => void;
  setProgress: (params: {
    done: number;
    currentPath: string;
    currentName: string;
    currentSize: number | null;
  }) => void;
  /** Called by the paste-loop poller as it sees FS_OP_STATUS replies.
   *  Stamps the byte counter for the current item without disturbing
   *  the rest of the progress fields. */
  setCurrentBytesCopied: (bytes: number) => void;
  /** Caller registers/clears the op_id of the currently-in-flight
   *  copy so the Stop button knows which payload op to cancel. */
  setCurrentOpId: (opId: number | null) => void;
  end: (errorBanner?: string | null) => void;
  clearError: () => void;
  /** Flip the cancel flag. The next iteration of the bulk loop
   *  in the screen breaks out and falls through to the finally
   *  block which calls end(). */
  requestCancel: () => void;
}

const idle: BulkOpState = {
  op: null,
  total: 0,
  done: 0,
  currentPath: "",
  currentName: "",
  currentSize: null,
  fromPath: "",
  toPath: "",
  startedAtMs: 0,
  errorBanner: null,
  cancelRequested: false,
  currentBytesCopied: 0,
  currentOpId: null,
};

export const useFsBulkOpStore = create<BulkOpState & BulkOpActions>((set) => ({
  ...idle,
  begin({ op, total, fromPath = "", toPath = "" }) {
    set({
      op,
      total,
      done: 0,
      currentPath: "",
      currentName: "",
      currentSize: null,
      fromPath,
      toPath,
      startedAtMs: Date.now(),
      errorBanner: null,
      cancelRequested: false,
      currentBytesCopied: 0,
      currentOpId: null,
    });
  },
  setProgress({ done, currentPath, currentName, currentSize }) {
    // Reset per-item byte counter when we move to a new item; the
    // paste-loop poller will re-stamp it from the next FS_OP_STATUS
    // reply. Without the reset, the banner would briefly show the
    // previous item's bytes as the current one's progress.
    set({
      done,
      currentPath,
      currentName,
      currentSize,
      currentBytesCopied: 0,
    });
  },
  setCurrentBytesCopied(bytes) {
    set({ currentBytesCopied: bytes });
  },
  setCurrentOpId(opId) {
    set({ currentOpId: opId });
  },
  end(errorBanner = null) {
    set({ ...idle, errorBanner });
  },
  clearError() {
    set({ errorBanner: null });
  },
  requestCancel() {
    set({ cancelRequested: true });
  },
}));

/** Same shape for tracked downloads — separate store because
 *  downloads can run concurrently with bulk ops and we want both
 *  visible at once. The download path picks individual files (or a
 *  single tree) so `total` here is "files in the manifest"; current*
 *  fields track which file is being pulled right now.
 *
 *  Generation counter (`runId`) gives the runner an abort handle:
 *  every begin() bumps it, the runner captures its own value, and
 *  every poll-loop iteration re-checks. `requestStop()` bumps the
 *  counter without resetting other fields, so the runner's next
 *  await boundary observes the abort and tears down cleanly. The
 *  engine job continues on the engine side (no engine cancel API
 *  today); the UI just stops polling and the download eventually
 *  finishes invisibly with the .part promotion happening server-
 *  side. */
export interface DownloadOpState {
  active: boolean;
  jobId: string | null;
  rootName: string;
  rootSrcPath: string;
  destDir: string;
  bytesReceived: number;
  totalBytes: number;
  startedAtMs: number;
  errorBanner: string | null;
  /** Bumped by begin() and requestStop(). Runner closures should
   *  capture the value at begin time and bail when getState().runId
   *  no longer matches. */
  runId: number;
}

interface DownloadOpActions {
  begin: (params: {
    jobId: string;
    rootName: string;
    rootSrcPath: string;
    destDir: string;
  }) => number;
  setProgress: (params: { bytesReceived: number; totalBytes: number }) => void;
  end: (errorBanner?: string | null) => void;
  clearError: () => void;
  /** Tear-down request from the UI. Bumps runId so the active
   *  runner (if any) stops polling at the next await; resets other
   *  fields. */
  requestStop: () => void;
}

const downloadIdle: Omit<DownloadOpState, "runId"> = {
  active: false,
  jobId: null,
  rootName: "",
  rootSrcPath: "",
  destDir: "",
  bytesReceived: 0,
  totalBytes: 0,
  startedAtMs: 0,
  errorBanner: null,
};

export const useFsDownloadOpStore = create<DownloadOpState & DownloadOpActions>(
  (set, get) => ({
    ...downloadIdle,
    runId: 0,
    begin({ jobId, rootName, rootSrcPath, destDir }) {
      const nextRunId = get().runId + 1;
      set({
        active: true,
        jobId,
        rootName,
        rootSrcPath,
        destDir,
        bytesReceived: 0,
        totalBytes: 0,
        startedAtMs: Date.now(),
        errorBanner: null,
        runId: nextRunId,
      });
      return nextRunId;
    },
    setProgress({ bytesReceived, totalBytes }) {
      set({ bytesReceived, totalBytes });
    },
    end(errorBanner = null) {
      set((s) => ({ ...downloadIdle, runId: s.runId, errorBanner }));
    },
    clearError() {
      set({ errorBanner: null });
    },
    requestStop() {
      // Bump runId so the runner's isLive() returns false at its
      // next check; reset everything else so the UI banner clears.
      set((s) => ({ ...downloadIdle, runId: s.runId + 1 }));
    },
  }),
);
