import { invoke } from "@tauri-apps/api/core";

import { useThemeStore } from "./theme";
import { useLangStore } from "./lang";
import { useKeepAwakeStore } from "./keepAwake";
import { useUploadSettingsStore } from "./uploadSettings";
import { useConnectionStore } from "./connection";

/**
 * Mirror all persisted user settings to `~/.ps5upload/settings.json`.
 *
 * Architecture: localStorage stays the primary store (synchronous, used
 * during first paint for theme/lang). This module is a *mirror* — it
 * observes all the settings stores and writes a JSON snapshot to disk
 * via the Tauri `user_config_save` command. On startup, it reads the
 * file once and, if its values differ from localStorage, hydrates the
 * stores from disk (authoritative) so the user's hand-edits take
 * effect.
 *
 * Writes are debounced (300 ms) so a burst of setting changes collapses
 * to a single atomic file write.
 */

const DEBOUNCE_MS = 300;

interface SettingsSnapshot {
  // Top-level keys mirror the renderer's internal store names. Flat by
  // design — easy to hand-edit, easy to extend.
  theme?: "dark" | "light";
  lang?: string;
  /** PS5 IP address the Connection screen probes + sends payload to.
   *  Survives relaunch so users don't re-type it every session. */
  ps5_host?: string;
  keep_awake?: boolean;
  upload?: {
    always_overwrite?: boolean;
    reconcile_mode?: "fast" | "safe";
    show_transfer_files?: boolean;
  };
}

function snapshotCurrent(): SettingsSnapshot {
  return {
    theme: useThemeStore.getState().theme,
    lang: useLangStore.getState().lang,
    ps5_host: useConnectionStore.getState().host,
    keep_awake: useKeepAwakeStore.getState().enabled,
    upload: {
      always_overwrite: useUploadSettingsStore.getState().alwaysOverwrite,
      reconcile_mode: useUploadSettingsStore.getState().reconcileMode,
      show_transfer_files: useUploadSettingsStore.getState().showTransferFiles,
    },
  };
}

let pendingTimer: ReturnType<typeof setTimeout> | null = null;

/** Request a file-mirror write. Coalesces rapid changes into one write. */
function schedulePersist() {
  if (pendingTimer) clearTimeout(pendingTimer);
  pendingTimer = setTimeout(async () => {
    pendingTimer = null;
    try {
      await invoke("user_config_save", { config: snapshotCurrent() });
    } catch (e) {
      // Silent: file-mirror is best-effort. localStorage is still the
      // source of truth, so a failed disk write doesn't lose anything.
      // (Tauri dev mode without the command registered throws here —
      // that's noise, not a problem.)
      console.warn("[settings-mirror] persist failed:", e);
    }
  }, DEBOUNCE_MS);
}

/** Subscribe to every settings store and schedule a persist on any
 *  change. Called once at app startup.
 *
 *  Idempotent: Vite HMR re-evaluates modules without unloading the old
 *  module's side-effects, so without this guard each HMR cycle would
 *  stack a fresh subscription on top of the previous one and
 *  `schedulePersist` would fire N times per change where N is the HMR
 *  count. The guard doesn't capture the unsubscribe functions — we
 *  never need to tear the mirror down during the app's lifetime. */
let mirrorInstalled = false;
export function installUserConfigMirror() {
  if (mirrorInstalled) return;
  mirrorInstalled = true;
  useThemeStore.subscribe(schedulePersist);
  useLangStore.subscribe(schedulePersist);
  useKeepAwakeStore.subscribe(schedulePersist);
  useUploadSettingsStore.subscribe(schedulePersist);
  // `host` is the only field in useConnectionStore that persists across
  // launches — status/step fields are transient. But Zustand.subscribe
  // fires on any slice change, so we coarsely re-snapshot on every
  // connection-store update; the debounce coalesces those into a
  // single write.
  useConnectionStore.subscribe(schedulePersist);
}

/** Force an immediate (non-debounced) write of the current snapshot.
 *  Used after first-launch hydration so the file always exists reflecting
 *  current state, not just after the user flips a toggle. */
async function persistNow(): Promise<void> {
  if (pendingTimer) {
    clearTimeout(pendingTimer);
    pendingTimer = null;
  }
  try {
    await invoke("user_config_save", { config: snapshotCurrent() });
  } catch (e) {
    console.warn("[settings-mirror] initial persist failed:", e);
  }
}

/** Hydrate stores from `~/.ps5upload/settings.json` if it exists. Called
 *  once, async, after React has mounted. If the file differs from the
 *  localStorage values we loaded synchronously, disk wins — matches the
 *  expectation that editing the file by hand and relaunching should
 *  take effect. */
export async function hydrateFromUserConfig(): Promise<void> {
  let data: SettingsSnapshot | null;
  try {
    data = await invoke<SettingsSnapshot | null>("user_config_load");
  } catch {
    return; // command not available, or file unreadable — stay on localStorage
  }
  // No file yet → write one now so the user can see where it lives and
  // what the schema looks like. Everything else in this function only
  // runs when a pre-existing file needs to override localStorage.
  if (!data) {
    await persistNow();
    return;
  }

  // Race guard: the user may have typed something during the file read.
  // Snapshot the initial (seeded-from-localStorage) store values NOW —
  // only apply the file value if the store hasn't been touched in the
  // meantime. Without this, a user edit between `installUserConfigMirror`
  // and the hydration callback could get reverted by the disk value.
  const seed = {
    theme: useThemeStore.getState().theme,
    lang: useLangStore.getState().lang,
    ps5_host: useConnectionStore.getState().host,
    keep_awake: useKeepAwakeStore.getState().enabled,
    always_overwrite: useUploadSettingsStore.getState().alwaysOverwrite,
    reconcile_mode: useUploadSettingsStore.getState().reconcileMode,
    show_transfer_files: useUploadSettingsStore.getState().showTransferFiles,
  };

  if (data.theme === "dark" || data.theme === "light") {
    if (useThemeStore.getState().theme === seed.theme && data.theme !== seed.theme) {
      useThemeStore.getState().setTheme(data.theme);
    }
  }
  if (typeof data.lang === "string") {
    if (useLangStore.getState().lang === seed.lang && data.lang !== seed.lang) {
      // setLang narrows to the valid LanguageCode set; unknown codes
      // fall back to "en" inside the store — no throw.
      useLangStore.getState().setLang(data.lang as never);
    }
  }
  if (typeof data.ps5_host === "string" && data.ps5_host.trim()) {
    if (
      useConnectionStore.getState().host === seed.ps5_host &&
      data.ps5_host !== seed.ps5_host
    ) {
      useConnectionStore.getState().setHost(data.ps5_host);
    }
  }
  if (typeof data.keep_awake === "boolean") {
    if (
      useKeepAwakeStore.getState().enabled === seed.keep_awake &&
      data.keep_awake !== seed.keep_awake
    ) {
      await useKeepAwakeStore.getState().setEnabled(data.keep_awake);
    }
  }
  if (data.upload) {
    const u = data.upload;
    const us = useUploadSettingsStore.getState();
    if (
      typeof u.always_overwrite === "boolean" &&
      us.alwaysOverwrite === seed.always_overwrite &&
      u.always_overwrite !== seed.always_overwrite
    ) {
      us.setAlwaysOverwrite(u.always_overwrite);
    }
    if (
      (u.reconcile_mode === "fast" || u.reconcile_mode === "safe") &&
      us.reconcileMode === seed.reconcile_mode &&
      u.reconcile_mode !== seed.reconcile_mode
    ) {
      us.setReconcileMode(u.reconcile_mode);
    }
    if (
      typeof u.show_transfer_files === "boolean" &&
      us.showTransferFiles === seed.show_transfer_files &&
      u.show_transfer_files !== seed.show_transfer_files
    ) {
      us.setShowTransferFiles(u.show_transfer_files);
    }
  }
  // Rewrite once after hydration. If we applied anything the setters
  // already scheduled a debounced persist; this ensures the file
  // converges to the authoritative (merged) snapshot without waiting
  // for the debounce window.
  await persistNow();
}

/** Resolve the on-disk path so the Settings screen can show users
 *  where their mirror lives. Safe to call without the Tauri shell. */
export async function userConfigPath(): Promise<string | null> {
  try {
    return await invoke<string>("user_config_path_resolved");
  } catch {
    return null;
  }
}
