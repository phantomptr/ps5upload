// The Linux desktop app's controllers, for a WebKitGTK built without libmanette: its Gamepad API
// never sees a controller, so the app reads them from the kernel (src-tauri
// commands/pad_native.rs) and sends each change as a `pad-native` event, which goes into the
// same bridge the Android app uses (lib/nativePad.ts). The bridge offers them only while the
// browser sees no controller itself, except on Linux, where they are preferred: a WebKitGTK that
// sees a pad may misread it (#668, see lib/nativePad.ts).
//
// The Windows app does the same for a DualSense (#643): WebView2 saw none for some players
// while Windows listed the pad as working, so the app reads it over HID itself.

import { listen } from "@tauri-apps/api/event";

import { invoke } from "./invokeLogged";
import { log } from "../state/logs";
import { preferNativePads } from "./nativePad";
import { isLinuxDesktop, isWindows } from "./platform";

interface NativePadEvent {
  slot: number;
  id: string;
  connected: boolean;
  buttons: number[];
  axes: number[];
}

let started = false;

/** On Linux and Windows: listen for the app's controllers, then ask it to read them (it sends
 *  each one again, so a reloaded page has them at once). Elsewhere nothing: the Gamepad API is
 *  used. */
export async function startNativePads(): Promise<void> {
  if (started || !(isLinuxDesktop() || isWindows())) return;
  started = true;
  if (isLinuxDesktop()) preferNativePads();
  try {
    // Which reader a controller goes through, for a bug report.
    const seen = new Set<number>();
    await listen<NativePadEvent>("pad-native", (e) => {
      const { slot, id, connected } = e.payload;
      if (connected && !seen.has(slot)) {
        seen.add(slot);
        const own = (navigator.getGamepads?.() ?? []).filter((g) => !!g?.connected).map((g) => g?.id);
        log.info(
          "remote-play",
          `controller: the app reads "${id}"; the browser sees ${own.length ? own.map((o) => `"${o}"`).join(", ") : "none"}`,
        );
      } else if (!connected) {
        seen.delete(slot);
      }
      window.__ps5uNativePad?.(e.payload);
    });
    await invoke("pad_native_start");
  } catch {
    // Best effort: the keyboard and the on-screen controller play the same without it.
  }
}
