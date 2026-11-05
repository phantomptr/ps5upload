// ps5upload:// links: a link a browser page hands to the PS5Upload app, where it is installed.
//
//   ps5upload://join?link=<Play Together invite>   → Play › Join a friend, the invite filled in
//   ps5upload://pad?link=<phone controller link>   → this device as the controller (or, with the
//                                                      link's w=1, watching)
//
// The page (join/JoinFlow, pad/PadApp's page) offers it; the shell hands it to the app
// (open_links.rs), and components/OpenLinks routes it. Everything a link carries is checked
// again here: only a well-formed invite or controller link of a computer leads anywhere.

import { joinAppLink, joinLinkFromAppLink } from "./joinProtocol";
import { padAppLink, parsePadLink, type PadLink } from "./padLink";

export { joinAppLink, padAppLink, parsePadLink, type PadLink };

/** What a ps5upload:// link asks for, or null for anything else. */
export function readAppLink(url: string): { kind: "join"; link: string } | { kind: "pad"; pad: PadLink } | null {
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

/** Where an opened ps5upload:// link leads in the app, or null for one it doesn't know. */
export function appLinkRoute(url: string): string | null {
  const a = readAppLink(url);
  if (!a) return null;
  return a.kind === "join" ? `/play?join=${encodeURIComponent(a.link)}` : padRoute(a.pad);
}
