// One guest's Play Together session (#641): the link to the host's engine (lib/joinClient), the
// picture (pad/watchPlayer, in the player's low-latency mode) and the pad this device sends.
// Shared by the /join page a friend opens in any browser and the app's own Join a friend view;
// React reads it through useJoinSession (useSyncExternalStore).

import { useSyncExternalStore } from "react";

import { JoinClient, type JoinStage } from "../lib/joinClient";
import type { JoinStatus } from "../lib/joinProtocol";
import { gamepadsOf } from "../lib/nativePad";
import { profileFor } from "../lib/padProfiles";
import { mergePads, padFromGamepads, playRumble, type PadState } from "../lib/remotePlayStream";
import { makeWatchPlayer, type MsePlayer, type WatchPlayer, type WatchProblem } from "../pad/watchPlayer";
import { rpInput } from "../state/remotePlay";

export interface JoinSnapshot {
  stage: JoinStage;
  status: JoinStatus | null;
  /** This page's own round trip, ms. */
  rttMs: number | null;
  light: [number, number, number] | null;
  problem: WatchProblem | null;
  sound: boolean;
  /** The host's name: from the link, then from the engine's status. */
  host: string | null;
}

export interface JoinSessionOptions {
  /** The guest socket (direct, or through this app's engine). */
  url: string;
  invite: string | null;
  code?: string | null;
  name: string;
  host: string | null;
}

/** The pad this device is: connected controllers (standard mapping) plus the on-screen one. */
export function devicePad(): PadState {
  const pads = padFromGamepads(gamepadsOf(typeof navigator === "undefined" ? undefined : navigator), profileFor);
  const touch = rpInput.touchPad;
  return { ...mergePads(pads, touch), touch: touch.touch ?? null, motion: rpInput.motion ?? null };
}

export class JoinSession {
  readonly client: JoinClient;
  private player: WatchPlayer | MsePlayer | null = null;
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
      host: opts.host,
    };
    this.client = new JoinClient({
      url: opts.url,
      invite: opts.invite,
      code: opts.code ?? null,
      name: opts.name,
      readPad: devicePad,
      onStage: (stage) => {
        this.set({ stage });
        if (stage.kind === "refused" || stage.kind === "ended") this.teardown();
      },
      onStatus: (status) => this.set({ status, host: status.host ?? this.snap.host }),
      onRtt: (rttMs) => this.set({ rttMs }),
      onFrame: (d) => this.picture().onMessage(d),
      onCodec: (h) => this.picture().setHevc(h),
      onRumble: (strong, weak) => {
        const pads = gamepadsOf(typeof navigator === "undefined" ? undefined : navigator);
        playRumble(pads, strong, weak);
        // A phone playing on its screen has one motor.
        if (!pads.some((g) => g?.connected)) navigator.vibrate?.(strong || weak ? 200 : 0);
      },
      onLight: (light) => this.set({ light }),
    });
  }

  /** The picture's player, made on first use (needs a document). */
  picture(): WatchPlayer | MsePlayer {
    if (!this.player) this.player = makeWatchPlayer(
        (problem) => this.set({ problem }),
        "player",
        () => this.client.askKeyframe(),
      );
    return this.player;
  }

  /** The element the picture is drawn in (moved into whichever view shows it). */
  get element(): HTMLElement {
    return this.picture().element;
  }

  start(): void {
    // Hidden or out of focus: let go of everything (no stuck stick while the friend is away).
    const vis = () => this.client.setNeutral(document.visibilityState === "hidden");
    const blur = () => this.client.setNeutral(true);
    const focus = () => this.client.setNeutral(document.visibilityState === "hidden");
    document.addEventListener("visibilitychange", vis);
    window.addEventListener("blur", blur);
    window.addEventListener("focus", focus);
    this.cleanups.push(() => {
      document.removeEventListener("visibilitychange", vis);
      window.removeEventListener("blur", blur);
      window.removeEventListener("focus", focus);
    });
    this.client.start();
  }

  startSound(): void {
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
