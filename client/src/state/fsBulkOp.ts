import { create } from "zustand";

import { hostOf } from "../lib/addr";

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
 * PER-HOST (multi-console). Each console runs FULLY independently: the
 * state lives in `byHost[<bare host>]`, so console B can run its own
 * paste/delete/download while console A's is in flight, and neither
 * console's banner, progress, or Stop button touches the other's. This
 * replaces the earlier single-global-op model (one op app-wide, tagged
 * with `host`, that blocked every other console). Reads go through
 * `bulkOpForHost(s, host)`; the screen's async runners use the bound
 * `fsBulkOpHandle(host)` so their bodies stay host-agnostic.
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
  /** Set by the Stop button. The bulk loop forwards the cancel to
   *  the payload via fsOpCancel so the in-flight op (cp_rf for
   *  paste, rm_rf for delete) bails at its next check — within one
   *  4 MiB buffer for copy, between directory entries for delete. */
  cancelRequested: boolean;
  /** Bytes processed for the *current* item, updated by the bulk-loop
   *  poller from FS_OP_STATUS. For paste-copy/paste-move it's bytes
   *  written to the destination; for delete it's bytes freed from
   *  the source. 0 when the poller hasn't seen a reply yet. The
   *  banner uses this + `currentSize` to render a per-item progress
   *  bar. */
  currentBytesCopied: number;
  /** When the current item is being processed via an op_id-tracked
   *  RPC (paste-copy / paste-move / delete), this is the unique
   *  64-bit id the loop generated. Stop button reads it to call
   *  fsOpCancel. null when no op_id-tracked item is in flight. */
  currentOpId: number | null;
}

interface BulkOpStore {
  /** Per-console bulk-op state, keyed by bare host (port-stripped via
   *  `hostOf`). Absent host → idle (see `IDLE_BULK`). */
  byHost: Record<string, BulkOpState>;
  begin: (
    host: string,
    params: {
      op: BulkOpKind;
      total: number;
      fromPath?: string;
      toPath?: string;
    },
  ) => void;
  setProgress: (
    host: string,
    params: {
      done: number;
      currentPath: string;
      currentName: string;
      currentSize: number | null;
    },
  ) => void;
  /** Called by the paste-loop poller as it sees FS_OP_STATUS replies.
   *  Stamps the byte counter for the current item without disturbing
   *  the rest of the progress fields. */
  setCurrentBytesCopied: (host: string, bytes: number) => void;
  /** Caller registers/clears the op_id of the currently-in-flight
   *  copy so the Stop button knows which payload op to cancel. */
  setCurrentOpId: (host: string, opId: number | null) => void;
  end: (host: string, errorBanner?: string | null) => void;
  clearError: (host: string) => void;
  /** Flip the cancel flag for one console. The next iteration of that
   *  console's bulk loop in the screen breaks out and falls through to
   *  the finally block which calls end(). */
  requestCancel: (host: string) => void;
}

/** Stable idle reference — returned by `bulkOpForHost` when a console has
 *  no op. MUST be a singleton (not a fresh object per call) so Zustand
 *  selectors comparing by `Object.is` don't see a new object every render. */
export const IDLE_BULK: BulkOpState = {
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

const keyOf = (host: string | null | undefined): string =>
  host?.trim() ? hostOf(host) : "";

/** Read one console's bulk-op state from a store snapshot. */
export function bulkOpForHost(
  s: { byHost: Record<string, BulkOpState> },
  host: string | null | undefined,
): BulkOpState {
  return s.byHost[keyOf(host)] ?? IDLE_BULK;
}

/** Bare host of some OTHER console that currently has an active bulk op,
 *  or null. Drives the "running on another console" note. Returns a
 *  primitive so selectors stay referentially stable. */
export function otherActiveBulkHost(
  s: { byHost: Record<string, BulkOpState> },
  host: string | null | undefined,
): string | null {
  const me = keyOf(host);
  for (const [h, st] of Object.entries(s.byHost)) {
    if (h !== me && st.op !== null) return h;
  }
  return null;
}

export const useFsBulkOpStore = create<BulkOpStore>((set) => {
  const patch = (host: string, partial: Partial<BulkOpState>) =>
    set((s) => {
      const key = keyOf(host);
      const cur = s.byHost[key] ?? IDLE_BULK;
      return { byHost: { ...s.byHost, [key]: { ...cur, ...partial } } };
    });
  return {
    byHost: {},
    begin(host, { op, total, fromPath = "", toPath = "" }) {
      set((s) => ({
        byHost: {
          ...s.byHost,
          [keyOf(host)]: {
            ...IDLE_BULK,
            op,
            total,
            fromPath,
            toPath,
            startedAtMs: Date.now(),
          },
        },
      }));
    },
    setProgress(host, { done, currentPath, currentName, currentSize }) {
      // Reset per-item byte counter when we move to a new item; the
      // paste-loop poller will re-stamp it from the next FS_OP_STATUS
      // reply. Without the reset, the banner would briefly show the
      // previous item's bytes as the current one's progress.
      patch(host, {
        done,
        currentPath,
        currentName,
        currentSize,
        currentBytesCopied: 0,
      });
    },
    setCurrentBytesCopied(host, bytes) {
      patch(host, { currentBytesCopied: bytes });
    },
    setCurrentOpId(host, opId) {
      patch(host, { currentOpId: opId });
    },
    end(host, errorBanner = null) {
      // Reset this console's slot to idle, preserving any error banner so
      // it stays visible (dismissed via clearError). Other consoles'
      // slots are untouched.
      set((s) => ({
        byHost: { ...s.byHost, [keyOf(host)]: { ...IDLE_BULK, errorBanner } },
      }));
    },
    clearError(host) {
      patch(host, { errorBanner: null });
    },
    requestCancel(host) {
      patch(host, { cancelRequested: true });
    },
  };
});

/** Host-bound view of the bulk-op store for the screen's async runners.
 *  Methods omit the host arg (baked in) and the getters read this host's
 *  slot live, so a runner body reads like the old single-op store while
 *  staying scoped to the console it started on — even if the user switches
 *  tabs mid-run. */
export function fsBulkOpHandle(host: string) {
  const read = () => bulkOpForHost(useFsBulkOpStore.getState(), host);
  return {
    begin: (p: {
      op: BulkOpKind;
      total: number;
      fromPath?: string;
      toPath?: string;
    }) => useFsBulkOpStore.getState().begin(host, p),
    setProgress: (p: {
      done: number;
      currentPath: string;
      currentName: string;
      currentSize: number | null;
    }) => useFsBulkOpStore.getState().setProgress(host, p),
    setCurrentBytesCopied: (b: number) =>
      useFsBulkOpStore.getState().setCurrentBytesCopied(host, b),
    setCurrentOpId: (id: number | null) =>
      useFsBulkOpStore.getState().setCurrentOpId(host, id),
    end: (e?: string | null) => useFsBulkOpStore.getState().end(host, e),
    clearError: () => useFsBulkOpStore.getState().clearError(host),
    requestCancel: () => useFsBulkOpStore.getState().requestCancel(host),
    get op() {
      return read().op;
    },
    get done() {
      return read().done;
    },
    get currentPath() {
      return read().currentPath;
    },
    get currentName() {
      return read().currentName;
    },
    get currentSize() {
      return read().currentSize;
    },
    get cancelRequested() {
      return read().cancelRequested;
    },
    get currentBytesCopied() {
      return read().currentBytesCopied;
    },
    get currentOpId() {
      return read().currentOpId;
    },
  };
}

/** Same per-host model for tracked downloads — separate store because
 *  downloads can run concurrently with bulk ops and we want both
 *  visible at once. The download path picks individual files (or a
 *  single tree) so `total` here is "files in the manifest"; current*
 *  fields track which file is being pulled right now.
 *
 *  Generation counter (`runId`) gives the runner an abort handle:
 *  every begin() bumps it (per host), the runner captures its own value,
 *  and every poll-loop iteration re-checks. `requestStop()` bumps the
 *  counter without resetting other fields, so the runner's next
 *  await boundary observes the abort and tears down cleanly. The
 *  engine job continues on the engine side (no engine cancel API
 *  today); the UI just stops polling and the download eventually
 *  finishes invisibly with the .part promotion happening server-side. */
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
   *  capture the value at begin time and bail when this host's runId
   *  no longer matches. */
  runId: number;
}

interface DownloadOpStore {
  byHost: Record<string, DownloadOpState>;
  begin: (
    host: string,
    params: {
      jobId: string;
      rootName: string;
      rootSrcPath: string;
      destDir: string;
    },
  ) => number;
  setProgress: (
    host: string,
    params: { bytesReceived: number; totalBytes: number },
  ) => void;
  end: (host: string, errorBanner?: string | null) => void;
  clearError: (host: string) => void;
  /** Tear-down request from the UI. Bumps this host's runId so its
   *  active runner (if any) stops polling at the next await; resets
   *  the other fields. */
  requestStop: (host: string) => void;
}

/** Stable idle reference for an absent console's download slot. */
export const IDLE_DOWNLOAD: DownloadOpState = {
  active: false,
  jobId: null,
  rootName: "",
  rootSrcPath: "",
  destDir: "",
  bytesReceived: 0,
  totalBytes: 0,
  startedAtMs: 0,
  errorBanner: null,
  runId: 0,
};

export function downloadForHost(
  s: { byHost: Record<string, DownloadOpState> },
  host: string | null | undefined,
): DownloadOpState {
  return s.byHost[keyOf(host)] ?? IDLE_DOWNLOAD;
}

/** Bare host of some OTHER console with an active download, or null. */
export function otherActiveDownloadHost(
  s: { byHost: Record<string, DownloadOpState> },
  host: string | null | undefined,
): string | null {
  const me = keyOf(host);
  for (const [h, st] of Object.entries(s.byHost)) {
    if (h !== me && st.active) return h;
  }
  return null;
}

export const useFsDownloadOpStore = create<DownloadOpStore>((set, get) => {
  const slot = (host: string) => get().byHost[keyOf(host)] ?? IDLE_DOWNLOAD;
  const put = (host: string, next: DownloadOpState) =>
    set((s) => ({ byHost: { ...s.byHost, [keyOf(host)]: next } }));
  return {
    byHost: {},
    begin(host, { jobId, rootName, rootSrcPath, destDir }) {
      const nextRunId = slot(host).runId + 1;
      put(host, {
        ...IDLE_DOWNLOAD,
        active: true,
        jobId,
        rootName,
        rootSrcPath,
        destDir,
        startedAtMs: Date.now(),
        runId: nextRunId,
      });
      return nextRunId;
    },
    setProgress(host, { bytesReceived, totalBytes }) {
      put(host, { ...slot(host), bytesReceived, totalBytes });
    },
    end(host, errorBanner = null) {
      put(host, { ...IDLE_DOWNLOAD, runId: slot(host).runId, errorBanner });
    },
    clearError(host) {
      put(host, { ...slot(host), errorBanner: null });
    },
    requestStop(host) {
      // Bump runId so the runner's isLive() returns false at its next
      // check; reset everything else so the UI banner clears.
      put(host, { ...IDLE_DOWNLOAD, runId: slot(host).runId + 1 });
    },
  };
});

/** Host-bound view of the download store for the screen's runner. */
export function fsDownloadOpHandle(host: string) {
  const read = () => downloadForHost(useFsDownloadOpStore.getState(), host);
  return {
    begin: (p: {
      jobId: string;
      rootName: string;
      rootSrcPath: string;
      destDir: string;
    }) => useFsDownloadOpStore.getState().begin(host, p),
    setProgress: (p: { bytesReceived: number; totalBytes: number }) =>
      useFsDownloadOpStore.getState().setProgress(host, p),
    end: (e?: string | null) => useFsDownloadOpStore.getState().end(host, e),
    clearError: () => useFsDownloadOpStore.getState().clearError(host),
    requestStop: () => useFsDownloadOpStore.getState().requestStop(host),
    get active() {
      return read().active;
    },
    get runId() {
      return read().runId;
    },
  };
}
