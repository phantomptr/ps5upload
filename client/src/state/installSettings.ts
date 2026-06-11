import { create } from "zustand";

/**
 * User preferences governing the Install Package flow.
 *
 * Both settings shape the hands-off "add a .pkg → it ends up installed → the
 * staged copy is cleaned up" flow. They are GLOBAL (one choice for all
 * consoles): they're workflow preferences, not topology-dependent. The actual
 * install MECHANISM is a fixed cascade (stage → main-payload InstallByPackage →
 * DPI-daemon fallback, in `pkgLibrary.runPkgInstall`), identical per console —
 * there is no user-selectable install "method" (an earlier stream/stage/dpi
 * setting was removed as dead code; nothing ever read it).
 *
 * Persisted to localStorage so the choices survive restarts. Namespaced keys —
 * never silently migrate; bump a key if its schema ever changes.
 */

const KEY_AUTO_REMOVE = "ps5upload.auto_remove_after_install";
const KEY_AUTO_INSTALL = "ps5upload.auto_install_after_upload";

function loadAutoRemove(): boolean {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem(KEY_AUTO_REMOVE) === "1";
}

// Defaults ON: the whole point of staging a .pkg in the library is to
// install it, so once the upload lands we kick the install off without a
// second manual click. Persisted, so a user who prefers to inspect the
// staged file first can turn it off and that sticks. Absent key → ON
// (opt-out), the behaviour the maintainer asked for.
function loadAutoInstall(): boolean {
  if (typeof window === "undefined") return true;
  return window.localStorage.getItem(KEY_AUTO_INSTALL) !== "0";
}

interface InstallSettingsState {
  /** When true, a staged .pkg in the library is deleted from the PS5
   *  automatically right after it installs successfully — so the
   *  library list doesn't accumulate spent packages. Off by default
   *  (a user may want to re-install or keep the .pkg around). */
  autoRemoveAfterInstall: boolean;
  setAutoRemoveAfterInstall: (v: boolean) => void;
  /** When true, a .pkg uploaded into the staging library installs
   *  automatically as soon as the upload finishes — no separate manual
   *  "Install" click. On by default (staging exists to be installed).
   *  Pairs with autoRemoveAfterInstall to make "upload → installed →
   *  staged copy cleaned up" one hands-off flow. */
  autoInstallAfterUpload: boolean;
  setAutoInstallAfterUpload: (v: boolean) => void;
}

export const useInstallSettingsStore = create<InstallSettingsState>((set) => ({
  autoRemoveAfterInstall: loadAutoRemove(),
  setAutoRemoveAfterInstall: (autoRemoveAfterInstall) => {
    if (typeof window !== "undefined") {
      window.localStorage.setItem(
        KEY_AUTO_REMOVE,
        autoRemoveAfterInstall ? "1" : "0",
      );
    }
    set({ autoRemoveAfterInstall });
  },
  autoInstallAfterUpload: loadAutoInstall(),
  setAutoInstallAfterUpload: (autoInstallAfterUpload) => {
    if (typeof window !== "undefined") {
      window.localStorage.setItem(
        KEY_AUTO_INSTALL,
        autoInstallAfterUpload ? "1" : "0",
      );
    }
    set({ autoInstallAfterUpload });
  },
}));
