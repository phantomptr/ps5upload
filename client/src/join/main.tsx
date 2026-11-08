// The page a friend's browser opens from the host's Play Together QR code or link (#641), served
// by the host's engine at /join and, for internet invites, by the website at /join/ (built by
// vite.join.config.ts into join_page.html and site/join/index.html). The invite key is in the
// address's #fragment, so it never reaches a server log; the page gives it to the socket in its
// hello.
import { createRoot } from "react-dom/client";
import "../index.css";
import { JoinFlow } from "./JoinFlow";
import { hubSocketUrl, parseInternetFragment, rtcTransport } from "./rtcTransport";
import { lanPage, lanTarget } from "./lanHint";

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
  root.render(<JoinFlow url={url} invite={invite} host={host} />);
}
