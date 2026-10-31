// The Linux app's WebKitGTK graphics switches (src-tauri/src/linux_gpu.rs). The shell decides at
// launch whether WebKit's GPU compositing and DMA-BUF renderer stay on, and tells the page through
// an init script; Safe graphics mode (Settings) turns both off from the next launch.

import { invoke } from "./invokeLogged";
import { isLinuxDesktop } from "./platform";
import { isTauriEnv } from "./tauriEnv";

/** What this launch uses (linux_gpu.rs LaunchGraphics). */
export interface LaunchGraphics {
  compositingOff: boolean;
  dmabufOff: boolean;
  /** Safe graphics mode was on at launch (the setting, --safe-graphics or the variable). */
  safeMode: boolean;
  /** The stack was judged to need no rescue. */
  currentStack: boolean;
  /** The app set a variable itself (otherwise they came from the user's environment). */
  setByApp: boolean;
}

export interface GraphicsStatus {
  linux: boolean;
  /** Safe graphics mode is saved on: applies from the next launch. */
  saved: boolean;
  launch: LaunchGraphics;
}

/** The Linux desktop app, where these switches exist. */
export function linuxGraphicsPossible(): boolean {
  return isTauriEnv() && isLinuxDesktop();
}

/** WebKit's compositing is off this launch: a WebGL canvas is then read back from the GPU every
 *  frame, so Remote Play draws with the 2D canvas. */
export function linuxCompositingOff(): boolean {
  if (typeof window === "undefined") return false;
  const flag = (
    window as unknown as {
      __PS5UPLOAD_LINUX_GRAPHICS__?: { compositingOff?: unknown };
    }
  ).__PS5UPLOAD_LINUX_GRAPHICS__;
  return flag?.compositingOff === true;
}

export function linuxGraphicsGet(): Promise<GraphicsStatus> {
  return invoke<GraphicsStatus>("linux_graphics_get");
}

export function linuxGraphicsSafeModeSet(on: boolean): Promise<GraphicsStatus> {
  return invoke<GraphicsStatus>("linux_graphics_safe_mode_set", { on });
}
