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

export { joinAppLink };

/** A computer's phone controller link (`http://<computer>:<port>/pad#t=<key>[&w=1]`). */
export interface PadLink {
  /** `<host>:<port>` as written (an IPv6 host in brackets). */
  authority: string;
  /** The host alone, to name the computer. */
  host: string;
  key: string;
  watch: boolean;
}

/** The controller link, or null when `text` is not one. */
export function parsePadLink(text: string): PadLink | null {
  let u: URL;
  try {
    u = new URL(text.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  if (!/^\/pad\/?$/.test(u.pathname) || u.username || u.password) return null;
  const hash = new URLSearchParams(u.hash.replace(/^#/, ""));
  const key = hash.get("t") ?? "";
  if (!/^[0-9a-fA-F]{8,128}$/.test(key)) return null;
  return { authority: u.host, host: u.hostname.replace(/^\[|\]$/g, ""), key, watch: hash.get("w") === "1" };
}

/** "Open in the PS5Upload app" for a controller (or watch) link. */
export function padAppLink(link: string): string {
  return `ps5upload://pad?link=${encodeURIComponent(link)}`;
}

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
    url: `ws://${pad.authority}/api/pad/ws`,
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
