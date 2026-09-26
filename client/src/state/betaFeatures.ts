import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * "Beta features" — one switch that reveals features that are still being
 * finished. Off by default, because the things it reveals are, by definition,
 * not ready to be stumbled into.
 *
 * Nothing is gated today: Convert to FPKG, the first feature it held back,
 * graduated once its packages installed and played. The switch stays for the
 * next feature that needs a quiet trial.
 *
 * Persisted like the other UI preferences and mirrored to settings.json so it
 * survives a storage reset or a reinstall — a user who turned it on to test
 * should not silently lose the feature they were testing.
 */

const STORAGE_KEY = "ps5upload.beta_features";

function loadPersisted(): boolean {
  if (typeof window === "undefined") return false;
  return safeGetItem(STORAGE_KEY) === "on";
}

interface BetaFeaturesState {
  enabled: boolean;
  setEnabled: (on: boolean) => void;
}

export const useBetaFeaturesStore = create<BetaFeaturesState>((set) => ({
  enabled: loadPersisted(),
  setEnabled: (on) => {
    if (typeof window !== "undefined") {
      safeSetItem(STORAGE_KEY, on ? "on" : "off");
    }
    set({ enabled: on });
  },
}));

/** Non-hook accessor for nav/route code that runs outside a React render and
 *  only needs the current value. React surfaces should use the hook so they
 *  re-render when the switch is flipped. */
export function betaFeaturesEnabled(): boolean {
  return useBetaFeaturesStore.getState().enabled;
}
