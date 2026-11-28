// Play Rooms on the guest page: a room handed over from the website's Rooms page or a friend's
// lookup, in the address's fragment (never sent to a server):
//   /join/#lobby=<hub lobby id>&hub=<hub host>&n=<host name>&tag=<host tag>&pw=1&via=rooms
// The same page then runs the same join as an invite: name → password if any → waiting → playing.

import type { RoomJoin } from "../lib/joinProtocol";

export interface RoomFragment {
  lobby: string;
  hub: string;
  host: string | null;
  tag: string;
  pw: boolean;
  via: RoomJoin["via"];
}

const VIAS: ReadonlyArray<RoomJoin["via"]> = ["rooms", "friends", "nearby", "again"];

/** The room in a fragment, or null when it isn't one (or names a hub that could be anywhere). */
export function parseRoomFragment(hash: string): RoomFragment | null {
  const p = new URLSearchParams(hash.replace(/^#/, ""));
  const lobby = p.get("lobby") ?? "";
  const hub = p.get("hub") ?? "";
  if (!/^[A-Za-z0-9_-]{16,32}$/.test(lobby)) return null;
  // A host[:port] only (like an invite's `hub`): no scheme, path, user or query.
  if (!/^(\[[0-9a-fA-F:]+\]|[A-Za-z0-9.-]+)(:\d{1,5})?$/.test(hub)) return null;
  const via = (VIAS as readonly string[]).includes(p.get("via") ?? "") ? (p.get("via") as RoomJoin["via"]) : "rooms";
  const tag = (p.get("tag") ?? "").replace(/[^0-9A-Fa-f]/g, "").slice(0, 8).toUpperCase();
  const host = (p.get("n") ?? "").trim().slice(0, 32) || null;
  return { lobby, hub, host, tag, pw: p.get("pw") === "1", via };
}

/** The fragment for a room (the website's Rooms page links to the join page with it). */
export function roomFragment(r: RoomFragment): string {
  const p = new URLSearchParams({ lobby: r.lobby, hub: r.hub, via: r.via });
  if (r.host) p.set("n", r.host);
  if (r.tag) p.set("tag", r.tag);
  if (r.pw) p.set("pw", "1");
  return `#${p.toString()}`;
}
