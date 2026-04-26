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
  end: (errorBanner?: string | null) => void;
  clearError: () => void;
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
    });
  },
  setProgress({ done, currentPath, currentName, currentSize }) {
    set({ done, currentPath, currentName, currentSize });
  },
  end(errorBanner = null) {
    set({ ...idle, errorBanner });
  },
  clearError() {
    set({ errorBanner: null });
  },
}));

/** Same shape for tracked downloads — separate store because
 *  downloads can run concurrently with bulk ops and we want both
 *  visible at once. The download path picks individual files (or a
 *  single tree) so `total` here is "files in the manifest"; current*
 *  fields track which file is being pulled right now. */
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
}

interface DownloadOpActions {
  begin: (params: {
    jobId: string;
    rootName: string;
    rootSrcPath: string;
    destDir: string;
  }) => void;
  setProgress: (params: { bytesReceived: number; totalBytes: number }) => void;
  end: (errorBanner?: string | null) => void;
  clearError: () => void;
}

const downloadIdle: DownloadOpState = {
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
  (set) => ({
    ...downloadIdle,
    begin({ jobId, rootName, rootSrcPath, destDir }) {
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
      });
    },
    setProgress({ bytesReceived, totalBytes }) {
      set({ bytesReceived, totalBytes });
    },
    end(errorBanner = null) {
      set({ ...downloadIdle, errorBanner });
    },
    clearError() {
      set({ errorBanner: null });
    },
  }),
);
