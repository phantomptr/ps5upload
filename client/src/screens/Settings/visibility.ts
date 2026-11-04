/** Which Settings controls make sense where the app is running.
 *
 *  The browser (Docker / self-hosted web UI) build talks to an engine on another
 *  machine and has no native shell: it cannot keep a computer awake, has no local
 *  settings file, no OS notifications, and its rest-mode reconnect loop never runs
 *  (AppShell returns early there). Pointing it at another engine URL would cut the
 *  page off from the engine that served it. The phone apps link their engine in-process: their
 *  native commands always reach it (engine_mobile.rs) and Android allows plain http to loopback
 *  only, so a self-hosted engine there needs more than this field. They say so instead
 *  (`engineBuiltIn`). */
export interface SettingsPlatform {
  /** The desktop or phone app (isTauriEnv). */
  tauri: boolean;
  /** Android or iPhone (isMobile). */
  mobile: boolean;
}

export interface SettingsVisibility {
  engineUrl: boolean;
  /** A line saying the phone app uses its built-in engine. */
  engineBuiltIn: boolean;
  keepDeviceAwake: boolean;
  settingsFile: boolean;
  osNotifications: boolean;
  reconnectAfterRest: boolean;
  /** In-app update check and download; the browser shows a note instead. */
  appUpdates: boolean;
}

export function settingsVisibility(p: SettingsPlatform): SettingsVisibility {
  return {
    engineUrl: p.tauri && !p.mobile,
    engineBuiltIn: p.tauri && p.mobile,
    // The browser build keeps the screen on (a page cannot hold the computer awake).
    keepDeviceAwake: true,
    settingsFile: p.tauri,
    osNotifications: p.tauri,
    reconnectAfterRest: p.tauri,
    appUpdates: p.tauri,
  };
}
