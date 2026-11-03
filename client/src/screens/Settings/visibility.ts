/** Which Settings controls make sense where the app is running.
 *
 *  The browser (Docker / self-hosted web UI) build talks to an engine on another
 *  machine and has no native shell: it cannot keep a computer awake, has no local
 *  settings file, no OS notifications, and its rest-mode reconnect loop never runs
 *  (AppShell returns early there). Pointing it at another engine URL would cut the
 *  page off from the engine that served it. The desktop and phone apps run their own
 *  engine on loopback and can use a self-hosted one (a NAS, Docker) instead. */
export interface SettingsPlatform {
  /** The desktop or Android shell (isTauriEnv). */
  tauri: boolean;
  /** Android (isMobile). */
  mobile: boolean;
}

export interface SettingsVisibility {
  engineUrl: boolean;
  keepDeviceAwake: boolean;
  settingsFile: boolean;
  osNotifications: boolean;
  reconnectAfterRest: boolean;
  /** In-app update check and download; the browser shows a note instead. */
  appUpdates: boolean;
}

export function settingsVisibility(p: SettingsPlatform): SettingsVisibility {
  return {
    engineUrl: p.tauri,
    // The browser build keeps the screen on (a page cannot hold the computer awake).
    keepDeviceAwake: true,
    settingsFile: p.tauri,
    osNotifications: p.tauri,
    reconnectAfterRest: p.tauri,
    appUpdates: p.tauri,
  };
}
