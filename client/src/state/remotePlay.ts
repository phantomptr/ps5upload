import { create } from "zustand";

import { getEngineUrl } from "./engine";
import {
  DEFAULT_KEYMAP,
  IDLE_PAD,
  RemotePlayStream,
  loadKeymap,
  mergePads,
  padFromGamepads,
  padFromKeys,
  saveKeymap,
  type KeyAction,
  type PadState,
  type PictureFit,
  type StreamError,
  type StreamPhase,
  type StreamStats,
} from "../lib/remotePlayStream";
import type { MotionReading } from "../lib/motion";
import { gamepadsOf } from "../lib/nativePad";
import { profileFor } from "../lib/padProfiles";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { setGameMode } from "../lib/gameMode";

/** How Remote Play looks and behaves on this device. */
export interface RpPrefs {
  res: "1920x1080" | "1280x720";
  fps: 60 | 30;
  /** How the picture fills its place: whole with black bars, cropped, or stretched. */
  fit: PictureFit;
  /** fps, bitrate and delay over the picture. */
  showStats: boolean;
  volume: number;
  muted: boolean;
  /** Sound kept ready, ms: lower is less delay, higher rides out a busy network. */
  audioBufferMs: number;
  /** How long a session waits for the app to come back after it left, s. */
  keepAliveS: number;
  /** Clicking the picture takes the keyboard and hides the pointer. */
  capturePointer: boolean;
  rumble: boolean;
  /** The controller drawn over the picture: on touch screens (auto), always, or never. */
  touchControls: "auto" | "on" | "off";
  /** How solid the on-screen controller is drawn, 0.3 (faint) to 1. */
  touchOpacity: number;
  /** The on-screen controller's size. */
  touchSize: "s" | "m" | "l";
  /** A phone's motion sensors as the controller's (gyro aiming, tilt). */
  motion: boolean;
  /** Least delay: the smallest sound buffer and short picture queues (more dropped frames on a
   *  busy network). */
  lowLatency: boolean;
  /** Offer 720p when a 1080p stream keeps losing frames. */
  autoQuality: boolean;
  /** Microphone for voice chat (experimental): sessions are set up for it and the mic button
   *  shows. Off, a session starts exactly as it did before the mic existed. */
  mic: boolean;
  /** Instant replay: the engine keeps the last this-many seconds (15, 30 or 60) to save with
   *  Save replay; 0 = off. */
  replayS: number;
}

export const DEFAULT_RP_PREFS: RpPrefs = {
  res: "1920x1080",
  fps: 60,
  fit: "fit",
  showStats: false,
  volume: 1,
  muted: false,
  audioBufferMs: 60,
  keepAliveS: 60,
  capturePointer: true,
  rumble: true,
  touchControls: "auto",
  touchOpacity: 1,
  touchSize: "m",
  motion: false,
  lowLatency: false,
  autoQuality: true,
  mic: false,
  replayS: 0,
};

/** The smallest sound buffer the Sound buffer setting offers. */
export const LOWEST_AUDIO_BUFFER_MS = 40;

/** The sound buffer in use: the smallest one in low-latency mode, else the one chosen. */
export function audioBufferFor(p: Pick<RpPrefs, "lowLatency" | "audioBufferMs">): number {
  return p.lowLatency ? Math.min(LOWEST_AUDIO_BUFFER_MS, p.audioBufferMs) : p.audioBufferMs;
}

const PREFS_KEY = "ps5upload.rp_prefs.v1";

export function loadRpPrefs(): RpPrefs {
  try {
    const raw = safeGetItem(PREFS_KEY);
    return { ...DEFAULT_RP_PREFS, ...(raw ? (JSON.parse(raw) as Partial<RpPrefs>) : {}) };
  } catch {
    return DEFAULT_RP_PREFS;
  }
}

/** The console, with the Wake & sign in keys a stream starts with. */
export interface RpTarget {
  host: string;
  name: string;
  registKey: string;
  rpKey: string;
}

interface RpState {
  stream: RemotePlayStream | null;
  target: RpTarget | null;
  phase: StreamPhase | null;
  error: StreamError | null;
  stats: StreamStats | null;
  light: [number, number, number] | null;
  prefs: RpPrefs;
  keymap: Record<string, KeyAction>;
  /** The Remote Play screen is on show: only then do controllers drive the console. */
  onScreen: boolean;
  /** The picture is in picture in picture, and how: a window of its own (desktop app popup or
   *  the browser's Document Picture-in-Picture), or the browser's video picture in picture. */
  pip: PipKind | null;
  /** The picture-in-picture window, when the picture is in one (not for "video"). */
  pipWindow: Window | null;
  start: (t: RpTarget, opts?: { attach?: boolean }) => Promise<void>;
  stop: () => void;
  setPrefs: (p: Partial<RpPrefs>) => void;
  setKeymap: (k: Record<string, KeyAction> | null) => void;
  setOnScreen: (v: boolean) => void;
  setPip: (kind: PipKind | null, win?: Window | null) => void;
}

/** popup: the desktop app's always-on-top window; document: the browser's Document
 *  Picture-in-Picture; video: the browser's video picture in picture (picture only). */
export type PipKind = "popup" | "document" | "video";

/** Keys held on the picture and a finger on the touchpad: read by the stream every frame. */
export const rpInput = {
  held: new Set<string>(),
  touch: null as [number, number] | null,
  /** The on-screen controller. */
  touchPad: IDLE_PAD as PadState,
  /** A controller is being mapped: its presses are answers, not play. */
  mapping: false,
  /** The phone's latest motion reading, while motion controls are on. */
  motion: null as MotionReading | null,
};

/** Controllers as every window showing the stream sees them. A hidden page's list can be a
 *  stale snapshot (buttons stuck down), so only visible ones count. Read once each: the same
 *  pad seen by two windows counts once (padFromGamepads takes the strongest). */
export function rpGamepads(): Array<Gamepad | null> {
  const out: Array<Gamepad | null> = [];
  const add = (w: Window | null | undefined) => {
    try {
      if (!w || w.closed || w.document.visibilityState !== "visible") return;
      out.push(...gamepadsOf(w.navigator));
    } catch {
      // A window being torn down.
    }
  };
  if (typeof window !== "undefined") add(window);
  add(useRemotePlayStore.getState().pipWindow);
  return out;
}

function readPad(): PadState {
  const s = useRemotePlayStore.getState();
  const keys = padFromKeys(rpInput.held, s.keymap);
  // A controller drives the console while you are looking at the stream, not while you use
  // another screen or another app. A picture-in-picture window is the stream on show whatever
  // has the focus: there the controller plays it while you use another app, where the system
  // gives a background window controller input at all (Chromium does; WebKit gives it only to
  // the window in front).
  const pip = !!s.pipWindow && !s.pipWindow.closed;
  const pads =
    !rpInput.mapping && (pip || (s.onScreen && typeof document !== "undefined" && document.hasFocus()))
      ? padFromGamepads(rpGamepads(), profileFor)
      : IDLE_PAD;
  return {
    ...mergePads(mergePads(keys, pads), rpInput.touchPad),
    touch: rpInput.touch ?? rpInput.touchPad.touch ?? null,
    motion: rpInput.motion,
  };
}

export const useRemotePlayStore = create<RpState>((set, get) => ({
  stream: null,
  target: null,
  phase: null,
  error: null,
  stats: null,
  light: null,
  prefs: loadRpPrefs(),
  keymap: loadKeymap(),
  onScreen: false,
  pip: null,
  pipWindow: null,

  async start(t, opts) {
    const cur = get().stream;
    if (cur && cur.currentPhase !== "ended") return;
    const { prefs } = get();
    const [width, height] = prefs.res.split("x").map(Number);
    const stream: RemotePlayStream = new RemotePlayStream({
      host: t.host,
      registKey: t.registKey,
      rpKey: t.rpKey,
      width,
      height,
      fps: prefs.fps,
      keepAliveS: prefs.keepAliveS,
      audioBufferMs: audioBufferFor(prefs),
      lowLatency: prefs.lowLatency,
      attach: opts?.attach,
      fit: prefs.fit,
      mic: prefs.mic,
      replayS: prefs.replayS,
      rumble: () => get().prefs.rumble,
      onPhase: (p) => {
        if (get().stream !== stream) return;
        set({ phase: p });
        if (p === "ended") {
          rpInput.held.clear();
          set({ stream: null, light: null });
        }
      },
      onStats: (st) => get().stream === stream && set({ stats: st }),
      onError: (e) => get().stream === stream && set({ error: e }),
      onLight: (rgb) => get().stream === stream && set({ light: rgb }),
      readPad,
      gamepads: rpGamepads,
    });
    stream.setVolume(prefs.muted ? 0 : prefs.volume);
    set({ stream, target: t, phase: "connecting", error: null, stats: null, light: null });
    await stream.start();
  },

  stop() {
    get().stream?.stop();
  },

  setPrefs(p) {
    const prefs = { ...get().prefs, ...p };
    safeSetItem(PREFS_KEY, JSON.stringify(prefs));
    set({ prefs });
    const s = get().stream;
    if (s) {
      s.setVolume(prefs.muted ? 0 : prefs.volume);
      s.setAudioBuffer(audioBufferFor(prefs));
      s.setFit(prefs.fit);
      s.setReplay(prefs.replayS);
    }
  },

  setKeymap(k) {
    saveKeymap(k);
    set({ keymap: k ?? DEFAULT_KEYMAP });
  },

  setOnScreen(v) {
    set({ onScreen: v });
  },

  setPip(kind, win = null) {
    set({ pip: kind, pipWindow: kind ? win : null });
    get().stream?.setFrameWindow(kind ? win : null);
  },
}));

// The OS's game mode (lib/gameMode.ts) while a stream plays, on any screen. Off at start-up:
// nothing plays yet, and a hold a reloaded page left on is let go.
setGameMode(false);
useRemotePlayStore.subscribe((s) => setGameMode(!!s.stream && s.phase === "playing"));

/** The session the engine is running, if any (it outlives a reload of this page). */
export async function fetchRunningSession(): Promise<{ host: string | null; playing?: boolean; attached?: boolean }> {
  try {
    const r = await fetch(`${getEngineUrl()}/api/ps5/rp/session`);
    return r.ok ? await r.json() : { host: null };
  } catch {
    return { host: null };
  }
}
