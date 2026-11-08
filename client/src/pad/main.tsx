// The controller page a phone's browser opens from the computer's QR code (served by the engine
// at /pad, built by vite.pad.config.ts). The key is in the address's #fragment, so it never
// reaches a server log; the page gives it to the socket.
//
// The same page is on the website at /pad/ (site/pad/index.html) for the phone link's internet
// link: `#room=…&k=…&fp=…&hub=…` (`&w=1` to watch). Then the phone reaches the computer's engine
// peer to peer over WebRTC, introduced by the signalling hub (join/rtcTransport with a "pad"
// channel), and says in its first message what the LAN socket's address carries; nothing goes to
// this page's own origin.
import { createRoot } from "react-dom/client";
import "../index.css";
import { PadApp } from "./PadApp";
import { OpenInAppGate } from "../components/OpenInAppGate";
import { padDeviceId } from "../lib/padClient";
import { padAppLink } from "../lib/padLink";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { hubSocketUrl, parseInternetFragment, rtcTransport } from "../join/rtcTransport";
import { deviceWord } from "./deviceName";

const hash = new URLSearchParams(location.hash.slice(1));
const key = hash.get("t") ?? "";
// Watch party: the computer's watch link adds w=1.
const watch = hash.get("w") === "1";
const name = deviceWord(navigator.userAgent, navigator.maxTouchPoints);
const device = padDeviceId({ get: safeGetItem, set: safeSetItem });
const net = parseInternetFragment(location.hash);
const root = createRoot(document.getElementById("root")!);

if (net) {
  // An internet link plays in the browser: no "Open in the app?" question.
  const transport = rtcTransport(net, { label: "pad", log: (m) => console.info(`[pad] ${m}`) });
  const hello: Record<string, string | number> = { t: net.key, name, d: device, ...(watch ? { w: 1 } : {}) };
  root.render(<PadApp url={hubSocketUrl(net.hub)} computer={null} watch={watch} transport={transport} hello={hello} />);
} else {
  // The engine may be served under a path (PS5UPLOAD_BASE_URL): the socket is beside this page.
  const prefix = location.pathname.replace(/\/pad\/?$/, "");
  const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${prefix}/api/pad/ws?t=${encodeURIComponent(key)}&name=${name}&d=${device}${watch ? "&w=1" : ""}`;

  // A link opened in a browser: offer the PS5Upload app first (Slack-style).
  root.render(
    <OpenInAppGate appLink={padAppLink(location.href)}>
      <PadApp url={url} computer={location.hostname} watch={watch} />
    </OpenInAppGate>,
  );
}
