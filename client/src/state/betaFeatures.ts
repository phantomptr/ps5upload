import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * "Beta features" — one switch that reveals features that are still being
 * finished. Off by default, because the things it reveals are, by definition,
 * not ready to be stumbled into.
 *
 * Today it gates the FPKG builder ("Convert to FPKG"), which can produce and
 * install a package but does not yet make the console mount it. Gating it here
 * rather than deleting it keeps the work reachable for testing without putting
 * a broken screen in everyone's sidebar.
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
