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
import { hubSocketUrl, parseInternetFragment, rtcTransport, type JoinSocket } from "./rtcTransport";
import { lanPage, lanTarget } from "./lanHint";
import { hostPath } from "./netPath";
import { joinTries, raceTransport } from "./pathRace";
import { fpOf } from "../lib/joinProtocol";

const hash = new URLSearchParams(location.hash.slice(1));
const host = hash.get("n")?.trim() || null;
// An internet invite (the same page at https://ps5upload.phantomptr.com/join/): peer to peer over
// WebRTC, introduced by the hub; nothing goes to this page's own origin. On the host's Wi-Fi the
// connection stays on it (ICE), and a link from a newer host names its page there (`lan`) for
// when the hub can't be reached ("Open on this Wi-Fi").
const net = parseInternetFragment(location.hash);
const root = createRoot(document.getElementById("root")!);
if (net) {
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
