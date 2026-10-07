import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * "Install by file path when the PS5 cannot reach this computer" — a trial switch, off by
 * default.
 *
 * When it is on, installs ask the engine to allow its last resort: a package that is on the
 * console, refused from the console's own storage and unreachable from this computer, is
 * offered to the PS5 by its plain file path. The engine still decides whether it may run (a
 * base game that is not installed, nothing else), so turning this on can never put an
 * installed game at risk. It is a switch at all because the route is unproven: it worked for
 * one reporter on FW 11.20 and is refused on FW 13.60.
 */

const STORAGE_KEY = "ps5upload.install_path_fallback";

function loadPersisted(): boolean {
  if (typeof window === "undefined") return false;
  return safeGetItem(STORAGE_KEY) === "on";
}

interface InstallPathFallbackState {
  enabled: boolean;
  setEnabled: (on: boolean) => void;
}

export const useInstallPathFallbackStore = create<InstallPathFallbackState>((set) => ({
  enabled: loadPersisted(),
  setEnabled: (on) => {
    if (typeof window !== "undefined") {
      safeSetItem(STORAGE_KEY, on ? "on" : "off");
    }
    set({ enabled: on });
  },
}));

/** The install options the switch adds: nothing while it is off. */
export function installPathFallbackOptions(): { console_path_fallback?: true } {
  return useInstallPathFallbackStore.getState().enabled ? { console_path_fallback: true } : {};
}
