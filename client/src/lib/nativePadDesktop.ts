// The Linux desktop app's controllers, for a WebKitGTK built without libmanette: its Gamepad API
// never sees a controller, so the app reads them from the kernel (src-tauri
// commands/pad_native.rs) and sends each change as a `pad-native` event, which goes into the
// same bridge the Android app uses (lib/nativePad.ts). The bridge offers them only while the
// browser sees no controller itself, so a WebKitGTK that does see them is never read twice.

import { listen } from "@tauri-apps/api/event";

import { invoke } from "./invokeLogged";
import { isLinuxDesktop } from "./platform";

interface NativePadEvent {
  slot: number;
  id: string;
  connected: boolean;
  buttons: number[];
  axes: number[];
}

let started = false;

/** On Linux: listen for the app's controllers, then ask it to read them (it sends each one
 *  again, so a reloaded page has them at once). Elsewhere nothing: the Gamepad API is used. */
export async function startNativePads(): Promise<void> {
  if (started || !isLinuxDesktop()) return;
  started = true;
  try {
    await listen<NativePadEvent>("pad-native", (e) => window.__ps5uNativePad?.(e.payload));
    await invoke("pad_native_start");
  } catch {
    // Best effort: the keyboard and the on-screen controller play the same without it.
  }
}
