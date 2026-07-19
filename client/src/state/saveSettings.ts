import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Where "Save to USB storage" writes save backups on the PS5 itself —
 * e.g. a USB stick or extended-storage drive plugged into the console,
 * NOT the host PC (that's the existing Saves screen's Backup-to-file
 * flow). Defaults to the first USB mount slot the payload exposes.
 *
 * Persisted to localStorage and mirrored to ~/.ps5upload/settings.json,
 * same as engineUrl (state/engine.ts).
 */

export const DEFAULT_SAVE_PATH = "/mnt/usb0/savedata";
const KEY_SAVE_PATH = "ps5upload.save_path";

/** Trim and drop trailing slashes so callers can `${savePath}/<title_id>/...`
 *  without doubling up. Falls back to the default when empty. */
export function normalizeSavePath(raw: string): string {
  const v = raw.trim().replace(/\/+$/, "");
  return v || DEFAULT_SAVE_PATH;
}

function loadSavePath(): string {
  if (typeof window === "undefined") return DEFAULT_SAVE_PATH;
  const v = safeGetItem(KEY_SAVE_PATH);
  return v ? normalizeSavePath(v) : DEFAULT_SAVE_PATH;
}

interface SaveSettingsState {
  savePath: string;
  setSavePath: (path: string) => void;
}

export const useSaveSettingsStore = create<SaveSettingsState>((set) => ({
  savePath: loadSavePath(),
  setSavePath: (path) => {
    const savePath = normalizeSavePath(path);
    if (typeof window !== "undefined") {
      safeSetItem(KEY_SAVE_PATH, savePath);
    }
    set({ savePath });
  },
}));

/** Non-hook accessor for module-scope callers (Saves screen handlers). */
export function getSavePath(): string {
  return useSaveSettingsStore.getState().savePath;
}
