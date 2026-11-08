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
  /** Seated in the PS5 profile the host chose for their player ("Guests play as"): its name. */
  playsAs: string | null;
  /** A profile was chosen for their player but couldn't be used, so the TV asks who they are:
   *  why ("signed_out", "owner", "starting", "unavailable", "failed", "old_helper", "unknown"). */
  seatNote: string | null;
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
}

/** The host's Play Together choices for internet invites (every console). */
export interface NetSettings {
  /** Internet invites allowed; null: an engine from before internet invites (none offered). */
  internetInvites: boolean | null;
  hubUrl: string;
  stun: string[];
  upnp: boolean;
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
  settings: { blockSystemButtons: true, maxPlayers: 3, streamSeat: false, internetInvites: null, hubUrl: "", stun: [], upnp: true },
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
function readNetInvite(v: unknown): NetInvite | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  return { url: str(o.url), hubState: str(o.state), hubError: str(o.error) };
}

/** The engine's internet peers (`internet.peers`, or the invite's own list). */
function readPeers(v: unknown): NetPeer[] {
  return (Array.isArray(v) ? v : [])
    .filter((p): p is Record<string, unknown> => !!p && typeof p === "object" && typeof p.gid === "string")
    .map((p) => ({ gid: p.gid as string, path: str(p.path) ?? "connecting", rttMs: num(p.rtt_ms) }));
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
      // An older engine says nothing: the TV asks every guest.
      playsAs: str((g.plays_as as Record<string, unknown> | null | undefined)?.name),
      seatNote: str(g.seat_note),
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
      hubUrl: typeof s.hub_url === "string" ? s.hub_url : "",
      stun: Array.isArray(s.stun) ? s.stun.filter((x): x is string => typeof x === "string") : [],
      upnp: s.upnp !== false,
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

/** A call's answer: the state, or the engine's reason it said no (`old_helper`, `not_playing`…). */
export type JoinReply = { state: JoinHostState; error?: undefined } | { state?: undefined; error: string };

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
    return r.ok ? { state: readJoinState(r.json) } : { error: errorOf(r.json) };
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

/** The body of an invite call: `new` only for a fresh link, `internet` only for an internet one. */
export function inviteBody(renew: boolean, internet = false): { new?: true; internet?: true } {
  return { ...(renew ? { new: true as const } : {}), ...(internet ? { internet: true as const } : {}) };
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
  internet_invites?: boolean;
  hub_url?: string;
  stun?: string[];
  upnp?: boolean;
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
