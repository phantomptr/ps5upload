// Play's picture from a capture device on this computer (#893): Remote Play keeps carrying the
// controller, keyboard and mouse (and the console's session), while the picture, and if chosen
// the sound, come from an HDMI capture device plugged into the computer, at its own quality.
//
// Desktop app only (Windows, macOS, Linux): the device is read with getUserMedia, which the
// phone apps and the browser build are not offered (a browser build plays on another device
// than the one the capture device would be in). The choice is kept per console, on this device.
//
// This file holds what can be worked out without a device (the choice, which device and which
// of its sound inputs, the session's light settings); state/capturePicture.ts runs it.

import { isMobile } from "./platform";
import { safeGetItem, safeSetItem } from "./safeStorage";
import { isTauriEnv } from "./tauriEnv";

/** Where Play's picture (and sound) come from, for one console. */
export interface CaptureChoice {
  /** "rp": the Remote Play picture (default); "device": the capture device. */
  picture: "rp" | "device";
  /** The capture device: its id (as the browser gave it) and name, which finds it again when
   *  the id changed (some systems give new ids each run). */
  deviceId: string;
  label: string;
  /** "rp": the Remote Play sound; "device": the capture device's own sound. */
  sound: "rp" | "device";
}

export const DEFAULT_CAPTURE: CaptureChoice = { picture: "rp", deviceId: "", label: "", sound: "rp" };

const KEY = "ps5upload.rp.capture.v1";

/** Can this app show a capture device's picture? The desktop app, with getUserMedia. */
export function captureAvailable(): boolean {
  if (!isTauriEnv() || isMobile()) return false;
  const md = typeof navigator !== "undefined" ? navigator.mediaDevices : undefined;
  return typeof md?.getUserMedia === "function" && typeof md?.enumerateDevices === "function";
}

function readAll(): Record<string, CaptureChoice> {
  try {
    const raw = safeGetItem(KEY);
    const v = raw ? (JSON.parse(raw) as unknown) : null;
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, CaptureChoice>) : {};
  } catch {
    return {};
  }
}

/** A stored choice made safe (a hand-edited or older entry gets the defaults). */
export function cleanChoice(c: Partial<CaptureChoice> | null | undefined): CaptureChoice {
  return {
    picture: c?.picture === "device" ? "device" : "rp",
    deviceId: typeof c?.deviceId === "string" ? c.deviceId : "",
    label: typeof c?.label === "string" ? c.label : "",
    sound: c?.sound === "device" ? "device" : "rp",
  };
}

/** The choice for the console at `host`. */
export function captureChoiceFor(host: string | null | undefined): CaptureChoice {
  if (!host) return DEFAULT_CAPTURE;
  return cleanChoice(readAll()[host]);
}

export function saveCaptureChoice(host: string, c: CaptureChoice): void {
  const all = readAll();
  if (c.picture === "rp" && !c.deviceId && c.sound === "rp") delete all[host];
  else all[host] = cleanChoice(c);
  safeSetItem(KEY, JSON.stringify(all));
}

/** The capture device's picture is wanted for this console (and can be shown here). */
export function captureWanted(host: string | null | undefined): boolean {
  if (!captureAvailable()) return false;
  const c = captureChoiceFor(host);
  return c.picture === "device" && !!(c.deviceId || c.label);
}

/** What Remote Play is asked for while the picture comes from a capture device: the lightest
 *  stream among the settings Play offers, which the console is known to take. The session stays
 *  up for the controls (and the picture to fall back to); the capture device shows the game. */
export const CAPTURE_LIGHT = { res: "1280x720", fps: 30, bitrateKbps: 3000 } as const;

export interface DeviceInfo {
  deviceId: string;
  label: string;
  groupId: string;
  kind: string;
}

/** The chosen device among the video inputs: by id, else by name (ids can change between
 *  runs), else none. */
export function findDevice(devices: readonly DeviceInfo[], c: Pick<CaptureChoice, "deviceId" | "label">): DeviceInfo | null {
  const video = devices.filter((d) => d.kind === "videoinput");
  return (
    (c.deviceId && video.find((d) => d.deviceId === c.deviceId)) ||
    (c.label && video.find((d) => d.label === c.label)) ||
    null
  );
}

/** The capture device's own sound input: the audio input of the same physical device (the same
 *  group), else one whose name matches the picture's (systems name both after the device). */
export function soundInputFor(devices: readonly DeviceInfo[], video: DeviceInfo): DeviceInfo | null {
  const audio = devices.filter((d) => d.kind === "audioinput" && d.deviceId !== "default" && d.deviceId !== "communications");
  if (video.groupId) {
    const same = audio.find((d) => d.groupId === video.groupId);
    if (same) return same;
  }
  const name = baseName(video.label);
  if (!name) return null;
  return (
    audio.find((d) => {
      const n = baseName(d.label);
      return !!n && (n.includes(name) || name.includes(n));
    }) ?? null
  );
}

/** A device's name without what systems add around it: "(Video)", "Audio", "(1234:abcd)",
 *  "Digital Audio Interface" and the like, so a picture and its sound compare equal. */
export function baseName(label: string): string {
  return label
    .replace(/\([0-9a-f]{4}:[0-9a-f]{4}\)/gi, "")
    .replace(/\b(video|audio|digital|interface|capture|input|line|in|stereo|microphone)\b/gi, "")
    .replace(/[()[\]\-:]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase();
}

/** The getUserMedia constraints for the chosen device: its best picture (up to 4K at 60 fps,
 *  the device picks the nearest it has) and, when asked, its sound as it is (no voice
 *  processing, which would duck the game). */
export function captureConstraints(video: DeviceInfo, sound: DeviceInfo | null): MediaStreamConstraints {
  return {
    video: {
      deviceId: { exact: video.deviceId },
      width: { ideal: 1920, max: 3840 },
      height: { ideal: 1080, max: 2160 },
      frameRate: { ideal: 60 },
    },
    audio: sound
      ? {
          deviceId: { exact: sound.deviceId },
          echoCancellation: false,
          noiseSuppression: false,
          autoGainControl: false,
          channelCount: { ideal: 2 },
        }
      : false,
  };
}

/** Why getUserMedia failed, as the notice says it. */
export type CaptureFailure = "denied" | "missing" | "busy" | "other";

export function captureFailure(e: unknown): CaptureFailure {
  const name = e instanceof Error || (e && typeof e === "object" && "name" in e) ? String((e as { name?: unknown }).name) : "";
  if (name === "NotAllowedError" || name === "SecurityError" || name === "PermissionDeniedError") return "denied";
  if (name === "NotFoundError" || name === "OverconstrainedError" || name === "DevicesNotFoundError") return "missing";
  if (name === "NotReadableError" || name === "TrackStartError" || name === "AbortError") return "busy";
  return "other";
}
