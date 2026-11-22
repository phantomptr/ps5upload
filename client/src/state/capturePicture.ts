// Runs Play's picture from a capture device (#893, lib/capturePicture.ts says what and why):
// while a stream of a console set to use one plays here, the device is opened, its picture is
// shown in place of the Remote Play picture (screens/Play/CaptureVideo.tsx) and, if chosen, its
// sound plays in place of the Remote Play sound. The device going away (unplugged, taken by
// another app) or not opening puts the Remote Play picture and sound back, with a notice.

import { create } from "zustand";

import {
  captureAvailable,
  captureChoiceFor,
  captureConstraints,
  captureFailure,
  findDevice,
  saveCaptureChoice,
  soundInputFor,
  type CaptureChoice,
  type CaptureFailure,
  type DeviceInfo,
} from "../lib/capturePicture";
import { trStatic } from "../lib/trStatic";
import { log } from "./logs";
import { pushNotification } from "./notifications";
import { setCaptureSound, useRemotePlayStore } from "./remotePlay";

interface CaptureState {
  /** The capture device's picture (and sound) while it is shown, else null. */
  media: MediaStream | null;
  /** The console it is shown for. */
  host: string | null;
  /** Its picture's shape (width / height), once known. */
  shape: number | null;
  /** Bumped when a console's choice changes, so what shows it reads it again. */
  rev: number;
  setChoice(host: string, c: CaptureChoice): void;
}

export const useCaptureStore = create<CaptureState>((set, get) => ({
  media: null,
  host: null,
  shape: null,
  rev: 0,
  setChoice(host, c) {
    saveCaptureChoice(host, c);
    set({ rev: get().rev + 1 });
    sync();
  },
}));

/** The video inputs (and audio inputs) on this computer, with names. Names are only given once
 *  the app may use a camera, so a list without them asks first (the system's prompt). */
export async function listCaptureDevices(): Promise<DeviceInfo[] | CaptureFailure> {
  const md = navigator.mediaDevices;
  try {
    let all = await md.enumerateDevices();
    if (!all.some((d) => d.kind === "videoinput" && d.label)) {
      // Listed while the probe is open: some webviews name devices only while one is in use.
      const probe = await md.getUserMedia({ video: true });
      try {
        all = await md.enumerateDevices();
      } finally {
        for (const t of probe.getTracks()) t.stop();
      }
    }
    return all.map((d) => ({ deviceId: d.deviceId, label: d.label, groupId: d.groupId, kind: d.kind }));
  } catch (e) {
    log.warn("remote-play", `capture devices: ${e instanceof Error ? `${e.name}: ${e.message}` : String(e)}`);
    return captureFailure(e);
  }
}

/** The element showing the capture picture now (for a screenshot), set by CaptureVideo. */
let shown: HTMLVideoElement | null = null;
export function setShownCaptureVideo(v: HTMLVideoElement | null): void {
  shown = v;
}

/** A screenshot of the capture picture, at its own size; null when it is not shown. */
export function captureSnapshot(): Promise<Blob | null> | null {
  const v = shown;
  if (!useCaptureStore.getState().media || !v || !v.videoWidth || !v.videoHeight) return null;
  const c = document.createElement("canvas");
  c.width = v.videoWidth;
  c.height = v.videoHeight;
  const g = c.getContext("2d");
  if (!g) return null;
  try {
    g.drawImage(v, 0, 0);
  } catch {
    return null;
  }
  return new Promise((resolve) => c.toBlob((b) => resolve(b), "image/png"));
}

/** For a recording: the capture picture, with the sound that plays (the device's, or Remote
 *  Play's from `rpSound`). Null when the capture picture is not shown. */
export function captureRecording(
  rpSound: () => { stream: MediaStream; release: () => void } | null,
): { stream: MediaStream; release: () => void } | null {
  const media = useCaptureStore.getState().media;
  const video = media?.getVideoTracks()[0];
  if (!media || !video || video.readyState !== "live") return null;
  // Copies of the device's tracks, stopped with the recording (the picture on show goes on).
  const copies: MediaStreamTrack[] = [video.clone()];
  const own = media.getAudioTracks()[0];
  if (own) copies.push(own.clone());
  // Remote Play's sound otherwise, tapped off by the stream (its picture is not used).
  const rp = own ? null : rpSound();
  const stream = new MediaStream([...copies, ...(rp?.stream.getAudioTracks() ?? [])]);
  return {
    stream,
    release: () => {
      for (const t of copies) t.stop();
      rp?.release();
    },
  };
}

// ── Running it ─────────────────────────────────────────────────────────────────────────────

/** What is shown now, as "<session>|<host>|<choice>", or "" for nothing: work is done only when
 *  it changes (the stream's store changes many times a second). */
let current = "";
let seq = 0;
let sound: HTMLAudioElement | null = null;
const sessions = new WeakMap<object, number>();
let nextSession = 1;

function sessionId(stream: object): number {
  let id = sessions.get(stream);
  if (!id) {
    id = nextSession++;
    sessions.set(stream, id);
  }
  return id;
}

function wantedKey(): string {
  const s = useRemotePlayStore.getState();
  const host = s.target?.host;
  if (!s.stream || s.phase !== "playing" || !s.pictureHere || !host || !captureAvailable()) return "";
  const c = captureChoiceFor(host);
  if (c.picture !== "device" || !(c.deviceId || c.label)) return "";
  return `${sessionId(s.stream)}|${host}|${c.deviceId}|${c.label}|${c.sound}`;
}

function release(): void {
  const { media } = useCaptureStore.getState();
  for (const t of media?.getTracks() ?? []) {
    t.onended = null;
    t.stop();
  }
  if (sound) {
    sound.pause();
    sound.srcObject = null;
    sound = null;
  }
  setCaptureSound(false);
  useCaptureStore.setState({ media: null, host: null, shape: null });
}

function notice(kind: CaptureFailure | "lost" | "no_sound"): void {
  const text: Record<typeof kind, string> = {
    denied: trStatic(
      "rp_capdev_denied",
      "The app may not use the capture device. Allow camera access for PS5Upload in the system settings, then start again. The Remote Play picture is shown.",
    ),
    missing: trStatic(
      "rp_capdev_missing",
      "The capture device was not found. Plug it in and choose it again in Settings › Picture. The Remote Play picture is shown.",
    ),
    busy: trStatic(
      "rp_capdev_busy",
      "The capture device is in use by another app. Close that app, then start again. The Remote Play picture is shown.",
    ),
    other: trStatic("rp_capdev_failed", "The capture device could not start. The Remote Play picture is shown."),
    lost: trStatic("rp_capdev_lost", "The capture device stopped, so the Remote Play picture and sound are back."),
    no_sound: trStatic(
      "rp_capdev_no_sound",
      "The capture device has no sound input here, so the Remote Play sound plays.",
    ),
  };
  pushNotification(kind === "no_sound" ? "info" : "warning", text[kind]);
}

async function open(key: string, host: string, c: CaptureChoice): Promise<void> {
  const mine = ++seq;
  const list = await listCaptureDevices();
  if (mine !== seq) return;
  const video = typeof list === "string" ? null : findDevice(list, c);
  if (typeof list === "string" || !video) {
    log.warn("remote-play", `capture picture: ${typeof list === "string" ? list : "device not found"}`);
    notice(typeof list === "string" ? list : "missing");
    return;
  }
  const soundIn = c.sound === "device" ? soundInputFor(list as DeviceInfo[], video) : null;
  let media: MediaStream;
  try {
    media = await navigator.mediaDevices.getUserMedia(captureConstraints(video, soundIn));
  } catch (e) {
    if (mine !== seq) return;
    log.warn("remote-play", `capture picture: ${e instanceof Error ? `${e.name}: ${e.message}` : String(e)}`);
    notice(captureFailure(e));
    return;
  }
  if (mine !== seq || current !== key) {
    for (const t of media.getTracks()) t.stop();
    return;
  }
  const track = media.getVideoTracks()[0];
  const st = track?.getSettings?.() ?? {};
  log.info(
    "remote-play",
    `capture picture: "${video.label}" ${st.width ?? "?"}x${st.height ?? "?"} at ${st.frameRate ?? "?"} fps, sound ${
      media.getAudioTracks().length ? "from the device" : "from Remote Play"
    }`,
  );
  // Unplugged, or taken by another app: the Remote Play picture comes back.
  for (const t of media.getTracks()) {
    t.onended = () => {
      if (useCaptureStore.getState().media !== media) return;
      log.warn("remote-play", "capture picture: the device stopped");
      release();
      notice("lost");
    };
  }
  useCaptureStore.setState({ media, host, shape: st.width && st.height ? st.width / st.height : null });
  if (media.getAudioTracks().length) {
    const a = new Audio();
    a.autoplay = true;
    a.srcObject = new MediaStream(media.getAudioTracks());
    applyVolume(a);
    sound = a;
    setCaptureSound(true);
    void a.play().catch(() => {});
  } else if (c.sound === "device") {
    notice("no_sound");
  }
}

function applyVolume(a: HTMLAudioElement | null): void {
  if (!a) return;
  const p = useRemotePlayStore.getState().prefs;
  a.volume = Math.max(0, Math.min(1, p.volume));
  a.muted = p.muted;
}

/** Open or close the device as the stream and the choice say. */
export function sync(): void {
  const key = wantedKey();
  if (key === current) return;
  current = key;
  seq++;
  if (useCaptureStore.getState().media) release();
  if (!key) return;
  const host = useRemotePlayStore.getState().target?.host ?? "";
  void open(key, host, captureChoiceFor(host));
}

useRemotePlayStore.subscribe((s, prev) => {
  if (s.prefs.volume !== prev.prefs.volume || s.prefs.muted !== prev.prefs.muted) applyVolume(sound);
  sync();
});

/** Test hook. */
export function resetCaptureForTest(): void {
  current = "";
  seq++;
  shown = null;
  if (useCaptureStore.getState().media || sound) release();
}
