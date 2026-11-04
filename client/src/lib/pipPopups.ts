// The app's picture-in-picture windows (Tauri: client/src-tauri/src/pip_window.rs). Their page lives
// in the main window, so they close with it; a reload of the main page alone leaves them behind.

import { isTauriEnv } from "./tauriEnv";

export const TAURI_PIP_PREFIX = "rp-pip-";

export async function closeTauriPopups(): Promise<void> {
  try {
    const { getAllWebviewWindows } = await import("@tauri-apps/api/webviewWindow");
    for (const w of await getAllWebviewWindows()) {
      if (w.label.startsWith(TAURI_PIP_PREFIX)) await w.close();
    }
  } catch {
    // Already gone.
  }
}

/** At start-up: no window of this kind can have a player yet. */
export async function closeLeftoverPipPopups(): Promise<void> {
  if (isTauriEnv()) await closeTauriPopups();
}
