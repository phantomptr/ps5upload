// The page a friend's browser opens from the host's Play Together QR code or link (#641), served
// by the host's engine at /join and, for internet invites, by the website at /join/ (built by
// vite.join.config.ts into join_page.html and site/join/index.html). The invite key is in the
// address's #fragment, so it never reaches a server log; the page gives it to the socket in its
// hello.
// First: stand-ins for what an older WebKit lacks (lib/polyfills), before any other module runs.
import "../lib/polyfills";
import { createRoot } from "react-dom/client";
import "../index.css";
import { JoinFlow } from "./JoinFlow";
import { hubSocketUrl, parseInternetFragment, prejoinHub, rtcTransport, type JoinSocket } from "./rtcTransport";
import { lanPage, lanTarget } from "./lanHint";
import { hostPath } from "./netPath";
import { joinTries, raceTransport } from "./pathRace";
import { fpOf, parsePs5Fragment } from "../lib/joinProtocol";
import { freshTarget, saveInvite, saveTarget } from "./ps5Handoff";
import { Ps5Page } from "./Ps5Page";
import { installArrowNav } from "../lib/tvNav";
import { parseRoomFragment } from "./roomFragment";

// A TV's remote (#926): the arrows move from button to button, OK presses one.
installArrowNav();

const hash = new URLSearchParams(location.hash.slice(1));
const host = hash.get("n")?.trim() || null;
// "Play on my PS5" (ps5Handoff.ts): the QR code on PS5Upload Play's screen (`#ps5=…`), or an
// invite page on the home network handing its link over (`#play=<link>`).
const ps5 = parsePs5Fragment(location.hash);
const play = hash.get("play");
const handoff = !!ps5 || play !== null;
if (ps5) saveTarget(ps5);
if (play) saveInvite(play);
// An internet invite (the same page at https://ps5upload.phantomptr.com/join/): peer to peer over
// WebRTC, introduced by the hub; nothing goes to this page's own origin. On the host's Wi-Fi the
// connection stays on it (ICE), and a link from a newer host names its page there (`lan`) for
// when the hub can't be reached ("Open on this Wi-Fi").
const net = handoff ? null : parseInternetFragment(location.hash);
// A Play Room from the website's Rooms page (or a friend's room): the same join, knocking first.
const roomFrag = handoff || net ? null : parseRoomFragment(location.hash);
// An invite opened on this phone: offered to the PS5 scanned on it (for 30 minutes).
if (net || hash.get("i")) saveInvite(location.href);
// The hub's socket first, before React draws anything: in the room by the time the friend has
// read the page and typed their name, so Join goes straight to the offer.
if (net) prejoinHub(net);
const root = createRoot(document.getElementById("root")!);
if (handoff) {
  root.render(<Ps5Page target={ps5 ?? freshTarget()} />);
} else if (roomFrag) {
  root.render(<JoinFlow url={hubSocketUrl(roomFrag.hub)} invite={null} host={roomFrag.host} room={roomFrag} />);
} else if (net) {
  const transport = rtcTransport(net, { video: true, log: (m) => console.info(`[join] ${m}`) });
  const lan = lanPage(location.hash, "join");
  root.render(
    <JoinFlow
      url={hubSocketUrl(net.hub)}
      invite={net.key}
      host={host}
      transport={transport}
      lanUrl={lan ? lanTarget(lan, { key: net.key }) : null}
    />,
  );
} else {
  const invite = hash.get("i") || null;
  // The engine may be served under a path (PS5UPLOAD_BASE_URL): the socket is beside this page.
  const prefix = location.pathname.replace(/\/join\/?$/, "");
  const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${prefix}/api/join/ws`;
  // WebRTC first, signalled on this engine's own socket (no hub): the picture and sound as RTP
  // into a <video> (an http page has no WebCodecs, and an iPhone's MSE is limited), checked
  // against the link's certificate when it names one. The plain socket when WebRTC can't connect.
  const log = (m: string) => console.info(`[join] ${m}`);
  const page = `${location.origin}${prefix}/join`;
  const transport =
    typeof RTCPeerConnection === "undefined"
      ? undefined
      : raceTransport(
          () =>
            joinTries({
              net: null,
              lan: page,
              ts: null,
              fp: fpOf(hash.get("fp")),
              rtc: true,
              reach: (ws) => ws,
              rtcMake: (inv, o) => rtcTransport(inv, { video: true, log, ...o }),
              socket: (u) => new WebSocket(u) as unknown as JoinSocket,
            }),
          { room: page, log },
        );
  root.render(
    <JoinFlow url={url} invite={invite} host={host} path={hostPath(location.hostname)} transport={transport} internet={false} />,
  );
}
