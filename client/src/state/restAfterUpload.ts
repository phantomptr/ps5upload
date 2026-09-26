import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * "Put the PS5 into rest mode after all uploads finish" — an opt-in
 * convenience so a long overnight upload queue can sleep the console when
 * it drains instead of leaving it powered on.
 *
 * Off by default (auto-sleeping a console is surprising), persisted to
 * localStorage like the other small UI preferences (saveSettings.ts,
 * keepAwake.ts). The actual standby call is driven from the upload queue's
 * drain-completion point (see uploadQueue.ts) using the existing
 * `powerStandby()` API — this store is just the preference + a module-scope
 * accessor the queue reads without subscribing.
 */

const STORAGE_KEY = "ps5upload.rest_after_upload";

function loadPersisted(): boolean {
  if (typeof window === "undefined") return false;
  return safeGetItem(STORAGE_KEY) === "on";
}

interface RestAfterUploadState {
  enabled: boolean;
  setEnabled: (on: boolean) => void;
}

export const useRestAfterUploadStore = create<RestAfterUploadState>((set) => ({
  enabled: loadPersisted(),
  setEnabled: (on) => {
    if (typeof window !== "undefined") {
      safeSetItem(STORAGE_KEY, on ? "on" : "off");
    }
    set({ enabled: on });
  },
}));

/** Non-hook accessor for the upload-queue drain hook, which runs outside a
 *  React render and just needs the current value. */
export function restAfterUploadEnabled(): boolean {
  return useRestAfterUploadStore.getState().enabled;
}
