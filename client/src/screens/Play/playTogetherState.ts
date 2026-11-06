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
  } | null;
  guests: JoinGuest[];
  asks: Array<{ id: number; name: string }>;
  settings: { blockSystemButtons: boolean; maxPlayers: number };
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
  settings: { blockSystemButtons: true, maxPlayers: 3 },
  helperOk: true,
  reason: null,
  ws: null,
};

const STATES: ReadonlySet<string> = new Set(["asking", "picking_user", "playing", "reconnecting", "left"]);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
/** A guest's state; an unknown one counts as in the game (never hides a friend). */
const stateOf = (v: unknown): GuestState => (typeof v === "string" && STATES.has(v) ? v : "playing") as GuestState;

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
      rttMs: num(g.rtt_ms),
      connected: g.connected !== false,
      behind: g.behind === true,
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
    },
    helperOk: o.helper_ok !== false,
    reason: typeof o.reason === "string" && o.reason ? o.reason : null,
    ws: typeof o.ws === "string" && /^wss?:\/\//.test(o.ws) ? o.ws : invite ? socketOf(invite.url) : null,
  };
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

/** The body of an invite call: `new` only for a fresh link. */
export function inviteBody(renew: boolean): { new?: true } {
  return renew ? { new: true } : {};
}

export const joinApi = {
  read,
  open: (addr: string) => whole("/api/join/open", addr),
  close: (addr: string) => whole("/api/join/close", addr),
  /** The open invite (a new one when there is none or it expired); `renew` makes a new one
   *  even then, and the old link and code stop working (#657: without `new` the engine hands
   *  back the same invite, so "New link" changed nothing). */
  invite: (addr: string, renew = false) => change("/api/join/invite", addr, inviteBody(renew)),
  decide: (addr: string, id: number, allow: boolean) => change("/api/join/decide", addr, { id, allow }),
  kick: (addr: string, player: number) => change("/api/join/kick", addr, { player }),
  settings: (addr: string, s: { block_system_buttons?: boolean; max_players?: number }) =>
    change("/api/join/settings", addr, s),
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
