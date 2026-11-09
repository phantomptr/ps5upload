// One guest's Play Together session (#641): the link to the host's engine (lib/joinClient), the
// picture (pad/watchPlayer, in the player's low-latency mode) and the pad this device sends.
// Shared by the /join page a friend opens in any browser and the app's own Join a friend view;
// React reads it through useJoinSession (useSyncExternalStore). The link runs over a WebSocket on
// the LAN, or over a WebRTC data channel for an internet invite (rtcTransport.ts): the same
// messages either way, so the picture, the sound, the pad and the stages are the same.

import { useSyncExternalStore } from "react";

import { JoinClient, type JoinStage } from "../lib/joinClient";
import type { JoinStatus } from "../lib/joinProtocol";
import { gamepadsOf } from "../lib/nativePad";
import { profileFor } from "../lib/padProfiles";
import {
  DEFAULT_KEYMAP,
  IDLE_PAD,
  mergePads,
  padFromGamepads,
  padFromKeys,
  playRumble,
  type KeyAction,
  type PadState,
} from "../lib/remotePlayStream";
import { makeWatchPlayer, type MsePlayer, type SoundReport, type WatchPlayer, type WatchProblem } from "../pad/watchPlayer";
import { rpInput } from "../state/rpInput";
import { setScreenWakeReason } from "../lib/androidScreenWake";
import { setPadForwardReason } from "../lib/androidPadBridge";
import type { NetPath } from "./netPath";
import type { JoinTransport } from "./rtcTransport";
import { TrackPlayer } from "./trackPlayer";

export interface JoinSnapshot {
  stage: JoinStage;
  status: JoinStatus | null;
  /** This page's own round trip, ms. */
  rttMs: number | null;
  light: [number, number, number] | null;
  problem: WatchProblem | null;
  sound: boolean;
  /** The sound cannot play in this browser (#657): said, rather than a silent game. */
  noSound: boolean;
  /** The host's name: from the link, then from the engine's status. */
  host: string | null;
  /** The guest muted the game's sound (remembered on this device). */
  muted: boolean;
  /** Which way the connection goes while joined (join/netPath), when it is known. */
  path: NetPath | null;
}

const MUTED_KEY = "ps5upload.join.muted";

export interface JoinSessionOptions {
  /** The page shows the game now (the app: its Play screen). Absent: always. */
  keysLive?: () => boolean;
  /** The guest socket (direct, or through this app's engine). */
  url: string;
  /** What carries the session: the WebSocket to `url` (the LAN, the default), or an internet
   *  invite's WebRTC data channel (`rtcTransport`, which ignores `url`). */
  transport?: JoinTransport;
  invite: string | null;
  code?: string | null;
  name: string;
  host: string | null;
  /** The way a plain socket goes (the host's page on their Wi-Fi: join/netPath hostPath). An
   *  internet invite's transport says it itself once joined. */
  path?: NetPath | null;
  /** An internet invite (its words for a dropped connection are the internet's); absent: it is
   *  one when it has a transport. A Wi-Fi link's WebRTC join is not. */
  internet?: boolean;
}

/** How often the way is read again while joined (ICE may move to another pair). */
export const PATH_EVERY_MS = 5000;

/** A guest's keyboard (#654): Remote Play's default keys, except Escape, which stays the
 *  browser's (it leaves full screen). */
export const GUEST_KEYMAP: Record<string, KeyAction> = Object.fromEntries(
  Object.entries(DEFAULT_KEYMAP).filter(([code]) => code !== "Escape"),
);

/** The keys held on this device (KeyboardEvent.code), while a session listens. */
export const guestHeld = new Set<string>();

/** Whether a key event is the guest's pad: a mapped key, not typed into a field. */
export function isPadKey(
  e: Pick<KeyboardEvent, "code" | "target" | "ctrlKey" | "metaKey" | "altKey"> & { shiftKey?: boolean },
): boolean {
  if (!(e.code in GUEST_KEYMAP) || e.ctrlKey || e.metaKey || e.altKey) return false;
  const t = e.target as (HTMLElement & { isContentEditable?: boolean }) | null;
  // Tab is the game's Options: Shift+Tab is how a keyboard reaches the page's own buttons (Leave),
  // and from there Tab moves between them.
  if (e.code === "Tab" && (e.shiftKey || isControl(t))) return false;
  const tag = t?.tagName;
  return !(tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t?.isContentEditable || isControl(t));
}

/** A button or a link with the focus: Enter and Space are its own. */
function isControl(t: HTMLElement | null): boolean {
  const tag = t?.tagName;
  if (tag === "BUTTON" || tag === "A") return true;
  const role = t?.getAttribute?.("role");
  return role === "button" || role === "link" || role === "menuitem" || role === "tab" || role === "switch";
}

let padChecking = false;

/** While the guest checks their controller, the game gets an idle pad. */
export function setPadChecking(on: boolean): void {
  padChecking = on;
}

/** What the page tells the host about its picture: none before the first picture, unless the
 *  picture fails, which the host then hears about with the decoder and codec (#720: an iPhone
 *  that never drew a picture sent nothing, so the host could not say which decoder failed). The
 *  sound's path and counters go with it once the sound started (#720: an iPhone guest's sound
 *  stuttered and the host's log said nothing about it). */
export function pageStats(
  stats: { frames: number; snapshot(): Record<string, number | string> | null } | null,
  problem: WatchProblem | null,
  sound: SoundReport | null = null,
): Record<string, number | string> | null {
  if (!stats || (stats.frames === 0 && !problem)) return null;
  const s = stats.snapshot();
  if (!s) return s;
  return { ...s, ...(problem ? { problem } : {}), ...(sound ?? {}) };
}

/** The pad this device is: connected controllers (each through its profile), the keyboard and the
 *  on-screen one, merged. */
export function devicePad(): PadState {
  // Checking the controller (PadCheck): its presses are not the game's.
  if (padChecking) return { ...IDLE_PAD };
  const pads = padFromGamepads(gamepadsOf(typeof navigator === "undefined" ? undefined : navigator), profileFor);
  const touch = rpInput.touchPad;
  const merged = mergePads(mergePads(pads, padFromKeys(guestHeld, GUEST_KEYMAP)), touch);
  return { ...merged, touch: touch.touch ?? null, motion: rpInput.motion ?? null };
}

export class JoinSession {
  readonly client: JoinClient;
  private player: WatchPlayer | MsePlayer | TrackPlayer | null = null;
  private snap: JoinSnapshot;
  private listeners = new Set<() => void>();
  private cleanups: Array<() => void> = [];

  constructor(readonly opts: JoinSessionOptions) {
    this.snap = {
      stage: { kind: "connecting" },
      status: null,
      rttMs: null,
      light: null,
      problem: null,
      sound: false,
      noSound: false,
      host: opts.host,
      muted: readMuted(),
      path: null,
    };
    this.client = new JoinClient({
      url: opts.url,
      invite: opts.invite,
      code: opts.code ?? null,
      name: opts.name,
      WebSocketImpl: opts.transport,
      readPad: devicePad,
      onStage: (stage) => {
        this.set({ stage });
        if (stage.kind === "joined") void this.readPath();
        if (stage.kind === "refused" || stage.kind === "ended") this.teardown();
      },
      onStatus: (status) => this.set({ status, host: status.host ?? this.snap.host }),
      onRtt: (rttMs) => this.set({ rttMs }),
      onFrame: (d) => this.picture().onMessage(d),
      // How the picture looks here, for the host's panel and log: none before the first picture,
      // unless the picture fails, which the host then hears about (#720).
      readStats: () => pageStats(this.player?.stats ?? null, this.snap.problem, this.player?.soundReport() ?? null),
      onCodec: (h, track, audio) => {
        const p = this.picture();
        p.setHevc(h);
        if (p instanceof TrackPlayer) {
          p.setTrack(track, !!audio);
          // The pictures come on the track now: a problem the channel's decoder had is not on show.
          if (track && this.snap.problem) this.set({ problem: null });
        }
      },
      onRumble: (strong, weak) => {
        const pads = gamepadsOf(typeof navigator === "undefined" ? undefined : navigator);
        playRumble(pads, strong, weak);
        // A phone playing on its screen has one motor.
        if (!pads.some((g) => g?.connected)) navigator.vibrate?.(strong || weak ? 200 : 0);
      },
      onLight: (light) => this.set({ light }),
    });
  }

  /** The picture's player, made on first use (needs a document). An internet invite's may take
   *  the pictures as a video track (trackPlayer.ts). */
  picture(): WatchPlayer | MsePlayer | TrackPlayer {
    if (!this.player) {
      const p = makeWatchPlayer(
        (problem) => this.set({ problem }),
        "player",
        () => this.client.askKeyframe(),
      );
      this.player = this.opts.transport ? new TrackPlayer(p) : p;
      this.player.onSoundFail = () => this.set({ noSound: true });
      this.player.setMuted(this.snap.muted);
    }
    return this.player;
  }

  /** Which way the connection goes now: the transport's own word (an internet invite's selected
   *  pair), else what the plain socket's address says. */
  async readPath(): Promise<void> {
    const s = this.client.socket as { netPath?: () => Promise<NetPath | null> } | null;
    let path: NetPath | null = this.opts.path ?? null;
    if (s?.netPath) {
      try {
        path = (await s.netPath()) ?? this.opts.path ?? null;
      } catch {
        path = null;
      }
    }
    if (path !== this.snap.path) this.set({ path });
  }

  /** The element the picture is drawn in (moved into whichever view shows it). */
  get element(): HTMLElement {
    return this.picture().element;
  }

  start(): void {
    // Hidden or out of focus: let go of everything (no stuck stick while the friend is away).
    // Keys held then are let go too (their keyup goes to whatever has the focus next).
    const vis = () => {
      if (document.visibilityState === "hidden") guestHeld.clear();
      // Back: sound the browser stopped while away plays again.
      else this.player?.resumeSound();
      this.client.setNeutral(document.visibilityState === "hidden");
    };
    const blur = () => {
      guestHeld.clear();
      this.client.setNeutral(true);
    };
    // The keyboard plays (#654) while joined; a key typed into a field stays the field's.
    const keyDown = (e: KeyboardEvent) => {
      if (this.client.current.kind !== "joined" || !isPadKey(e)) return;
      // The app's other screens are the user's own: the keyboard plays only on the game's.
      if (this.opts.keysLive && !this.opts.keysLive()) return;
      e.preventDefault();
      guestHeld.add(e.code);
    };
    const keyUp = (e: KeyboardEvent) => {
      if (!guestHeld.delete(e.code)) return;
      if (isPadKey(e)) e.preventDefault();
    };
    window.addEventListener("keydown", keyDown);
    window.addEventListener("keyup", keyUp);
    const focus = () => {
      this.client.setNeutral(document.visibilityState === "hidden");
      if (document.visibilityState !== "hidden") this.player?.resumeSound();
    };
    document.addEventListener("visibilitychange", vis);
    window.addEventListener("blur", blur);
    window.addEventListener("focus", focus);
    const pathTimer = setInterval(() => {
      if (this.client.current.kind === "joined") void this.readPath();
    }, PATH_EVERY_MS);
    this.cleanups.push(() => {
      clearInterval(pathTimer);
      document.removeEventListener("visibilitychange", vis);
      window.removeEventListener("blur", blur);
      window.removeEventListener("focus", focus);
      window.removeEventListener("keydown", keyDown);
      window.removeEventListener("keyup", keyUp);
      guestHeld.clear();
    });
    this.client.start();
  }

  /** Mute or unmute the game's sound; remembered for the next game. */
  setMuted(m: boolean): void {
    this.player?.setMuted(m);
    try {
      localStorage.setItem(MUTED_KEY, m ? "1" : "0");
    } catch {
      // Private mode: this game only.
    }
    this.set({ muted: m });
  }

  startSound(): void {
    // Once: the first touch anywhere and the "Tap for sound" button both call this (#657).
    if (this.snap.sound) return;
    this.picture().startSound();
    this.set({ sound: true });
  }

  leave(): void {
    this.client.leave();
    this.teardown();
  }

  /** The page is going away: keep the place for a reload (the token stays). */
  stop(): void {
    this.client.stop();
    this.teardown();
  }

  private teardown(): void {
    for (const c of this.cleanups.splice(0)) c();
    rpInput.touchPad = { buttons: 0, l2: 0, r2: 0, lx: 0, ly: 0, rx: 0, ry: 0 };
    const p = this.player;
    this.player = null;
    if (p) {
      p.stop();
      p.element.remove();
    }
  }

  /** What the on-screen controller calls on every touch. */
  sendPad(): void {
    this.client.sendPad(false);
  }

  get snapshot(): JoinSnapshot {
    return this.snap;
  }

  private set(p: Partial<JoinSnapshot>): void {
    this.snap = { ...this.snap, ...p };
    for (const l of this.listeners) l();
  }

  subscribe = (l: () => void): (() => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
}

export function useJoinSession(s: JoinSession): JoinSnapshot {
  return useSyncExternalStore(s.subscribe, () => s.snapshot);
}

function readMuted(): boolean {
  try {
    return typeof localStorage !== "undefined" && localStorage.getItem(MUTED_KEY) === "1";
  } catch {
    return false;
  }
}

/** While `session` is alive (connecting, asking, playing or reconnecting): the screen stays on (a
 *  controller does not count as touching the phone; when it sleeps the page hides, the pad goes
 *  neutral and the game drops to Reconnecting) and controllers are the page's. Returns the undo. */
export function holdWhileJoined(session: JoinSession): () => void {
  const apply = () => {
    const k = session.snapshot.stage.kind;
    const on = k !== "refused" && k !== "ended";
    setScreenWakeReason("play-together", on);
    setPadForwardReason("play-together", on);
  };
  apply();
  const off = session.subscribe(apply);
  return () => {
    off();
    setScreenWakeReason("play-together", false);
    setPadForwardReason("play-together", false);
  };
}
