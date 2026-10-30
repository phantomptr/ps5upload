// The controller page a phone's browser opens from the computer's QR code (served by the engine
// at /pad, built by vite.pad.config.ts). The key is in the address's #fragment, so it never
// reaches a server log; the page gives it to the socket.
import { createRoot } from "react-dom/client";
import "../index.css";
import { PadApp } from "./PadApp";

const hash = new URLSearchParams(location.hash.slice(1));
const key = hash.get("t") ?? "";
// Watch party: the computer's watch link adds w=1.
const watch = hash.get("w") === "1";
const name = /iPhone|iPad/.test(navigator.userAgent)
  ? "iPhone"
  : /Android/.test(navigator.userAgent)
    ? "Android phone"
    : "Phone";
const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/pad/ws?t=${encodeURIComponent(key)}&name=${encodeURIComponent(name)}${watch ? "&w=1" : ""}`;

createRoot(document.getElementById("root")!).render(<PadApp url={url} computer={location.hostname} watch={watch} />);
