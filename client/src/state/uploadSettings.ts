import { create } from "zustand";
import type { ReconcileMode } from "../api/ps5";

/**
 * User preferences that govern the upload flow's defaults — what to do
 * when a destination already has content, and how strict the reconcile
 * should be when the user chooses "Resume".
 *
 * Persisted to localStorage so the settings survive restarts. Keyed on
 * a single namespace per concern — if the schema evolves, bump the key
 * (never silently migrate, the cost is one re-pick from the user).
 */

const KEY_ALWAYS_OVERWRITE = "ps5upload.always_overwrite";
const KEY_RECONCILE_MODE = "ps5upload.reconcile_mode";
const KEY_SHOW_FILES = "ps5upload.show_transfer_files";

function loadAlwaysOverwrite(): boolean {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem(KEY_ALWAYS_OVERWRITE) === "true";
}

function loadReconcileMode(): ReconcileMode {
  // Safe mode was removed -- it hashed every remote file via BLAKE3
  // which took hours on large libraries and could destabilize the
  // payload. Per-shard BLAKE3 verification during actual upload
  // already catches mismatches, so size-only is a safe shortcut.
  // Ignore any stale "safe" in localStorage and always return "fast".
  return "fast";
}

function loadShowFiles(): boolean {
  if (typeof window === "undefined") return true;
  const v = window.localStorage.getItem(KEY_SHOW_FILES);
  // Default ON — most users want to see what's happening.
  return v === null ? true : v === "true";
}

interface UploadSettingsState {
  /** When true, the Upload flow skips the Override/Resume/Cancel
   *  dialog and always overwrites the destination. Useful for
   *  iterating on dev builds. */
  alwaysOverwrite: boolean;
  /** Default mode for "Resume": Fast (size-only) or Safe (hash). */
  reconcileMode: ReconcileMode;
  /** When true, the Upload screen renders the per-file scroll panel
   *  during a transfer. Off hides the panel (keeps the progress bar +
   *  summary counters). Useful on low-end displays or when the list
   *  feels noisy for folders with thousands of files. */
  showTransferFiles: boolean;
  setAlwaysOverwrite: (on: boolean) => void;
  setReconcileMode: (m: ReconcileMode) => void;
  setShowTransferFiles: (on: boolean) => void;
}

export const useUploadSettingsStore = create<UploadSettingsState>((set) => ({
  alwaysOverwrite: loadAlwaysOverwrite(),
  reconcileMode: loadReconcileMode(),
  showTransferFiles: loadShowFiles(),
  setAlwaysOverwrite: (alwaysOverwrite) => {
    window.localStorage.setItem(
      KEY_ALWAYS_OVERWRITE,
      alwaysOverwrite ? "true" : "false",
    );
    set({ alwaysOverwrite });
  },
  setReconcileMode: (reconcileMode) => {
    window.localStorage.setItem(KEY_RECONCILE_MODE, reconcileMode);
    set({ reconcileMode });
  },
  setShowTransferFiles: (showTransferFiles) => {
    window.localStorage.setItem(
      KEY_SHOW_FILES,
      showTransferFiles ? "true" : "false",
    );
    set({ showTransferFiles });
  },
}));
