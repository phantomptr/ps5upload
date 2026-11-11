import { useEffect, useRef } from "react";
import { create } from "zustand";

import { hostOf } from "../../../lib/addr";
import type { LaunchTarget } from "../../../lib/useGameLaunch";
import { useConnectionStore } from "../../../state/connection";

/** How long a woken console has to bring its helper back before the launch gives up. */
export const HELPER_WAIT_MS = 120_000;

/** Where a "play on the stream" is. */
export type StreamLaunchStage =
  /** The stream is starting (it wakes the console and signs the user in). */
  | "stream"
  /** The stream plays; the console's helper is not back yet (it is sent again after a wake). */
  | "helper"
  /** The title is being started (and confirmed running). */
  | "launch"
  | "failed";

export interface StreamLaunch extends LaunchTarget {
  /** The console to play it on (bare host). */
  host: string;
  stage: StreamLaunchStage;
  /** Why it stopped (stage "failed"). */
  error?: string;
}

interface State {
  pending: StreamLaunch | null;
  /** A console (its roster host) whose stream was asked for from another screen, with no game
   *  to start. The stream panel takes it and starts through its own guards (leave a friend's
   *  game first, a payload that blocks the picture), which a direct store start would skip. */
  streamOnly: string | null;
  requestStream: (host: string) => void;
  /** Hand the stream-only request over (once): the host, or null when there is none. */
  takeStream: () => string | null;
  request: (host: string, t: LaunchTarget) => void;
  set: (p: Partial<StreamLaunch>) => void;
  clear: () => void;
}

/** One "play on the stream" at a time: the latest request replaces an earlier one. */
export const useStreamLaunch = create<State>((set, get) => ({
  pending: null,
  streamOnly: null,
  requestStream: (host) => set({ streamOnly: host }),
  takeStream: () => {
    const host = get().streamOnly;
    if (host) set({ streamOnly: null });
    return host;
  },
  request: (host, t) => set({ pending: { ...t, host: hostOf(host) || host, stage: "stream" } }),
  set: (p) => set((s) => (s.pending ? { pending: { ...s.pending, ...p } } : s)),
  clear: () => set({ pending: null }),
}));

/** What the runner needs from the stream panel. */
export interface RunnerDeps {
  /** The stream is up with this console (the picture shows). */
  playingHost: string | null;
  /** A stream is on (any console, any phase). */
  streamingHost: string | null;
  /** Why the stream stopped or did not start, translated; null when nothing went wrong. */
  streamError: string | null;
  /** Start (or switch to) the stream with `host`. Resolves once it was asked for. */
  startStream: (host: string) => Promise<void>;
  /** Start the title on the connected console (useGameLaunch's launch). */
  launch: (t: LaunchTarget) => Promise<void>;
  /** A sentence for a failure, translated by the caller. */
  messages: { notConnected: (name: string) => string; noHelper: string };
}

/**
 * Carries a pending "play on the stream" through: stream → helper → launch. Lives in the stream
 * panel, which owns both the stream and the launcher. Each step starts once; a failure stays on
 * screen until dismissed.
 */
export function useStreamLaunchRunner(d: RunnerDeps) {
  const pending = useStreamLaunch((s) => s.pending);
  const helperUp = useConnectionStore((s) =>
    pending ? s.runtimeByHost[pending.host]?.payloadStatus === "up" : false,
  );
  const connectedHost = useConnectionStore((s) => hostOf(s.host ?? ""));
  const started = useRef<string | null>(null);
  const launched = useRef<string | null>(null);
  const helperSince = useRef(0);
  const deps = useRef(d);
  useEffect(() => {
    deps.current = d;
  });

  useEffect(() => {
    if (!pending || pending.stage === "failed") return;
    const st = useStreamLaunch.getState();
    const key = `${pending.host}|${pending.titleId}`;
    // The launch goes to the connected console: it must be the streamed one.
    if (connectedHost !== pending.host) {
      st.set({ stage: "failed", error: deps.current.messages.notConnected(pending.host) });
      return;
    }
    if (d.playingHost !== pending.host) {
      // Asked for, and it failed: no stage to wait on any more. (An error from before the request
      // does not count: the start below clears it.)
      if (started.current === key && d.streamError) {
        st.set({ stage: "failed", error: d.streamError });
        return;
      }
      if (started.current !== key && d.streamingHost !== pending.host) {
        started.current = key;
        void deps.current.startStream(pending.host);
      }
      if (pending.stage !== "stream") st.set({ stage: "stream" });
      return;
    }
    if (!helperUp) {
      if (pending.stage !== "helper") {
        helperSince.current = Date.now();
        st.set({ stage: "helper" });
      }
      return;
    }
    if (launched.current === key) return;
    launched.current = key;
    st.set({ stage: "launch" });
    void deps.current
      .launch({ titleId: pending.titleId, titleName: pending.titleName, source: pending.source })
      .finally(() => {
        // The launcher reports its own outcome (Now playing, a slow start, a refusal).
        if (useStreamLaunch.getState().pending?.titleId === pending.titleId) st.clear();
        launched.current = null;
        started.current = null;
      });
  }, [pending, helperUp, connectedHost, d.playingHost, d.streamingHost, d.streamError]);

  // Waiting on the helper has an end.
  useEffect(() => {
    if (pending?.stage !== "helper") return;
    const left = HELPER_WAIT_MS - (Date.now() - helperSince.current);
    const t = window.setTimeout(() => {
      const p = useStreamLaunch.getState().pending;
      if (p?.stage === "helper") useStreamLaunch.getState().set({ stage: "failed", error: deps.current.messages.noHelper });
    }, Math.max(0, left));
    return () => window.clearTimeout(t);
  }, [pending?.stage]);

  // Dismissing a failure (or a new request) starts over.
  useEffect(() => {
    if (!pending) {
      started.current = null;
      launched.current = null;
    }
  }, [pending]);
}
