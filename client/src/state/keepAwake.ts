import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

const STORAGE_KEY = "ps5upload.keep_awake";

interface KeepAwakeResp {
  enabled: boolean;
  supported: boolean;
  error?: string;
}

interface KeepAwakeState {
  enabled: boolean;
  supported: boolean;
  lastError: string | null;
  setEnabled: (on: boolean) => Promise<void>;
  syncFromBackend: () => Promise<void>;
}

/** Persist the user's preferred state so we can restore it on next launch.
 *  The OS-level inhibitor itself does NOT survive app restarts (the
 *  caffeinate child dies with the parent) — this is just the UI
 *  checkbox preference. */
function loadPersisted(): boolean {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem(STORAGE_KEY) === "on";
}

export const useKeepAwakeStore = create<KeepAwakeState>((set, get) => ({
  enabled: loadPersisted(),
  supported: true, // optimistic; gets corrected by first syncFromBackend call
  lastError: null,

  async setEnabled(on) {
    const resp = await invoke<KeepAwakeResp>("keep_awake_set", { enabled: on });
    window.localStorage.setItem(
      STORAGE_KEY,
      resp.enabled ? "on" : "off"
    );
    set({
      enabled: resp.enabled,
      supported: resp.supported,
      lastError: resp.error ?? null,
    });
  },

  async syncFromBackend() {
    try {
      const resp = await invoke<KeepAwakeResp>("keep_awake_state");
      set({
        enabled: resp.enabled,
        supported: resp.supported,
        lastError: null,
      });
      // If the user had it toggled on persistently and the backend shows
      // it off (fresh app launch), re-apply.
      if (resp.supported && !resp.enabled && get().enabled) {
        await get().setEnabled(true);
      }
    } catch (e) {
      set({ lastError: e instanceof Error ? e.message : String(e) });
    }
  },
}));
