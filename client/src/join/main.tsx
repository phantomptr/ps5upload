// The page a friend's browser opens from the host's Play Together QR code or link (#641), served
// by the host's engine at /join (built by vite.join.config.ts into join_page.html). The invite
// key is in the address's #fragment, so it never reaches a server log; the page gives it to the
// socket in its hello.
import { createRoot } from "react-dom/client";
import "../index.css";
import { JoinFlow } from "./JoinFlow";

const hash = new URLSearchParams(location.hash.slice(1));
const invite = hash.get("i") || null;
const host = hash.get("n")?.trim() || null;
// The engine may be served under a path (PS5UPLOAD_BASE_URL): the socket is beside this page.
const prefix = location.pathname.replace(/\/join\/?$/, "");
const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${prefix}/api/join/ws`;

createRoot(document.getElementById("root")!).render(<JoinFlow url={url} invite={invite} host={host} />);
