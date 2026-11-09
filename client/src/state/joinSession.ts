// The app as a Play Together guest (#641): the friend's game this app joined, if any. Like the
// app's own Remote Play session (state/remotePlay), it belongs to the app, not to a screen:
// leaving Play keeps playing (the controller keeps sending) and coming back shows it again.
//
// One thing at a time, app-wide: the app holds at most one stream, its own Remote Play or a
// friend's game. The Play screen asks before switching (screens/Play/JoinAFriend.tsx).

import { create } from "zustand";

import { joinSocketUrl, type JoinLink } from "../lib/joinProtocol";
import { urlPath } from "../join/netPath";
import { joinTries, raceTransport } from "../join/pathRace";
import { holdWhileJoined, JoinSession } from "../join/session";
import { hubSocketUrl, rtcTransport, type JoinSocket } from "../join/rtcTransport";
import { getEngineUrl } from "./engine";
import { useJoinPresence } from "./joinPresence";

/** The app reaches the friend's engine through its own (an app page may not open a plain ws://
 *  to another machine: Android blocks cleartext, iOS ATS; its own loopback it may), the way the
 *  phone app reaches a pad socket (pad_link.rs relay). */
export function joinRelayUrl(engineUrl: string, target: string): string {
  const base = engineUrl.replace(/^http/, "ws").replace(/\/$/, "");
  return `${base}/api/join/relay?url=${encodeURIComponent(target)}`;
}

interface JoinStoreState {
  session: JoinSession | null;
  link: JoinLink | null;
  /** A new session for `link` (whatever was joined before is left first). */
  join: (link: JoinLink, name: string) => JoinSession;
  /** Leave the friend's game (says so to their engine). */
  leave: () => void;
  /** The session ended on its own (refused, kicked…): forget it. */
  clear: () => void;
}

export const useJoinStore = create<JoinStoreState>((set, get) => ({
  session: null,
  link: null,
  join: (link, name) => {
    get().session?.leave();
    // The one link for everywhere (12.1): peer to peer, as the friend's browser page does; ICE
    // keeps it on the Wi-Fi when both are home. It is signalled through the host's own engine on
    // their Wi-Fi or tailnet too, racing the hub, and a Wi-Fi link joins over WebRTC the same way
    // (join/pathRace): no internet or hub needed at home. The plain socket (through this app's
    // engine: an app page may not open ws:// to another machine) only when WebRTC can't connect,
    // or this web view has none.
    const log = (m: string) => console.info(`[join] ${m}`);
    const engineUrl = getEngineUrl();
    const rtc = typeof RTCPeerConnection !== "undefined";
    const net = link.net ?? null;
    const transport = rtc
      ? raceTransport(
          () =>
            joinTries({
              net,
              lan: link.lan ?? (net ? null : `${link.origin}/join`),
              ts: link.ts ?? null,
              fp: net?.fp ?? link.fp ?? null,
              rtc,
              reach: (ws) => joinRelayUrl(engineUrl, ws),
              rtcMake: (inv, o) => rtcTransport(inv, { video: true, log, ...o }),
              socket: (url) => new WebSocket(url) as unknown as JoinSocket,
            }),
          { room: net?.room ?? link.origin, log },
        )
      : null;
    const path = net ? null : urlPath(link.origin);
    const s = new JoinSession({
      url: net && rtc ? hubSocketUrl(net.hub) : joinRelayUrl(engineUrl, joinSocketUrl(link)),
      ...(transport ? { transport } : {}),
      path,
      internet: !!net,
      invite: link.invite,
      code: link.code ?? null,
      name,
      host: link.name,
      keysLive: () => useJoinPresence.getState().onScreen,
    });
    set({ session: s, link });
    useJoinPresence.setState({ host: link.name, leave: () => get().leave() });
    s.start();
    return s;
  },
  leave: () => {
    get().session?.leave();
    set({ session: null, link: null });
    useJoinPresence.setState({ host: null, leave: null });
  },
  clear: () => {
    get().session?.stop();
    set({ session: null, link: null });
    useJoinPresence.setState({ host: null, leave: null });
  },
}));

/** The friend's game this app is in (connecting, asking, playing or reconnecting), or null. */
export function activeJoin(): { session: JoinSession; host: string } | null {
  const { session } = useJoinStore.getState();
  if (!session) return null;
  const k = session.snapshot.stage.kind;
  if (k === "refused" || k === "ended") return null;
  return { session, host: session.snapshot.host ?? session.opts.host ?? "" };
}

// From the store, not a screen: leaving Play keeps playing.
let joinHold: { session: JoinSession; undo: () => void } | null = null;
useJoinStore.subscribe(({ session }) => {
  if (joinHold?.session === session) return;
  joinHold?.undo();
  joinHold = session ? { session, undo: holdWhileJoined(session) } : null;
});
