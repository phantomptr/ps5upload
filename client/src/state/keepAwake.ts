import { create } from "zustand";
import { invoke } from "../lib/invokeLogged";
import { isAndroid } from "../lib/platform";
import {
  screenWakeSupported,
  setScreenWakeReason,
} from "../lib/androidScreenWake";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

const STORAGE_KEY = "ps5upload.keep_awake";

// Manual-toggle reason name, mirroring keep_awake.rs's MANUAL_REASON.
const MANUAL_REASON = "manual";

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
  return safeGetItem(STORAGE_KEY) === "on";
}

export const useKeepAwakeStore = create<KeepAwakeState>((set, get) => ({
  enabled: loadPersisted(),
  supported: true, // optimistic; gets corrected by first syncFromBackend call
  lastError: null,

  async setEnabled(on) {
    // Android: the Rust inhibitor is a no-op here (keep_awake.rs has no
    // Android arm), so drive a screen wake lock instead — and report
    // `supported` from the WebView API, not the backend. Kept entirely
    // client-side so the manual toggle works the same as desktop.
    if (isAndroid()) {
      setScreenWakeReason(MANUAL_REASON, on);
      safeSetItem(STORAGE_KEY, on ? "on" : "off");
      set({ enabled: on, supported: screenWakeSupported(), lastError: null });
      return;
    }
    // Guard the invoke like syncFromBackend does: an IPC rejection
    // (running outside the Tauri runtime, command-resolution failure)
    // must not propagate. Two callers are fire-and-forget — the
    // Settings checkbox onChange and hydrateFromUserConfig — so an
    // unguarded throw here would surface as an unhandledrejection and,
    // in hydration, abort the remaining config restore. Route failures
    // to lastError and return normally.
    try {
      const resp = await invoke<KeepAwakeResp>("keep_awake_set", { enabled: on });
      safeSetItem(STORAGE_KEY, resp.enabled ? "on" : "off");
      set({
        enabled: resp.enabled,
        supported: resp.supported,
        lastError: resp.error ?? null,
      });
    } catch (e) {
      set({ lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  async syncFromBackend() {
    // Android: no backend state to read. Re-apply the persisted manual
    // preference (a screen wake lock doesn't survive an app restart) and
    // report support from the WebView.
    if (isAndroid()) {
      const want = get().enabled;
      setScreenWakeReason(MANUAL_REASON, want);
      set({ supported: screenWakeSupported(), lastError: null });
      return;
    }
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
