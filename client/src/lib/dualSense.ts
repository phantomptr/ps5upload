// A DualSense's own features in Remote Play: adaptive triggers and haptics (motion and the
// touchpad come with the pad's input reports, lib/padTouchpad.ts).
//
// When the desktop app finds a DualSense at the start of a stream (and Settings › DualSense
// effects is on), the session is started in DualSense mode: the console is told the pad is one
// and sends trigger effects and haptics instead of plain rumble (engine rp_stream). The page
// passes them on to the app, which writes the triggers to the pad over HID
// (commands/pad_dualsense.rs) and plays the haptics on its USB audio device
// (commands/pad_haptics.rs). Where the haptics cannot be played there (Bluetooth, or a Mac
// whose DualSense audio is still set to stereo), they are turned into rumble as chiaki-ng
// does, so the pad still vibrates. Browsers and the phone apps never start DualSense mode:
// nothing changes for them.

import { invoke as rawInvoke } from "@tauri-apps/api/core";

import { invoke } from "./invokeLogged";
import { isMobile } from "./platform";
import { isTauriEnv } from "./tauriEnv";

export interface DualSenseInfo {
  connected: boolean;
  usb: boolean;
  /** Its USB audio device is there, for the haptics. */
  haptics: boolean;
}

/** Only the desktop app reads a DualSense natively. */
export function dualSenseCapable(): boolean {
  return isTauriEnv() && !isMobile();
}

/** A DualSense on this computer, or null (none, or not the desktop app). */
export async function findDualSense(): Promise<DualSenseInfo | null> {
  if (!dualSenseCapable()) return null;
  try {
    const info = await invoke<DualSenseInfo>("pad_dualsense_info");
    return info?.connected ? info : null;
  } catch {
    return null;
  }
}

/** What the stream hands over in DualSense mode (lib/remotePlayStream.ts). */
export interface DualSenseSink {
  /** Each trigger's effect: [mode, 10 parameters]. */
  triggers(left: number[], right: number[]): void;
  /** The console's effect strengths: 0 off, 1 strong, 2 medium, 3 weak. */
  intensity(haptics: number, triggers: number): void;
  /** One 10 ms haptics frame: 3 kHz stereo s16le. */
  haptics(pcm: Uint8Array): void;
}

/** Haptics this quiet are not rumble (chiaki HAPTIC_RUMBLE_MIN_STRENGTH). */
const RUMBLE_MIN = 100;
/** Haptics frames averaged into one rumble level (chiaki RUMBLE_HAPTICS_PACKETS_PER_RUMBLE). */
const FRAMES_PER_RUMBLE = 3;

/** Haptics as rumble, for a pad that cannot play them (chiaki-ng `PushHapticsFrame` without a
 *  haptics device, at its Normal setting): each frame's mean loudness, the louder side, then
 *  averaged over three frames. `push` returns a rumble level (0-255) every third frame,
 *  otherwise null; a zero is returned once after rumbling, to stop it. */
export class HapticsRumble {
  private sum = 0;
  private n = 0;
  private on = false;

  push(pcm: Uint8Array): number | null {
    const v = new DataView(pcm.buffer, pcm.byteOffset, pcm.byteLength);
    const count = Math.floor(pcm.byteLength / 4);
    if (count) {
      let l = 0;
      let r = 0;
      for (let i = 0; i < count; i++) {
        l += Math.abs(v.getInt16(i * 4, true)) * 2;
        r += Math.abs(v.getInt16(i * 4 + 2, true)) * 2;
      }
      l = Math.floor(l / count);
      r = Math.floor(r / count);
      let strength = Math.max(l > RUMBLE_MIN ? l : 0, r > RUMBLE_MIN ? r : 0);
      // chiaki's floor for pads that drop the low bits.
      if (strength > 0 && strength < 512) strength = 512;
      this.sum += Math.min(strength, 0xffff);
    }
    this.n++;
    if (this.n < FRAMES_PER_RUMBLE) return null;
    const level = Math.floor(this.sum / FRAMES_PER_RUMBLE) >> 8;
    this.sum = 0;
    this.n = 0;
    if (level === 0 && !this.on) return null;
    this.on = level > 0;
    return level;
  }
}

/** The app side of one DualSense-mode stream. `rumble` plays a rumble level on the pads (the
 *  stream's own rumble path, which honours Settings › Controller vibration). */
export class DualSenseEffects implements DualSenseSink {
  private native: boolean;
  private fallback = new HapticsRumble();
  private hapticsOff = false;

  constructor(
    info: DualSenseInfo,
    private rumble: (strong: number, weak: number) => void,
  ) {
    this.native = info.usb && info.haptics;
    if (this.native) void invoke("pad_haptics_set", { on: true }).catch(() => (this.native = false));
  }

  triggers(left: number[], right: number[]): void {
    void invoke("pad_dualsense_triggers", { left, right }).catch(() => {});
  }

  intensity(haptics: number, triggers: number): void {
    this.hapticsOff = haptics === 0;
    void invoke("pad_dualsense_intensity", { haptics, triggers }).catch(() => {});
  }

  haptics(pcm: Uint8Array): void {
    if (this.native) {
      // Not logged: a hundred a second. False: no stream opened on the pad, so rumble instead.
      void rawInvoke<boolean>("pad_haptics_push", { pcm: Array.from(pcm) })
        .then((played) => {
          if (!played) this.native = false;
        })
        .catch(() => (this.native = false));
      return;
    }
    if (this.hapticsOff) return;
    const level = this.fallback.push(pcm);
    if (level !== null) this.rumble(level, level);
  }

  /** The stream ended: triggers off, the pad's audio closed. */
  stop(): void {
    void invoke("pad_dualsense_reset").catch(() => {});
    void invoke("pad_haptics_set", { on: false }).catch(() => {});
  }
}
