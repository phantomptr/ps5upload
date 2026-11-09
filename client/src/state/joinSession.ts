// The app as a Play Together guest (#641): the friend's game this app joined, if any. Like the
// app's own Remote Play session (state/remotePlay), it belongs to the app, not to a screen:
// leaving Play keeps playing (the controller keeps sending) and coming back shows it again.
//
// One thing at a time, app-wide: the app holds at most one stream, its own Remote Play or a
// friend's game. The Play screen asks before switching (screens/Play/JoinAFriend.tsx).

import { create } from "zustand";

import { joinSocketUrl, type InternetInvite, type JoinLink } from "../lib/joinProtocol";
import { urlPath } from "../join/netPath";
import { raceTransport, type PathTry } from "../join/pathRace";
import { holdWhileJoined, JoinSession } from "../join/session";
import { hubSocketUrl, rtcTransport, type JoinSocket, type JoinTransport } from "../join/rtcTransport";
import { getEngineUrl } from "./engine";
import { useJoinPresence } from "./joinPresence";

/** The app reaches the friend's engine through its own (an app page may not open a plain ws://
 *  to another machine: Android blocks cleartext, iOS ATS; its own loopback it may), the way the
 *  phone app reaches a pad socket (pad_link.rs relay). */
export function joinRelayUrl(engineUrl: string, target: string): string {
  const base = engineUrl.replace(/^http/, "ws").replace(/\/$/, "");
  return `${base}/api/join/relay?url=${encodeURIComponent(target)}`;
}

/** The host engine's signalling socket beside its page (`http://host:port[/prefix]/join` →
 *  `ws://host:port[/prefix]/api/join/signal`), or null when it is not such a page. */
export function signalSocketUrl(page: string): string | null {
  if (!/^https?:\/\/[^/]+(\/.*)?\/join\/?$/.test(page)) return null;
  return page.replace(/^http/, "ws").replace(/\/join\/?$/, "/api/join/signal");
}

/** The ways the app tries an internet invite, best first: signalled through the host's own engine
 *  on their Wi-Fi (the link's `lan`) and on Tailscale (`ts`), both through this app's engine (an
 *  app page may not open a plain ws:// to another machine), and through the hub. Each is the same
 *  WebRTC connection with the link's fingerprint checked (join/pathRace). */
export function joinTries(
  link: Pick<JoinLink, "lan" | "ts">,
  net: InternetInvite,
  engineUrl: string,
  make: (inv: InternetInvite, signalUrl?: string) => JoinTransport,
): PathTry[] {
  const out: PathTry[] = [];
  for (const [via, page] of [
    ["lan", link.lan],
    ["tailscale", link.ts],
  ] as const) {
    const sig = page ? signalSocketUrl(page) : null;
    if (!sig) continue;
    const T = make(net, joinRelayUrl(engineUrl, sig));
    out.push({ via, make: () => new T("") as unknown as JoinSocket });
  }
  const H = make(net);
  out.push({ via: "hub", make: () => new H("") as unknown as JoinSocket });
  return out;
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
    // keeps it on the Wi-Fi when both are home. A web view with no WebRTC joins the host's own
    // page instead, when the link names it. On the host's Wi-Fi (or tailnet) it signals through the host's own engine too, racing the
    // hub (join/pathRace): no internet or hub needed at home, and the first way to open plays.
    const net = link.net && typeof RTCPeerConnection !== "undefined" ? link.net : null;
    const log = (m: string) => console.info(`[join] ${m}`);
    const engineUrl = getEngineUrl();
    const transport = net
      ? raceTransport(
          () => joinTries(link, net, engineUrl, (inv, signalUrl) => rtcTransport(inv, { video: true, log, ...(signalUrl ? { signalUrl } : {}) })),
          { room: net.room, log },
        )
      : null;
    const path = net ? null : urlPath(link.origin);
    const s = new JoinSession({
      url: net ? hubSocketUrl(net.hub) : joinRelayUrl(engineUrl, joinSocketUrl(link)),
      ...(transport ? { transport } : {}),
      path,
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
