import { create } from "zustand";

/**
 * User preferences governing the Install Package flow's defaults.
 *
 * The only setting today is the install method — Tier-1 staging
 * (upload-then-install) vs the DPI 2.0 streaming path (BGFT pulls
 * directly from a desktop-hosted HTTP route). Streaming is faster,
 * needs no 2× disk space, and gets native pause/resume for free
 * (BGFT's own behaviour), so it's the new default. The staging path
 * remains available for users on LAN topologies where the PS5 can't
 * reach the desktop's HTTP port (segregated VLANs, host firewalls
 * that block inbound to the engine port, etc).
 *
 * The setting is the DEFAULT applied when a new install is queued;
 * the queue item itself records the method that was used so a row
 * already in flight isn't retroactively switched if the user toggles
 * the default mid-install. Same pattern as uploadSettings.
 *
 * Persisted to localStorage so the choice survives restarts. Single
 * namespaced key — never silently migrate; bump the key if the
 * schema ever changes (cost: one re-pick from the user).
 */

/** "stream" = DPI 2.0; "stage" = legacy upload-then-install (Tier 1). */
export type InstallMethod = "stream" | "stage";

const KEY_INSTALL_METHOD = "ps5upload.install_method";

function loadInstallMethod(): InstallMethod {
  if (typeof window === "undefined") return "stream";
  const v = window.localStorage.getItem(KEY_INSTALL_METHOD);
  return v === "stage" ? "stage" : "stream";
}

interface InstallSettingsState {
  /** Default install method for newly-queued items. Per-item override
   *  lives on the queue row, so an in-flight install isn't disturbed
   *  by changing this. */
  installMethod: InstallMethod;
  setInstallMethod: (m: InstallMethod) => void;
}

export const useInstallSettingsStore = create<InstallSettingsState>((set) => ({
  installMethod: loadInstallMethod(),
  setInstallMethod: (installMethod) => {
    window.localStorage.setItem(KEY_INSTALL_METHOD, installMethod);
    set({ installMethod });
  },
}));
