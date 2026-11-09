// ps5upload:// links: a link a browser page hands to the PS5Upload app, where it is installed.
//
//   ps5upload://join?link=<Play Together invite>   → Play › Join a friend, the invite filled in
//   ps5upload://pad?link=<phone controller link>   → this device as the controller (or, with the
//                                                      link's w=1, watching)
//   ps5upload://receive?link=<a friend's Share link> → Share, the link filled in
//
// The page (join/JoinFlow, pad/PadApp's page) offers it; the shell hands it to the app
// (open_links.rs), and components/OpenLinks routes it. Everything a link carries is checked
// again here: only a well-formed invite or controller link of a computer leads anywhere.

import { joinAppLink, joinLinkFromAppLink } from "./joinProtocol";
import { padAppLink, parsePadLink, type PadLink } from "./padLink";
import { isShareLink } from "../api/share";

export { joinAppLink, padAppLink, parsePadLink, type PadLink };

/** What a ps5upload:// link asks for, or null for anything else. */
export function readAppLink(
  url: string,
): { kind: "join"; link: string } | { kind: "pad"; pad: PadLink } | { kind: "receive"; link: string } | null {
  const join = joinLinkFromAppLink(url);
  if (join) return { kind: "join", link: join };
  let u: URL;
  try {
    u = new URL(url.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "ps5upload:") return null;
  const what = (u.host || u.pathname.replace(/^\/+/, "")).replace(/\/+$/, "").toLowerCase();
  if (what === "receive") {
    const link = u.searchParams.get("link") ?? "";
    return isShareLink(link) ? { kind: "receive", link: link.trim() } : null;
  }
  if (what !== "pad") return null;
  const pad = parsePadLink(u.searchParams.get("link") ?? "");
  return pad ? { kind: "pad", pad } : null;
}

/** The in-app screen for a controller link: /play/pad, through this app's engine relay. */
export function padRoute(pad: PadLink): string {
  const q = new URLSearchParams({
    url: `ws://${pad.authority}${pad.prefix ?? ""}/api/pad/ws`,
    name: pad.host,
    t: pad.key,
  });
  if (pad.watch) q.set("w", "1");
  return `/play/pad?${q}`;
}

/** Where an opened ps5upload:// link leads in the app, or null for one it doesn't know.
 *  ps5upload://console?… (an invite to a console, screens/Connection/consoles/model.ts) opens
 *  Consoles with the Add sheet showing it; the page checks the invite again. */
export function appLinkRoute(url: string): string | null {
  if (/^\s*ps5upload:\/\/console[/?]/i.test(url)) return `/connection?invite=${encodeURIComponent(url.trim())}`;
  const a = readAppLink(url);
  if (!a) return null;
  if (a.kind === "join") return `/play?join=${encodeURIComponent(a.link)}`;
  if (a.kind === "receive") return `/share?receive=${encodeURIComponent(a.link)}`;
  return padRoute(a.pad);
}

/** The app link a Share link's web page offers, to open it in PS5Upload. */
export function receiveAppLink(shareLink: string): string {
  return `ps5upload://receive?link=${encodeURIComponent(shareLink)}`;
}
