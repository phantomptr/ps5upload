import { invoke } from "@tauri-apps/api/core";
import { isTauriEnv } from "../lib/tauriEnv";

import { useThemeStore, type Theme } from "./theme";
import { useLangStore } from "./lang";
import { useKeepAwakeStore } from "./keepAwake";
import { useUploadSettingsStore } from "./uploadSettings";
import { useConnectionStore } from "./connection";
import { useEngineStore, normalizeEngineUrl } from "./engine";

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
  theme?: Theme;
  lang?: string;
  /** PS5 IP address the Connection screen probes + sends payload to.
   *  Survives relaunch so users don't re-type it every session. */
  ps5_host?: string;
  /** Base URL of the engine the UI talks to. Default is the local
   *  sidecar; can point at a remote/self-hosted engine. */
  engine_url?: string;
  keep_awake?: boolean;
  upload?: {
    always_overwrite?: boolean;
    reconcile_mode?: "fast" | "safe";
    show_transfer_files?: boolean;
    bandwidth_cap_mbps?: number;
  };
}

function snapshotCurrent(): SettingsSnapshot {
  return {
    theme: useThemeStore.getState().theme,
    lang: useLangStore.getState().lang,
    ps5_host: useConnectionStore.getState().host,
    engine_url: useEngineStore.getState().engineUrl,
    keep_awake: useKeepAwakeStore.getState().enabled,
    upload: {
      always_overwrite: useUploadSettingsStore.getState().alwaysOverwrite,
      reconcile_mode: useUploadSettingsStore.getState().reconcileMode,
      show_transfer_files: useUploadSettingsStore.getState().showTransferFiles,
      bandwidth_cap_mbps: useUploadSettingsStore.getState().bandwidthCapMbps,
    },
  };
}

let pendingTimer: ReturnType<typeof setTimeout> | null = null;

/** Request a file-mirror write. Coalesces rapid changes into one write. */
function schedulePersist() {
  if (!isTauriEnv()) return; // browser dev/test contexts: no Tauri invoke
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
  // Engine URL: persist to disk AND push to the Rust shell so its command
  // proxies follow the change live (the spawn-mode switch still needs a
  // restart). Fires on any engine-store change.
  useEngineStore.subscribe((s) => {
    schedulePersist();
    pushEngineUrl(s.engineUrl);
  });
}

/** Tell the Rust shell which engine URL its proxies should hit. */
function pushEngineUrl(url: string): void {
  if (!isTauriEnv()) return;
  invoke("engine_url_set", { url }).catch((e) => {
    console.warn("[settings-mirror] engine_url_set failed:", e);
  });
}

/** Force an immediate (non-debounced) write of the current snapshot.
 *  Used after first-launch hydration so the file always exists reflecting
 *  current state, not just after the user flips a toggle. */
async function persistNow(): Promise<void> {
  if (!isTauriEnv()) return;
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

  // Skip-when-matches: only apply disk values that actually differ
  // from the live store state. Avoids spurious setter calls (which
  // re-fire mirror writes and cascade through subscribers).
  //
  // The previous shape included an additional `store === seed` guard
  // intended as race protection — but `seed` was snapshotted from the
  // store one statement above, so the equality always held in practice.
  // Drop the no-op guard and rely on the value-inequality check.
  const liveTheme = useThemeStore.getState().theme;
  if (
    (data.theme === "dark" || data.theme === "light" || data.theme === "oled") &&
    data.theme !== liveTheme
  ) {
    useThemeStore.getState().setTheme(data.theme);
  }
  const liveLang = useLangStore.getState().lang;
  if (typeof data.lang === "string" && data.lang !== liveLang) {
    // setLang narrows to the valid LanguageCode set; unknown codes
    // fall back to "en" inside the store — no throw.
    useLangStore.getState().setLang(data.lang as never);
  }
  const liveHost = useConnectionStore.getState().host;
  if (
    typeof data.ps5_host === "string" &&
    data.ps5_host.trim() &&
    data.ps5_host !== liveHost
  ) {
    useConnectionStore.getState().setHost(data.ps5_host);
  }
  const liveEngineUrl = useEngineStore.getState().engineUrl;
  if (
    typeof data.engine_url === "string" &&
    data.engine_url.trim() &&
    normalizeEngineUrl(data.engine_url) !== liveEngineUrl
  ) {
    useEngineStore.getState().setEngineUrl(data.engine_url);
  }
  // Push the resolved engine URL to the Rust shell regardless of whether
  // it changed here — the shell read settings.json at startup, but a
  // localStorage-only value (no file) wouldn't have reached it.
  pushEngineUrl(useEngineStore.getState().engineUrl);
  const liveKeepAwake = useKeepAwakeStore.getState().enabled;
  if (typeof data.keep_awake === "boolean" && data.keep_awake !== liveKeepAwake) {
    await useKeepAwakeStore.getState().setEnabled(data.keep_awake);
  }
  if (data.upload) {
    const u = data.upload;
    const us = useUploadSettingsStore.getState();
    if (
      typeof u.always_overwrite === "boolean" &&
      u.always_overwrite !== us.alwaysOverwrite
    ) {
      us.setAlwaysOverwrite(u.always_overwrite);
    }
    // reconcile_mode is intentionally not hydrated: "safe" was retired
    // (it ran BLAKE3 on every remote file, which destabilized the
    // payload on large libraries). The store always reports "fast";
    // hand-edited "safe" in settings.json gets ignored.
    if (
      typeof u.show_transfer_files === "boolean" &&
      u.show_transfer_files !== us.showTransferFiles
    ) {
      us.setShowTransferFiles(u.show_transfer_files);
    }
    if (
      typeof u.bandwidth_cap_mbps === "number" &&
      Number.isFinite(u.bandwidth_cap_mbps) &&
      u.bandwidth_cap_mbps >= 0 &&
      u.bandwidth_cap_mbps !== us.bandwidthCapMbps
    ) {
      us.setBandwidthCapMbps(u.bandwidth_cap_mbps);
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

/**
 * Factory-reset: wipe ALL local ps5upload data + metadata and the renderer's
 * own storage. The caller should reload the window afterwards so every store
 * re-initialises from defaults.
 *
 * Order matters: cancel any queued settings-mirror write FIRST, so the
 * debounced `user_config_save` can't recreate `settings.json` right after the
 * backend deletes it. Then wipe the filesystem (backend) + localStorage
 * (renderer). Returns the count of filesystem entries removed.
 */
export async function resetAllAppData(): Promise<number> {
  if (pendingTimer) {
    clearTimeout(pendingTimer);
    pendingTimer = null;
  }
  let removed = 0;
  if (isTauriEnv()) {
    removed = await invoke<number>("app_data_reset");
  }
  // localStorage.clear() drops every ps5upload.* key the renderer owns in one
  // shot (the app owns its origin's storage). Best-effort.
  try {
    window.localStorage.clear();
  } catch {
    // private-mode / sandbox: nothing more we can do
  }
  return removed;
}
