// Play Together (#641), the host's side: what the engine says about the friends playing along
// (GET /api/join?addr=<console>) and the calls that change it. Kept apart from the panel so the
// rules (who counts, when to advise a lower quality) are tested without React.

import { useEffect, useState } from "react";

import type { MeshJoin } from "../../api/mesh";
import { HIGH_DELAY_MS, type GuestState } from "../../lib/joinProtocol";
import { getEngineUrl } from "../../state/engine";

export interface JoinGuest {
  id: number;
  name: string;
  player: number;
  status: GuestState;
  /** Seconds left of a dropped friend's 60 s window. */
  reconnectingLeftS: number | null;
  rttMs: number | null;
  connected: boolean;
  /** Pictures were skipped in the last few seconds: their connection can't keep up. */
  behind: boolean;
  /** On this stream's own controller (Player 1): plays as the host's PS5 user. */
  streamSeat: boolean;
  /** How they reached this computer: the home network, or an internet invite (WebRTC). */
  via: "lan" | "internet";
  /** An internet guest's connection: "direct", "connecting", "failed", "closed" (null on the LAN). */
  path: string | null;
  /** How they are connected, from the engine's `link.path`: "lan" (the home network),
   *  "tailscale", "internet" (direct through both routers); null when the engine doesn't say
   *  (an older one, or ICE has no pair yet). */
  netPath: "lan" | "tailscale" | "internet" | null;
  /** Seated in the PS5 profile the host chose for their player ("Guests play as"): its name. */
  playsAs: string | null;
  /** A profile was chosen for their player but couldn't be used, so the TV asks who they are:
   *  why ("signed_out", "owner", "starting", "unavailable", "failed", "old_helper", "unknown"). */
  seatNote: string | null;
  /** How the picture looks on their side, from their page's own report: "smooth", "stutters",
   *  "poor", or null (no report yet, or an older engine or page). */
  quality: "smooth" | "stutters" | "poor" | null;
  /** An internet guest's picture on its video track whose bandwidth estimate stays under the
   *  stream's bitrate (kbit/s both): the host should lower the stream's quality, since the console
   *  can't change it mid-stream. Null otherwise (the data channel, an older engine). */
  carries: { kbps: number; streamKbps: number } | null;
  /** The connection check of a guest on the video track (engine join_net TrackSt): the estimate
   *  (str0m's TWCC, probing over the rate at the start), the stream's rate, what it carries with
   *  20% to spare, how long it has stayed under the rate, the loss and RTCP round trip; null off
   *  the track (the data channel, the plain socket, an older engine). */
  link: GuestLink | null;
}

export interface GuestLink {
  measuring: boolean;
  kbps: number | null;
  streamKbps: number | null;
  fitKbps: number | null;
  underS: number;
  /** 0-1. */
  loss: number | null;
  rttMs: number | null;
}

/** A PS5 profile signed in on the console (GET /api/join/profiles). */
export interface ConsoleProfile {
  id: number;
  name: string;
  /** The one in front on the TV: the host's own. */
  foreground: boolean;
}

/** "Guests play as": the profile one guest's player gets on this console. */
export interface SeatProfile {
  /** 2-4. */
  player: number;
  user: number;
  name: string;
  /** The host confirmed it although it is their own profile (saves and trophies). */
  owner: boolean;
}

/** What Settings › Play Together shows for "Guests play as". */
export interface ProfilesState {
  users: ConsoleProfile[];
  seats: SeatProfile[];
  /** The helper's companion that seats guests: "running", "starting", "no_bind", "unavailable",
   *  "off", "old_helper", "unreachable"; null when not asked yet (Play Together closed). */
  companion: string | null;
}

/** An internet guest's connection the engine knows of, in the game or not (yet). */
export interface NetPeer {
  gid: string;
  /** "connecting", "direct", "failed" or "closed". */
  path: string;
  rttMs: number | null;
}

/** The internet half of an invite (POST /api/join/invite with `internet`). */
export interface NetInvite {
  /** The link to share (null until the engine has its certificate). */
  url: string | null;
  /** The engine's link to the signalling hub: "connecting", "hosted", "reconnecting", "error",
   *  "closed". */
  hubState: string | null;
  /** Why the hub can't be reached, in the engine's words. */
  hubError: string | null;
  /** The engine can't reach the hub (hub.rs Room::offline): the invite is the Wi-Fi's for now. */
  offline: boolean;
}

/** The one link to share (POST /api/join/invite `share`): where it works, and why it is the
 *  Wi-Fi's ("setting": Invites work says Only on my Wi-Fi; "offline": no internet here, or the
 *  invite service is down; "local": none was asked for). */
export interface ShareLink {
  url: string | null;
  works: "anywhere" | "wifi";
  why: string | null;
}

/** The host's Play Together choices for internet invites (every console). */
export interface NetSettings {
  /** Internet invites allowed; null: an engine from before internet invites (none offered). */
  internetInvites: boolean | null;
  /** Settings › App › Play Together › Invites work: "anywhere" (the one link: the internet
   *  invite, which works on the Wi-Fi too) or "wifi"; null like internetInvites. An engine from
   *  before the word says it with internet_invites. */
  invites: "anywhere" | "wifi" | null;
  hubUrl: string;
  stun: string[];
  upnp: boolean;
  /** Internet friends' picture as a video track (the data channel when off, or for a page or
   *  engine without it). */
  videoTrack: boolean;
}

export interface JoinHostState {
  open: boolean;
  invite: {
    url: string;
    code: string;
    expiresMs: number | null;
    urls: string[];
    /** This computer's Tailscale address (one of `urls`), while Tailscale is connected. */
    tailscale: string | null;
    /** The internet invite, when the host made one. */
    internet: NetInvite | null;
    /** The one link (null: an engine from before it, see `shareOf`). */
    share: ShareLink | null;
  } | null;
  guests: JoinGuest[];
  asks: Array<{ id: number; name: string }>;
  settings: { blockSystemButtons: boolean; maxPlayers: number; streamSeat: boolean } & NetSettings;
  /** Internet guests' connections, those not in the game yet (connecting, failed) too. */
  netPeers: NetPeer[];
  /** The next friend allowed in gets this stream's controller (Player 1, as the host's PS5 user). */
  seatNext: boolean;
  /** The next friend allowed in plays as this profile (the host's choice for their player). */
  nextProfile: { player: number; name: string } | null;
  helperOk: boolean;
  reason: string | null;
  /** The guest socket friends connect to (`ws`, else made from the invite's address). */
  ws: string | null;
}

export const EMPTY_JOIN: JoinHostState = {
  open: false,
  invite: null,
  guests: [],
  asks: [],
  settings: { blockSystemButtons: true, maxPlayers: 3, streamSeat: false, internetInvites: null, invites: null, hubUrl: "", stun: [], upnp: true, videoTrack: true },
  netPeers: [],
  seatNext: false,
  nextProfile: null,
  helperOk: true,
  reason: null,
  ws: null,
};

const STATES: ReadonlySet<string> = new Set(["asking", "picking_user", "playing", "reconnecting", "left"]);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
/** A guest's state; an unknown one counts as in the game (never hides a friend). */
const stateOf = (v: unknown): GuestState => (typeof v === "string" && STATES.has(v) ? v : "playing") as GuestState;
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

/** An invite's internet half (`invite.internet`), or null. */
export function readNetInvite(v: unknown): NetInvite | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  return { url: str(o.url), hubState: str(o.state), hubError: str(o.error), offline: o.offline === true };
}

/** The invite's one link (`invite.share`), or null. */
export function readShare(v: unknown): ShareLink | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  return { url: str(o.url), works: o.works === "anywhere" ? "anywhere" : "wifi", why: str(o.why) };
}

/** How a guest is connected (`link.path`), or null. */
function readNetPath(link: unknown): JoinGuest["netPath"] {
  const p = (link as { path?: unknown } | null | undefined)?.path;
  return p === "lan" || p === "tailscale" || p === "internet" ? p : null;
}

/** The link to show: the engine's one link, else (an engine from before it) the internet link
 *  when there is one, else the home network's. */
export function shareOf(invite: NonNullable<JoinHostState["invite"]>): ShareLink {
  if (invite.share) return invite.share;
  if (invite.internet?.url) return { url: invite.internet.url, works: "anywhere", why: null };
  return { url: invite.url, works: "wifi", why: "local" };
}

/** The engine's internet peers (`internet.peers`, or the invite's own list). */
export function readPeers(v: unknown): NetPeer[] {
  return (Array.isArray(v) ? v : [])
    .filter((p): p is Record<string, unknown> => !!p && typeof p === "object" && typeof p.gid === "string")
    .map((p) => ({ gid: p.gid as string, path: str(p.path) ?? "connecting", rttMs: num(p.rtt_ms) }));
}

/** A guest's picture in a word, as the engine says it (anything else: unknown). */
function readQuality(v: unknown): JoinGuest["quality"] {
  return v === "smooth" || v === "stutters" || v === "poor" ? v : null;
}

/** What an internet guest's video track carries when it can't carry the stream (`link.track`
 *  with `slow`), or null. */
export function readCarries(link: unknown): JoinGuest["carries"] {
  const t = (link as { track?: Record<string, unknown> | null } | null | undefined)?.track;
  const kbps = num(t?.bwe_kbps);
  const streamKbps = num(t?.stream_kbps);
  return t?.slow === true && kbps != null && streamKbps != null ? { kbps, streamKbps } : null;
}

/** The connection check off `link.track`, or null when the guest has no track. */
export function readLink(link: unknown): GuestLink | null {
  const t = (link as { track?: Record<string, unknown> | null } | null | undefined)?.track;
  if (!t || typeof t !== "object") return null;
  return {
    measuring: t.measuring === true,
    kbps: num(t.bwe_kbps),
    streamKbps: num(t.stream_kbps),
    fitKbps: num(t.fit_kbps),
    underS: num(t.under_s) ?? 0,
    loss: num(t.loss),
    rttMs: num(t.rtcp_rtt_ms),
  };
}

export type LinkGrade = "measuring" | "good" | "ok" | "slow";

/** A guest's connection in a word for the host's panel, with about how many Mbit/s it carries:
 *  "slow" under the stream's rate (or losing 5%, or a 150 ms round trip), "ok" without 20% to
 *  spare (or 2% loss, or 80 ms), else "good"; "measuring" while the check runs. Null with no
 *  check (off the track). */
export function linkGrade(l: GuestLink | null): { grade: LinkGrade; mbps: number | null } | null {
  if (!l) return null;
  const mbps = l.kbps == null ? null : l.kbps >= 10_000 ? Math.round(l.kbps / 1000) : Math.round(l.kbps / 100) / 10;
  if (l.measuring || l.kbps == null) return { grade: "measuring", mbps: null };
  const loss = l.loss ?? 0;
  const rtt = l.rttMs ?? 0;
  const need = l.streamKbps ?? 0;
  if (l.underS > 0 || (need > 0 && l.kbps < need * 0.9) || loss >= 0.05 || rtt >= 150) return { grade: "slow", mbps };
  if ((need > 0 && l.kbps < need * 1.2) || loss >= 0.02 || rtt >= 80) return { grade: "ok", mbps };
  return { grade: "good", mbps };
}

/** How long a guest's estimate stays under the stream's rate before a lower quality is offered. */
export const SUGGEST_AFTER_S = 10;

/** A lower bitrate that fits every guest: offered when a playing guest's estimate has stayed under
 *  the stream's rate for SUGGEST_AFTER_S, the highest of `rates` (Settings › Picture's, kbit/s)
 *  under the slowest such guest's estimate with 20% to spare and under the rate now; the lowest
 *  one when nothing fits. Null when nobody needs it (or nothing lower exists). Only offered,
 *  never applied: every guest shares the PS5's one stream, and a new rate restarts it. */
export function lowerForAll(
  guests: readonly JoinGuest[],
  rates: readonly number[],
): { guest: JoinGuest; gotKbps: number; toKbps: number } | null {
  const slow = guests
    .filter((g) => g.status === "playing" && g.link && g.link.underS >= SUGGEST_AFTER_S && g.link.fitKbps != null)
    .sort((a, b) => (a.link!.fitKbps ?? 0) - (b.link!.fitKbps ?? 0));
  const g = slow[0];
  if (!g?.link) return null;
  const now = g.link.streamKbps ?? Infinity;
  const lower = rates.filter((r) => r > 0 && r < now).sort((a, b) => b - a);
  if (!lower.length) return null;
  const to = lower.find((r) => r <= g.link!.fitKbps!) ?? lower[lower.length - 1];
  return { guest: g, gotKbps: g.link.kbps ?? g.link.fitKbps!, toKbps: to };
}

/** The engine's answer, checked field by field (an older or odd one never breaks the page). */
export function readJoinState(v: unknown): JoinHostState {
  const o = (v ?? {}) as Record<string, unknown>;
  const inv = o.invite as Record<string, unknown> | null | undefined;
  const invite =
    inv && typeof inv === "object" && typeof inv.url === "string"
      ? {
          url: inv.url,
          code: typeof inv.code === "string" ? inv.code : "",
          // The engine says seconds left (`expires_in_s`).
          expiresMs: num(inv.expires_in_s) != null ? num(inv.expires_in_s)! * 1000 : num(inv.expires_ms),
          // Every address of this computer a friend may reach, best first (multi-network).
          urls: Array.isArray(inv.urls) ? inv.urls.filter((u): u is string => typeof u === "string") : [],
          tailscale: typeof inv.tailscale === "string" ? inv.tailscale : null,
          internet: readNetInvite(inv.internet),
          share: readShare(inv.share),
        }
      : null;
  const guests = (Array.isArray(o.guests) ? o.guests : [])
    .filter((g): g is Record<string, unknown> => !!g && typeof g === "object")
    .filter((g) => typeof g.id === "number" || typeof g.id === "string")
    .map((g) => ({
      id: Number(g.id),
      // Empty: they gave no name, the panel says "Friend" in this app's language.
      name: typeof g.name === "string" ? g.name : "",
      player: num(g.player) ?? 0,
      // The engine's field is `state` (`status` kept for an older one).
      status: stateOf(g.state ?? g.status),
      reconnectingLeftS: num(g.reconnect_left_s) ?? num(g.reconnecting_left_s),
      // An internet guest's own round trip, else ICE's.
      rttMs: num(g.rtt_ms) ?? num(g.ice_rtt_ms),
      connected: g.connected !== false,
      behind: g.behind === true,
      // An older engine says nothing: Player 1 was the stream's controller there.
      streamSeat: typeof g.stream_seat === "boolean" ? g.stream_seat : num(g.player) === 1,
      // An older engine says nothing: the home network.
      via: g.via === "internet" ? ("internet" as const) : ("lan" as const),
      path: str(g.path),
      netPath: readNetPath(g.link),
      // An older engine says nothing: the TV asks every guest.
      playsAs: str((g.plays_as as Record<string, unknown> | null | undefined)?.name),
      seatNote: str(g.seat_note),
      quality: readQuality(g.quality),
      carries: readCarries(g.link),
      link: readLink(g.link),
    }))
    .sort((a, b) => a.player - b.player);
  // Friends asking to join: the engine's `pending` (`asks` kept for an older one).
  const asks = (Array.isArray(o.pending) ? o.pending : Array.isArray(o.asks) ? o.asks : [])
    .filter((a): a is { id: number; name: string } => !!a && typeof a.id === "number")
    .map((a) => ({ id: a.id, name: typeof a.name === "string" ? a.name : "" }));
  const s = (o.settings ?? {}) as Record<string, unknown>;
  return {
    open: o.open === true,
    invite,
    guests,
    asks,
    settings: {
      blockSystemButtons: s.block_system_buttons === true,
      maxPlayers: num(s.max_players) ?? 3,
      streamSeat: s.stream_seat === true,
      internetInvites: typeof s.internet_invites === "boolean" ? s.internet_invites : null,
      invites:
        s.invites === "anywhere" || s.invites === "wifi"
          ? s.invites
          : typeof s.internet_invites === "boolean"
            ? s.internet_invites
              ? "anywhere"
              : "wifi"
            : null,
      hubUrl: typeof s.hub_url === "string" ? s.hub_url : "",
      stun: Array.isArray(s.stun) ? s.stun.filter((x): x is string => typeof x === "string") : [],
      upnp: s.upnp !== false,
      videoTrack: s.video_track !== false,
    },
    netPeers: readPeers((o.internet as Record<string, unknown> | null | undefined)?.peers ?? (inv?.internet as Record<string, unknown> | null | undefined)?.peers),
    seatNext: o.seat_next === true,
    nextProfile: readNextProfile(o.next_profile),
    helperOk: o.helper_ok !== false,
    reason: typeof o.reason === "string" && o.reason ? o.reason : null,
    ws: typeof o.ws === "string" && /^wss?:\/\//.test(o.ws) ? o.ws : invite ? socketOf(invite.url) : null,
  };
}

/** The profile the next friend plays as (`next_profile`), or null. */
function readNextProfile(v: unknown): { player: number; name: string } | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const name = str(o.name);
  const player = num(o.player);
  return name && player ? { player, name } : null;
}

/** The ids the engine writes (unsigned 32-bit); anything else is no profile. */
const userId = (v: unknown): number | null => {
  const n = num(v);
  return n != null && Number.isInteger(n) && n > 0 && n < 0xffffffff ? n : null;
};

/** GET /api/join/profiles, checked field by field. */
export function readProfiles(v: unknown): ProfilesState {
  const o = (v ?? {}) as Record<string, unknown>;
  const users = (Array.isArray(o.users) ? o.users : [])
    .filter((u): u is Record<string, unknown> => !!u && typeof u === "object" && userId(u.id) != null)
    .map((u) => ({ id: userId(u.id)!, name: typeof u.name === "string" ? u.name : "", foreground: u.foreground === true }));
  const seats = (Array.isArray(o.seats) ? o.seats : [])
    .filter((s): s is Record<string, unknown> => !!s && typeof s === "object" && userId(s.user) != null)
    .map((s) => ({
      player: num(s.player) ?? 0,
      user: userId(s.user)!,
      name: typeof s.name === "string" ? s.name : "",
      owner: s.owner === true,
    }))
    .filter((s) => s.player >= 2 && s.player <= 4);
  return { users, seats, companion: str(o.companion) };
}

/** The seats with `player` set to `user` (null: the TV asks), as POST /api/join/settings takes
 *  them. A profile is on one player at most: choosing it for another moves it. */
export function withSeat(
  seats: SeatProfile[],
  player: number,
  user: ConsoleProfile | null,
  owner: boolean,
): SeatProfile[] {
  const rest = seats.filter((s) => s.player !== player && (!user || s.user !== user.id));
  const next = user ? [...rest, { player, user: user.id, name: user.name, owner }] : rest;
  return next.sort((a, b) => a.player - b.player);
}

/** The guest socket of an invite link's computer. */
function socketOf(url: string): string | null {
  try {
    const u = new URL(url);
    const prefix = u.pathname.replace(/\/join\/?$/, "");
    return `${u.protocol === "https:" ? "wss:" : "ws:"}//${u.host}${prefix}/api/join/ws`;
  } catch {
    return null;
  }
}

/** What this app tells the other apps on the network while Play Together is open (mesh
 *  presence, useMeshSync), or null when it is closed. */
export function presenceJoin(s: JoinHostState, hostName: string, consoleName: string, game: string | null = null): MeshJoin | null {
  if (!s.open || !s.ws || !s.helperOk) return null;
  return {
    code: s.invite?.code ?? "",
    players: 1 + friendsIn(s).length,
    max: 1 + Math.max(1, Math.min(3, s.settings.maxPlayers)),
    host_name: hostName,
    console_name: consoleName,
    ...(game ? { game_title: game } : {}),
    ws: s.ws,
  };
}

/** The socket this app publishes (its own engine): never listed as "playing nearby". */
let ownWs: string | null = null;
export const ownJoinWs = () => ownWs;
export const setOwnJoinWs = (ws: string | null) => {
  ownWs = ws;
};

/** Friends who are in the game (or keeping their place): what stopping Play would end. */
export function friendsIn(s: JoinHostState): JoinGuest[] {
  return s.guests.filter((g) => g.status !== "left" && g.status !== "asking");
}

/** The friend whose delay most needs the host's attention, or null. */
export function slowFriend(s: JoinHostState): JoinGuest | null {
  let worst: JoinGuest | null = null;
  for (const g of s.guests) {
    if (g.status !== "playing" && g.status !== "picking_user") continue;
    if (g.rttMs == null || g.rttMs < HIGH_DELAY_MS) continue;
    if (!worst || g.rttMs > (worst.rttMs ?? 0)) worst = g;
  }
  return worst;
}

/** The friend on the stream's own controller (Player 1, with the picture off here), or null. */
export function streamSeatFriend(s: JoinHostState): JoinGuest | null {
  return s.guests.find((g) => g.player === 1 && g.status !== "left") ?? null;
}

/** The places, with the friend in each (or null: free): P2–P4, and P1 first while a friend is on
 *  the stream's own controller. Never more than the friends allowed. */
export function playerSlots(s: JoinHostState): Array<{ player: number; guest: JoinGuest | null }> {
  const n = Math.max(1, Math.min(3, s.settings.maxPlayers));
  const onStream = streamSeatFriend(s);
  const first = onStream ? 1 : 2;
  return Array.from({ length: n }, (_, i) => {
    const player = first + i;
    const here = s.guests.filter((g) => g.player === player);
    // A friend who left shows until another takes the place.
    return { player, guest: here.find((g) => g.status !== "left") ?? here[0] ?? null };
  });
}

/** The address an invite link goes to with another of this computer's addresses. */
export function inviteOn(url: string, address: string): string {
  try {
    const u = new URL(url);
    const a = new URL(address.includes("://") ? address : `http://${address}`);
    u.hostname = a.hostname;
    if (a.port) u.port = a.port;
    return u.toString();
  } catch {
    return url;
  }
}

/** A call's answer: the state, or the engine's reason it said no (`old_helper`, `not_playing`…).
 *  `found`: what the engine found instead, for a bug report (`not_playing`: the console asked for
 *  and the one it is streaming). */
export type JoinReply =
  | { state: JoinHostState; error?: undefined }
  | { state?: undefined; error: string; found?: string };

/** What a `not_playing` refusal found (join.rs not_playing_json), as one line for the panel's
 *  failure text and a bug report; undefined from an engine that doesn't say. Not translated: it is
 *  read by whoever looks into the report. */
export function foundOf(json: unknown): string | undefined {
  const j = json as { asked?: unknown; streaming?: unknown; stopping?: unknown } | null;
  if (typeof j?.asked !== "string") return undefined;
  const streaming = typeof j.streaming === "string" && j.streaming ? j.streaming : "none";
  return `asked ${j.asked}, streaming ${streaming}${j.stopping === true ? " (stopping)" : ""}`;
}

async function request(path: string, body?: object): Promise<{ ok: boolean; json: unknown }> {
  const r = await fetch(`${getEngineUrl()}${path}`, {
    method: body ? "POST" : "GET",
    headers: body ? { "Content-Type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await r.text();
  let json: unknown;
  try {
    json = text ? JSON.parse(text) : {};
  } catch {
    json = {};
  }
  return { ok: r.ok, json };
}

const errorOf = (json: unknown): string => {
  const e = (json as { error?: unknown } | null)?.error;
  return typeof e === "string" && e ? e : "engine";
};

/** GET /api/join: the whole state. */
async function read(addr: string): Promise<JoinHostState | null> {
  try {
    const r = await request(`/api/join?addr=${encodeURIComponent(addr)}`);
    return r.ok ? readJoinState(r.json) : null;
  } catch {
    return null;
  }
}

/** open and close answer with the whole state. */
async function whole(path: string, addr: string): Promise<JoinReply> {
  try {
    const r = await request(path, { addr });
    if (r.ok) return { state: readJoinState(r.json) };
    const found = foundOf(r.json);
    return found ? { error: errorOf(r.json), found } : { error: errorOf(r.json) };
  } catch {
    return { error: "engine" };
  }
}

/** The other calls answer with just their part (`{ok}`, the invite, the settings): the whole
 *  state is read after, so the panel never flashes an empty one. */
async function change(path: string, addr: string, body: object): Promise<JoinHostState | null> {
  try {
    const r = await request(path, { addr, ...body });
    if (!r.ok) return null;
  } catch {
    return null;
  }
  return read(addr);
}

/** The body of an invite call: `new` only for a fresh link, `internet` only for an internet one,
 *  `share` for the one link (the engine follows Invites work). */
export function inviteBody(renew: boolean, internet = false, share = false): { new?: true; internet?: true; share?: true } {
  return {
    ...(renew ? { new: true as const } : {}),
    ...(internet ? { internet: true as const } : {}),
    ...(share ? { share: true as const } : {}),
  };
}

/** Like `change`, but a refusal says why (`internet_off`, `hub_url`, `stun`…). */
async function changeOrWhy(path: string, addr: string, body: object): Promise<JoinReply> {
  try {
    const r = await request(path, { addr, ...body });
    if (!r.ok) return { error: errorOf(r.json) };
  } catch {
    return { error: "engine" };
  }
  const s = await read(addr);
  return s ? { state: s } : { error: "engine" };
}

/** The settings of internet invites, as POST /api/join/settings takes them. */
export interface NetSettingsBody {
  /** Invites work: "anywhere" or "wifi" (sent with internet_invites, which an engine from before
   *  the word reads). */
  invites?: "anywhere" | "wifi";
  internet_invites?: boolean;
  hub_url?: string;
  stun?: string[];
  upnp?: boolean;
  video_track?: boolean;
}

/** Invites work, as POST /api/join/settings takes it: the word, and the older on/off an engine
 *  from before the word reads (Allow internet invites off is Only on my Wi-Fi). */
export function invitesBody(v: "anywhere" | "wifi"): NetSettingsBody {
  return { invites: v, internet_invites: v === "anywhere" };
}

/** A hub address the engine takes: `wss://`, or `ws://` only to this computer (join_net::hub_url_ok). */
export function hubUrlOk(url: string): boolean {
  let u: URL;
  try {
    u = new URL(url.trim());
  } catch {
    return false;
  }
  if (!u.hostname) return false;
  if (u.protocol === "wss:") return true;
  if (u.protocol !== "ws:") return false;
  const h = u.hostname.replace(/^\[|\]$/g, "");
  return h === "localhost" || h === "::1" || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(h);
}

/** At most this many STUN servers (the engine's limit). */
export const MAX_STUN = 8;

/** STUN servers typed one per line: the list, or null when one is not `host:port` or there are
 *  too many (join.rs stun_server_ok). A `stun:` prefix is dropped, empty lines skipped. */
export function parseStun(text: string): string[] | null {
  const list = text
    .split(/\r?\n/)
    .map((l) => l.trim().replace(/^stun:/, ""))
    .filter(Boolean);
  if (list.length > MAX_STUN) return null;
  const ok = (s: string) => {
    const i = s.lastIndexOf(":");
    if (i <= 0) return false;
    const host = s.slice(0, i).replace(/^\[/, "").replace(/\]$/, "");
    const port = Number(s.slice(i + 1));
    return /^\d+$/.test(s.slice(i + 1)) && port > 0 && port < 65536 && host.length <= 253 && /^[A-Za-z0-9.:-]+$/.test(host);
  };
  return list.every(ok) ? list : null;
}

export const joinApi = {
  read,
  open: (addr: string) => whole("/api/join/open", addr),
  close: (addr: string) => whole("/api/join/close", addr),
  /** The open invite (a new one when there is none or it expired); `renew` makes a new one
   *  even then, and the old link and code stop working (#657: without `new` the engine hands
   *  back the same invite, so "New link" changed nothing). */
  invite: (addr: string, renew = false) => change("/api/join/invite", addr, inviteBody(renew)),
  /** The invite with its internet link (`invite.internet.url`): friends anywhere join peer to
   *  peer. A refusal says why: `internet_off` (off in the settings), `hub_url` (no usable hub
   *  address), `closed`… An engine from before internet invites ignores `internet`: the state
   *  then has no `invite.internet`. */
  inviteInternet: (addr: string, renew = false) => changeOrWhy("/api/join/invite", addr, inviteBody(renew, true)),
  /** The one link (`invite.share`): the internet invite when Invites work says Anywhere and it
   *  can be made, else the home network's; never refused for that. An engine from before it
   *  answers the home network's invite (no `share`). */
  inviteShare: async (addr: string, renew = false): Promise<JoinHostState | null> => {
    const s = await change("/api/join/invite", addr, inviteBody(renew, false, true));
    // An engine from before the one link (12.0) ignores `share`: its internet link, as its Internet
    // choice made it (the home network's invite stays when that is refused).
    if (s?.invite && !s.invite.share && s.settings.internetInvites && !s.invite.internet) {
      const r = await changeOrWhy("/api/join/invite", addr, inviteBody(false, true));
      return r.state ?? s;
    }
    return s;
  },
  decide: (addr: string, id: number, allow: boolean) => change("/api/join/decide", addr, { id, allow }),
  kick: (addr: string, player: number) => change("/api/join/kick", addr, { player }),
  settings: (addr: string, s: { block_system_buttons?: boolean; max_players?: number; stream_seat?: boolean }) =>
    change("/api/join/settings", addr, s),
  /** Internet invite settings; a refusal says which (`hub_url`, `stun`). */
  netSettings: (addr: string, s: NetSettingsBody) => changeOrWhy("/api/join/settings", addr, s),
  /** "Guests play as": the PS5's signed-in profiles and the host's choices for this console;
   *  null when the engine or the console didn't answer (or an engine from before it). */
  profiles: async (addr: string): Promise<ProfilesState | null> => {
    try {
      const r = await request(`/api/join/profiles?addr=${encodeURIComponent(addr)}`);
      return r.ok ? readProfiles(r.json) : null;
    } catch {
      return null;
    }
  },
  /** Sets every guest player's profile on this console; a refusal says why (`owner_confirm`: the
   *  host's own profile without `owner`, `duplicate`, `player`, `user`). */
  seats: async (addr: string, seats: SeatProfile[]): Promise<{ error: string | null }> => {
    try {
      const r = await request("/api/join/settings", { addr, guest_profiles: seats });
      return { error: r.ok ? null : errorOf(r.json) };
    } catch {
      return { error: "engine" };
    }
  },
};

/** How often, and for how long, the panel asks again while the host's stream is starting. */
export const OPEN_RETRY_MS = 2000;
export const OPEN_RETRY_FOR_MS = 90_000;

/** Opens Play Together for the panel. While the host's stream is not up yet the engine answers
 *  `not_playing`: after "Lower to" restarts the stream at a new bitrate, the panel shows again with
 *  the new session's first try, seconds before the PS5 lets go of the old one (#720: it said "Play
 *  Together could not start" and stayed so while friends could come back). So that answer is asked
 *  again every `everyMs` for up to `forMs`, while `alive()`; null when the panel went away. */
export async function openWhenPlaying(
  addr: string,
  alive: () => boolean,
  everyMs = OPEN_RETRY_MS,
  forMs = OPEN_RETRY_FOR_MS,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<JoinReply | null> {
  for (let waited = 0; ; waited += everyMs) {
    const reply = await joinApi.open(addr);
    if (!alive()) return null;
    if (reply.error !== "not_playing" || waited + everyMs > forMs) return reply;
    await wait(everyMs);
    if (!alive()) return null;
  }
}

/** `openWhenPlaying` for as long as the panel shows: `not_playing` is never the last word. The
 *  panel is only up while this device shows a stream, so once that time is over the refusal is
 *  told (`onRefused`, with what the engine found) and asking goes on: when the session does come
 *  up the panel opens by itself. It used to stay on "Play Together could not start (not_playing)"
 *  over a running picture until it was closed and opened again. Any other refusal is told and ends
 *  it. The opened state, or null (refused, or the panel went away). */
export async function openUntilPlaying(
  addr: string,
  alive: () => boolean,
  onRefused: (reply: { error: string; found?: string }) => void,
  everyMs = OPEN_RETRY_MS,
  forMs = OPEN_RETRY_FOR_MS,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<JoinHostState | null> {
  for (;;) {
    const reply = await openWhenPlaying(addr, alive, everyMs, forMs, wait);
    if (!reply) return null;
    if (reply.state) return reply.state;
    onRefused(reply);
    if (reply.error !== "not_playing") return null;
    await wait(everyMs);
    if (!alive()) return null;
  }
}

/** Sets one guest player's profile (`withSeat`). When the PS5 can't say which profile is the host's
 *  own (`owner_unknown`), the chosen one may be: `ask` puts the same question as for the host's own,
 *  and a yes saves it as confirmed (`owner`). A no changes nothing (`cancelled`). */
export async function saveSeat(
  addr: string,
  seats: SeatProfile[],
  player: number,
  user: ConsoleProfile | null,
  owner: boolean,
  ask: () => Promise<boolean>,
): Promise<{ error: string | null }> {
  const r = await joinApi.seats(addr, withSeat(seats, player, user, owner));
  if (r.error !== "owner_unknown" || !user || owner) return r;
  if (!(await ask())) return { error: "cancelled" };
  return joinApi.seats(addr, withSeat(seats, player, user, true));
}

/** The engine's Play Together state for a console: every second while it is open (the delays
 *  and asks are live), every 4 s otherwise. */
export function useJoinHost(on: boolean, addr: string | null): [JoinHostState, (s: JoinHostState) => void] {
  const [state, setState] = useState<JoinHostState>(EMPTY_JOIN);
  useEffect(() => {
    if (!on || !addr) {
      setState(EMPTY_JOIN);
      return;
    }
    let alive = true;
    let timer = 0;
    const read = async () => {
      const s = await joinApi.read(addr);
      if (!alive) return;
      if (s) setState(s);
      timer = window.setTimeout(() => void read(), s?.open ? 1000 : 4000);
    };
    void read();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [on, addr]);
  return [state, setState];
}
