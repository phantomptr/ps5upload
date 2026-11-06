import { create } from "zustand";

import { askToSignIn } from "../lib/signInNeeded";
import { getEngineUrl } from "./engine";
import {
  DEFAULT_KEYMAP,
  FIT_ORDER,
  IDLE_PAD,
  PadButton,
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
  type PasscodeAsk,
} from "../lib/remotePlayStream";
import type { MotionReading } from "../lib/motion";
import { gamepadsOf } from "../lib/nativePad";
import { profileFor } from "../lib/padProfiles";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { setGameMode } from "../lib/gameMode";
import { setScreenWakeReason } from "../lib/androidScreenWake";
import { setPadForwardReason } from "../lib/androidPadBridge";
import { padMotion, padTouchState, setPadTouchpad } from "../lib/padTouchpad";
import { DualSenseEffects, dualSenseCapable, findDualSense } from "../lib/dualSense";
import { applyControls } from "../lib/gameControls";
import { DEFAULT_PICTURE, readPicture, type PictureTools } from "../lib/rpPicture";
import { DEFAULT_BUTTON_MAP, readButtonMap, remapPad, type ButtonMap } from "../lib/rpButtonMap";
import type { CodecPref } from "../lib/rpHdr";
import { nextKeyboard, type RpKeyboardState } from "../lib/rpKeyboard";
import { pushNotification } from "./notifications";
import { trStatic } from "../lib/trStatic";
import { currentControls, rpControls } from "./gameControls";
import { useKbmStore } from "./kbm";
import { emitPadKbm, readKbmStatus } from "../lib/kbmInput";

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
  /** How solid the on-screen controller is drawn, 0.25 (faint) to 1. */
  touchOpacity: number;
  /** The on-screen controller's size. */
  touchSize: "s" | "m" | "l";
  /** A phone's motion sensors as the controller's (gyro aiming, tilt). */
  motion: boolean;
  /** A DualSense on this computer gets the console's adaptive triggers and haptics (sessions
   *  start in DualSense mode, lib/dualSense.ts). Off, sessions start as before. */
  dualSense: boolean;
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
  /** Picture tools: sharpen, colour, 21:9 crop, sharp pixels (lib/rpPicture.ts). */
  picture: PictureTools;
  /** On a phone, screenshots and clips go to the photo library (lib/rpCapture.ts saveCapture). */
  toPhotos: boolean;
  /** Decode on the graphics card when there is one (auto), or always in software: a way out
   *  of a graphics driver's decoder bug (Windows' hardware HEVC, #538/#575). */
  decoding: "auto" | "software";
  /** The video format asked of the console: HEVC where this device decodes it (auto), always
   *  H.264, or HDR (HEVC Main10) where this device can show it in HDR (lib/rpHdr.ts). */
  codec: CodecPref;
  /** The video bitrate asked of the console, kbit/s; 0 = Auto (15 Mbps at 1080p, 10 at 720p).
   *  Go live sends the console's video on as it is, so this is what a streaming site gets. */
  bitrateKbps: number;
  /** What each controller button sends (lib/rpButtonMap.ts); empty = each sends itself. */
  buttonMap: ButtonMap;
  /** While the PS5 has the keyboard and mouse, a small "⌃ ⌥ to release" over the picture. */
  kbmChip: boolean;
  /** Holding Esc gives the keyboard and mouse back too (besides the release shortcut). */
  escHoldRelease: boolean;
  /** On a touch screen: the on-screen controller, or the screen as the PS5 mouse's touchpad. */
  touchMode: "controller" | "touchpad";
  /** How far the PS5's pointer moves for a finger's movement on the touchpad. */
  touchpadSpeed: number;
  /** On other screens of the app, the picture in a small floating window (layout/RemotePlayMini). */
  miniPlayer: boolean;
  /** Android app: leaving the app while playing keeps the picture floating over other apps
   *  (Android's picture in picture, lib/androidPip.ts). */
  systemPip: boolean;
  /** What a new session starts with: the picture, sound and controller on this device, or none
   *  of them (Go live and Play Together keep getting the stream; #670, #649). */
  pictureHere: boolean;
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
  dualSense: true,
  lowLatency: false,
  autoQuality: true,
  mic: false,
  replayS: 0,
  picture: DEFAULT_PICTURE,
  toPhotos: true,
  decoding: "auto",
  codec: "auto",
  bitrateKbps: 0,
  buttonMap: DEFAULT_BUTTON_MAP,
  kbmChip: true,
  escHoldRelease: true,
  touchMode: "controller",
  touchpadSpeed: 1.5,
  miniPlayer: true,
  systemPip: true,
  pictureHere: true,
};

/** The smallest sound buffer the Sound buffer setting offers. */
export const LOWEST_AUDIO_BUFFER_MS = 40;

/** The sound buffer in use: the smallest one in low-latency mode, else the one chosen. */
export function audioBufferFor(p: Pick<RpPrefs, "lowLatency" | "audioBufferMs">): number {
  return p.lowLatency ? Math.min(LOWEST_AUDIO_BUFFER_MS, p.audioBufferMs) : p.audioBufferMs;
}

const PREFS_KEY = "ps5upload.rp_prefs.v1";

/** The bitrates Settings › Picture offers (kbit/s; 0 = Auto). */
export const RP_BITRATES: readonly number[] = [0, 15000, 12000, 10000, 8000, 6000, 4500, 3000];

export function loadRpPrefs(): RpPrefs {
  try {
    const raw = safeGetItem(PREFS_KEY);
    const p = { ...DEFAULT_RP_PREFS, ...(raw ? (JSON.parse(raw) as Partial<RpPrefs>) : {}) };
    const codec: CodecPref = p.codec === "h264" || p.codec === "hdr" ? p.codec : "auto";
    const touchMode = p.touchMode === "touchpad" ? "touchpad" : "controller";
    const touchpadSpeed = Number.isFinite(p.touchpadSpeed) ? Math.min(4, Math.max(0.5, p.touchpadSpeed)) : 1.5;
    const bitrateKbps = RP_BITRATES.includes(p.bitrateKbps) ? p.bitrateKbps : 0;
    // A value this version has no use for (hand-edited, a restored backup) falls back: `res` is
    // split into numbers for the session, so a bad one would fail every stream.
    const one = <T,>(v: T, allowed: readonly T[], fallback: T): T => (allowed.includes(v) ? v : fallback);
    return {
      ...p,
      res: one(p.res, ["1920x1080", "1280x720"], DEFAULT_RP_PREFS.res),
      fps: one(p.fps, [60, 30], DEFAULT_RP_PREFS.fps),
      fit: one(p.fit, FIT_ORDER, DEFAULT_RP_PREFS.fit),
      touchSize: one(p.touchSize, ["s", "m", "l"], DEFAULT_RP_PREFS.touchSize),
      decoding: one(p.decoding, ["auto", "software"], DEFAULT_RP_PREFS.decoding),
      codec,
      bitrateKbps,
      touchMode,
      touchpadSpeed,
      picture: readPicture(p.picture),
      buttonMap: readButtonMap(p.buttonMap),
    };
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

/** The fixed-for-a-session settings a restart may change for that session alone. */
export type SessionQuality = Partial<Pick<RpPrefs, "res" | "codec" | "bitrateKbps" | "pictureHere">>;

interface RpState {
  stream: RemotePlayStream | null;
  target: RpTarget | null;
  phase: StreamPhase | null;
  error: StreamError | null;
  stats: StreamStats | null;
  light: [number, number, number] | null;
  /** The console's on-screen keyboard is open (lib/rpKeyboard.ts); the stream shows a text box. */
  keyboard: RpKeyboardState | null;
  /** The text box is closed here (done or cancelled) before the console says so. */
  closeKeyboard: () => void;
  /** The console waits for the user's sign-in passcode (`seq` changes with each ask). Only
   *  whether it asks lives here: the digits are sent straight on and never kept. */
  passcode: (PasscodeAsk & { seq: number }) | null;
  answerPasscode: (digits: string) => void;
  cancelPasscode: () => void;
  prefs: RpPrefs;
  keymap: Record<string, KeyAction>;
  /** The Remote Play screen is on show: only then do controllers drive the console. */
  onScreen: boolean;
  /** The picture is in picture in picture, and how: a window of its own (desktop app popup or
   *  the browser's Document Picture-in-Picture), or the browser's video picture in picture. */
  pip: PipKind | null;
  /** The picture-in-picture window, when the picture is in one (not for "video"). */
  pipWindow: Window | null;
  /** What this session runs at instead of the saved prefs ("Switch to 720p", Go live's restart).
   *  Ends with the session: the saved settings are what the next one starts with. */
  sessionQuality: SessionQuality | null;
  /** This session's picture on this device: off, nothing is decoded or played here. */
  pictureHere: boolean;
  /** Turn it off or back on for this session (Settings keep what the next one starts with). */
  setPictureHere: (on: boolean) => void;
  start: (t: RpTarget, opts?: { attach?: boolean; quality?: SessionQuality | null }) => Promise<void>;
  stop: () => void;
  /** Stop to start again at once (Play Together stays open). */
  stopForRestart: () => void;
  setPrefs: (p: Partial<RpPrefs>) => void;
  setKeymap: (k: Record<string, KeyAction> | null) => void;
  setOnScreen: (v: boolean) => void;
  setPip: (kind: PipKind | null, win?: Window | null) => void;
}

/** popup: the desktop app's always-on-top window; document: the browser's Document
 *  Picture-in-Picture; video: the browser's video picture in picture (picture only). */
export type PipKind = "popup" | "document" | "video";

/** Who holds the game's input back now (Settings, the passcode box, the keyboard box). */
const inputPauses = new Set<string>();

/** While any `owner` has it on, the console gets an idle pad. Each lets go only of its own: the
 *  keyboard box closing must not hand the pad back under open Settings. */
export function pauseRpInput(owner: "settings" | "passcode" | "keyboard", on: boolean): void {
  if (on) inputPauses.add(owner);
  else inputPauses.delete(owner);
}

/** Keys held on the picture and a finger on the touchpad: read by the stream every frame. */
export const rpInput = {
  held: new Set<string>(),
  touch: null as [number, number] | null,
  /** The on-screen controller. */
  touchPad: IDLE_PAD as PadState,
  /** A controller is being mapped: its presses are answers, not play. */
  mapping: false,
  /** Remote Play's settings or a box are open over the stream: keys, controllers and touch are
   *  theirs, and the console gets an idle pad until they close ([`pauseRpInput`]). */
  get paused(): boolean {
    return inputPauses.size > 0;
  },
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

/** A controller is connected, and neither it nor anything the browser sees has motors to run. */
export function padsCannotVibrate(
  pads: ReadonlyArray<Gamepad | null> = rpGamepads(),
  browserPads: ReadonlyArray<Gamepad | null> = globalThis.navigator?.getGamepads?.() ?? [],
): boolean {
  const motors = (g: Gamepad | null) => !!(g as (Gamepad & { vibrationActuator?: unknown }) | null)?.vibrationActuator;
  return pads.some((g) => !!g?.connected) && !pads.some(motors) && !browserPads.some(motors);
}

function readPad(): PadState {
  if (rpInput.paused) return IDLE_PAD;
  const s = useRemotePlayStore.getState();
  if (!s.pictureHere) return IDLE_PAD;
  const controls = currentControls();
  // A macro's key plays the macro, not whatever button the key map gives it.
  const macroKeys = controls?.macros.length ? new Set(controls.macros.map((m) => m.key)) : null;
  const held = macroKeys ? new Set([...rpInput.held].filter((k) => !macroKeys.has(k))) : rpInput.held;
  const keys = padFromKeys(held, s.keymap);
  // A controller drives the console while you are looking at the stream, not while you use
  // another screen or another app. A picture-in-picture window is the stream on show whatever
  // has the focus: there the controller plays it while you use another app, where the system
  // gives a background window controller input at all (Chromium does; WebKit gives it only to
  // the window in front).
  const pip = !!s.pipWindow && !s.pipWindow.closed;
  const live = !rpInput.mapping && (pip || (s.onScreen && typeof document !== "undefined" && document.hasFocus()));
  const read = live ? padFromGamepads(rpGamepads(), profileFor) : IDLE_PAD;
  // A DualSense's touchpad, which the Gamepad API never gives (lib/padTouchpad.ts): the same
  // rule as its buttons, and the click is the touchpad button whatever the browser reports.
  const pt = live ? padTouchState() : null;
  // Controller buttons sent as Settings › Controller buttons says (the keyboard has its own map).
  const pads = remapPad(pt?.click ? { ...read, buttons: read.buttons | PadButton.TOUCHPAD } : read, s.prefs.buttonMap);
  const merged = {
    ...mergePads(mergePads(keys, pads), rpInput.touchPad),
    touch: rpInput.touch ?? pt?.touch ?? rpInput.touchPad.touch ?? null,
    // A phone's motion, else a DualSense's (read with its touchpad, same rule).
    motion: rpInput.motion ?? (live ? padMotion() : null),
  };
  // The running game's turbo, mouse aim and macros (Game panel › Controls).
  const now = typeof performance !== "undefined" ? performance.now() : Date.now();
  return applyControls(merged, controls, rpInput.held, rpControls, now);
}

export const useRemotePlayStore = create<RpState>((set, get) => ({
  stream: null,
  target: null,
  phase: null,
  error: null,
  stats: null,
  light: null,
  keyboard: null,
  closeKeyboard: () => set({ keyboard: null }),
  passcode: null,
  answerPasscode: (digits) => {
    const st = get();
    if (st.stream?.sendPasscode(digits)) set({ passcode: null });
  },
  cancelPasscode: () => {
    get().stream?.cancelPasscode();
    set({ passcode: null });
  },
  prefs: loadRpPrefs(),
  keymap: loadKeymap(),
  onScreen: false,
  pip: null,
  pipWindow: null,
  sessionQuality: null,
  pictureHere: true,
  setPictureHere(on) {
    const stream = get().stream;
    if (!on) {
      rpInput.held.clear();
      rpInput.touch = null;
      rpInput.touchPad = IDLE_PAD;
    }
    if (stream) stream.setPictureHere(on);
    else set({ pictureHere: on });
  },

  async start(t, opts) {
    const cur = get().stream;
    if (cur && cur.currentPhase !== "ended") return;
    const { prefs } = get();
    const quality = opts?.quality ?? null;
    const [width, height] = (quality?.res ?? prefs.res).split("x").map(Number);
    // A DualSense here: the session starts in DualSense mode (triggers and haptics). Only the
    // desktop app looks (and only it waits for the answer).
    const ds = prefs.dualSense && dualSenseCapable() ? await findDualSense() : null;
    // Started meanwhile (a second tap while the controller was looked for).
    const now = get().stream;
    if (now && now.currentPhase !== "ended") return;
    const dualSense = ds
      ? new DualSenseEffects(ds, (strong, weak) => get().stream?.vibrate(strong, weak))
      : undefined;
    // A page joining a running session takes what the session has (the engine says).
    const pictureHere = opts?.attach ? undefined : (quality?.pictureHere ?? prefs.pictureHere);
    const stream: RemotePlayStream = new RemotePlayStream({
      pictureHere,
      onPictureHere: (on) => {
        if (get().stream === stream) set({ pictureHere: on });
      },
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
      dualSense,
      replayS: prefs.replayS,
      picture: prefs.picture,
      decoding: prefs.decoding,
      codec: quality?.codec ?? prefs.codec,
      bitrateKbps: (quality?.bitrateKbps ?? prefs.bitrateKbps) || undefined,
      onPictureOff: (why) =>
        pushNotification(
          "warning",
          why === "slow"
            ? trStatic("rp_picture_off_slow", "Picture tools turned off: this device could not keep up.")
            : why === "linux"
              ? trStatic(
                  "rp_picture_off_linux",
                  "Sharpen and upscaling are off: with the Linux app's safe graphics, drawing them makes the stream lag. Chrome on Linux has them.",
                )
              : trStatic("rp_picture_off_failed", "Picture tools turned off: this device has no WebGL 2."),
        ),
      onHdrOff: (_why, started) => {
        // HDR did not work here: back to Auto so the next start is plain HEVC (the reason is in
        // the logs).
        if (get().prefs.codec === "hdr") get().setPrefs({ codec: "auto" });
        pushNotification(
          "warning",
          started
            ? trStatic(
                "rp_hdr_off_started",
                "HDR could not be shown on this device, so the picture is shown in SDR. Video format is back to Auto for the next start.",
              )
            : trStatic(
                "rp_hdr_off",
                "HDR does not work on this device, so this stream is SDR. Video format is back to Auto.",
              ),
        );
      },
      onHevcFailed: () => {
        get().setPrefs({ codec: "h264" });
        pushNotification(
          "warning",
          trStatic(
            "rp_codec_now_h264",
            "This device showed no picture in the video format it tried, so Video format is now H.264. Press Start to play again.",
          ),
        );
      },
      rumble: () => get().prefs.rumble,
      onPhase: (p) => {
        if (get().stream !== stream) return;
        set({ phase: p });
        if (p === "ended") {
          dualSense?.stop();
          rpInput.held.clear();
          set({ stream: null, light: null, keyboard: null, passcode: null, sessionQuality: null });
        }
      },
      onStats: (st) => get().stream === stream && set({ stats: st }),
      onError: (e) => get().stream === stream && set({ error: e }),
      onLight: (rgb) => get().stream === stream && set({ light: rgb }),
      onHidden: () => {
        if (get().stream !== stream) return;
        // Picture in picture is the stream on show: its window has its own keys and focus.
        const pip = get().pipWindow;
        if (pip && !pip.closed) return;
        rpInput.held.clear();
        rpInput.touch = null;
        rpInput.touchPad = IDLE_PAD;
      },
      onKbm: (v) => {
        if (get().stream !== stream) return;
        const kbm = useKbmStore.getState();
        const next = readKbmStatus(v);
        // Nobody signed in: the PS5 has no user to give a keyboard to. Said once per change.
        if (next?.err === "vinput_no_user" && kbm.status?.err !== "vinput_no_user") askToSignIn(t.host);
        kbm.setStatus(next);
      },
      onPadKbm: (v) => get().stream === stream && emitPadKbm(v),
      onKeyboard: (m) => get().stream === stream && set({ keyboard: nextKeyboard(get().keyboard, m) }),
      onPasscode: (ask) => {
        if (get().stream !== stream) return;
        set({ passcode: ask ? { ...ask, seq: (get().passcode?.seq ?? 0) + 1 } : null });
      },
      readPad,
      gamepads: rpGamepads,
    });
    stream.setVolume(prefs.muted ? 0 : prefs.volume);
    set({ stream, target: t, phase: "connecting", error: null, stats: null, light: null, keyboard: null, passcode: null, sessionQuality: quality, pictureHere: pictureHere !== false });
    await stream.start();
  },

  stopForRestart() {
    // A restart with other settings: the engine keeps friends' places through it.
    get().stream?.stop();
  },

  stop() {
    // A stop someone asked for (Stop, the banner, the mini player): Play Together on that
    // console ends with it, so friends are told at once instead of waiting out the window the
    // engine keeps for a host whose stream dropped by itself.
    const host = get().stream ? get().target?.host : undefined;
    if (host) {
      void import("../screens/Play/playTogetherState")
        .then(({ joinApi }) => joinApi.close(host))
        .catch(() => {});
    }
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
      s.setPicture(prefs.picture);
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
useRemotePlayStore.subscribe((s) => setGameMode(!!s.stream && s.phase === "playing" && s.pictureHere));

// Keep the screen on while playing (a controller does not count as touching the phone). From the
// store, not a screen: the stream keeps playing in the mini player on other screens.
useRemotePlayStore.subscribe((s) => setScreenWakeReason("remote-play", !!s.stream && s.phase === "playing"));

// Controllers are the page's while there is a stream (lib/androidPadBridge).
useRemotePlayStore.subscribe((s) => setPadForwardReason("remote-play", !!s.stream && s.pictureHere));

// A DualSense's touchpad (lib/padTouchpad.ts) is read while a stream plays; each change sends the
// pad at once, like a key, so a quick tap is not lost between two frames.
setPadTouchpad(false, () => useRemotePlayStore.getState().stream?.sendPad());
useRemotePlayStore.subscribe((s) => setPadTouchpad(!!s.stream && s.phase === "playing" && s.pictureHere));

/** The session the engine is running, if any (it outlives a reload of this page). */
export async function fetchRunningSession(): Promise<{
  host: string | null;
  playing?: boolean;
  attached?: boolean;
  /** It was told to stop and is finishing (a wake or handshake it can't cut short at once). */
  stopping?: boolean;
  /** False: the picture is off on this device for that session. */
  picture?: boolean;
}> {
  try {
    const r = await fetch(`${getEngineUrl()}/api/ps5/rp/session?passive=1`);
    return r.ok ? await r.json() : { host: null };
  } catch {
    return { host: null };
  }
}
