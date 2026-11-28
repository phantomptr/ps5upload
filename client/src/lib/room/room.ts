// Play Rooms (13.0.0), the host's side: the `room` block the engine adds to GET /api/join, the extra
// facts on each player and ask, and the calls that change the room (design 3.1, 5, 10.1). Today's
// Play Together party *is* the room: an engine from before rooms says nothing of it, and the page
// then shows a Private room with the defaults (exactly what Play Together did).
//
// Read field by field like playTogetherState.readJoinState: an older or odd answer never breaks
// the page.

import { getEngineUrl } from "../../state/engine";
import { readTier, type Tier } from "../../join/routeTier";
import { readChatMode, type ChatMode } from "./chat";
import { readEngineChatLine } from "../joinProtocol";
import { NOTICE_VERSION } from "./notice";
import { readRegion, type Region } from "./regions";

export type Visibility = "private" | "friends" | "public";
export type VoiceMode = "off" | "ptt" | "open";

/** Where an ask or a player came from (design 3.2). */
export type Via = "invite" | "again" | "nearby" | "friends" | "rooms" | "group";

export interface Approval {
  /** People you've played with get in without asking. */
  friends: boolean;
  /** The password lets people in without asking (only with a password). */
  password: boolean;
  /** This household's PS5s (consoles PS5Upload knows) get in without asking. Off by default;
   * absent in a change: left as it is. */
  household?: boolean;
}

export interface RoomGame {
  title: string;
  /** PPSA01234 / CUSA01234, or null when the console didn't say. */
  id: string | null;
  from: "auto" | "picked";
}

/** "Listed in Rooms" / "Listing…" / "Not listed: <why>" (design 3.3). */
export interface Listed {
  state: "off" | "listing" | "listed" | "refused" | "old_hub";
  /** Listed until (unix seconds), when the hub said. */
  untilUnix?: number | null;
  why: string | null;
}

/** What people see before joining (design 6.4), each on by default. */
export interface PreviewShows {
  names: boolean;
  pings: boolean;
  stream: boolean;
  time: boolean;
}

export interface Blocked {
  name: string;
  tag: string;
  at: number | null;
}

export interface ReadyCheck {
  /** Seconds left of the 30 s check. */
  leftS: number;
  /** Players who said ready. */
  ready: number[];
  /** Everyone asked said ready (shown for a moment before the check ends). */
  all: boolean;
}

export interface RoomInfo {
  name: string;
  visibility: Visibility;
  /** Players including the host (2–4). */
  size: number;
  /** A password is set (the password itself never comes back). */
  password: boolean;
  approval: Approval;
  locked: boolean;
  chat: ChatMode;
  voice: VoiceMode;
  startMuted: boolean;
  /** Voice in and out of the engine for the whole room, kbit/s. */
  voiceKbps: { in: number; out: number } | null;
  previewShows: PreviewShows;
  /** Let people check their connection before asking (internet-direct preview, off by default). */
  checkBefore: boolean;
  ready: ReadyCheck | null;
  watchers: number;
  lang: string | null;
  region: Region | null;
  nearby: boolean;
  game: RoomGame | null;
  listed: Listed;
  blocked: Blocked[];
  /** The engine's link to the console (Remote Play round trip). */
  hostLink: { rttMs: number | null; tier: Tier | null } | null;
  /** When the room opened (ms since the epoch), for "Playing for N min". */
  openedAt: number | null;
  /** The engine knows rooms (13.0.0+). False: a Play Together engine; the room is shown with the
   *  defaults and nothing about rooms can be changed. */
  known: boolean;
  /** The room's chat as the host engine keeps it (the last 50 lines, memory only). */
  chatLog: HostChatLine[];
  /** Many wrong passwords lately (20 in 10 minutes): "Change it?". */
  manyWrongPasswords: boolean;
  /** This computer's device tag, and the room's lobby id on the hub (Friends/Public). */
  hostTag: string | null;
  lobby: string | null;
}

/** One line of the room's chat on the host's side (`room.chat_log`). */
export interface HostChatLine {
  /** The engine's line id (stable across reads). */
  n: number;
  /** Said this many times in a row (folded). */
  x: number;
  from: string;
  /** Sent by the host (this computer). */
  host: boolean;
  quick: number | null;
  text: string | null;
  at: number;
}

/** The engine's room defaults (design 2.3, 3.1): Private, friends auto-allowed, chat on. */
export const DEFAULT_ROOM: RoomInfo = {
  name: "",
  visibility: "private",
  size: 4,
  password: false,
  approval: { friends: true, password: false },
  locked: false,
  chat: "on",
  voice: "open",
  startMuted: false,
  voiceKbps: null,
  previewShows: { names: true, pings: true, stream: true, time: true },
  checkBefore: false,
  ready: null,
  watchers: 2,
  lang: null,
  region: null,
  nearby: true,
  game: null,
  listed: { state: "off", why: null },
  blocked: [],
  hostLink: null,
  openedAt: null,
  known: false,
  chatLog: [],
  manyWrongPasswords: false,
  hostTag: null,
  lobby: null,
};

/** The defaults a visibility brings (design 3.1, 7A.2): Public is quick messages, push-to-talk
 *  and new players muted; Private and Friends are chat on and open mic. */
export function visibilityDefaults(v: Visibility): Pick<RoomInfo, "chat" | "voice" | "startMuted"> {
  return v === "public" ? { chat: "quick", voice: "ptt", startMuted: true } : { chat: "on", voice: "open", startMuted: false };
}

const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);
const obj = (v: unknown): Record<string, unknown> | null => (v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null);

export function readVisibility(v: unknown): Visibility | null {
  return v === "private" || v === "friends" || v === "public" ? v : null;
}

export function readVoiceMode(v: unknown): VoiceMode | null {
  // The guest push says "ptt"; the settings may say "push_to_talk".
  if (v === "push_to_talk") return "ptt";
  return v === "off" || v === "ptt" || v === "open" ? v : null;
}

const VIAS: readonly Via[] = ["invite", "again", "nearby", "friends", "rooms", "group"];
export function readVia(v: unknown): Via | null {
  return typeof v === "string" && (VIAS as readonly string[]).includes(v) ? (v as Via) : null;
}

function readLink(v: unknown): RoomInfo["hostLink"] {
  const o = obj(v);
  if (!o) return null;
  return { rttMs: num(o.rtt_ms), tier: readTier(o.tier) };
}

/** `room` of GET /api/join, or the defaults (with `known: false`) when the engine has none. */
export function readRoom(v: unknown): RoomInfo {
  const o = obj(v);
  if (!o) return DEFAULT_ROOM;
  const visibility = readVisibility(o.visibility) ?? "private";
  const defs = visibilityDefaults(visibility);
  const ap = obj(o.approval);
  // `approval` may also come as one word ("ask"), the preview's form.
  const approval: Approval = ap
    ? { friends: ap.friends !== false, password: ap.password === true, household: ap.household === true }
    : { friends: true, password: false };
  const vk = obj(o.voice_kbps);
  const ps = obj(o.preview_shows);
  const rd = obj(o.ready);
  const li = obj(o.listed);
  const g = obj(o.game);
  const listedState = li?.state;
  return {
    name: typeof o.name === "string" ? o.name : "",
    visibility,
    size: Math.max(2, Math.min(4, num(o.size) ?? 4)),
    password: o.password === true,
    approval,
    locked: o.lock === true || o.locked === true,
    chat: readChatMode(o.chat) ?? defs.chat,
    voice: readVoiceMode(o.voice) ?? defs.voice,
    startMuted: typeof o.start_muted === "boolean" ? o.start_muted : defs.startMuted,
    voiceKbps: vk ? { in: num(vk.in) ?? 0, out: num(vk.out) ?? 0 } : null,
    previewShows: {
      names: ps?.names !== false,
      pings: ps?.pings !== false,
      stream: ps?.stream !== false,
      time: ps?.time !== false,
    },
    // The engine keeps it in preview_shows.check (join_room.rs PreviewShows).
    checkBefore: o.check_before === true || ps?.check === true,
    ready:
      rd && num(rd.left_s) != null
        ? {
            leftS: num(rd.left_s)!,
            ready: Array.isArray(rd.ready) ? rd.ready.filter((x): x is number => typeof x === "number") : [],
            all: rd.all === true,
          }
        : null,
    watchers: Math.max(0, Math.min(2, num(o.watchers) ?? 2)),
    lang: str(o.lang),
    region: readRegion(o.region),
    nearby: o.nearby !== false,
    game: g && typeof g.title === "string" ? { title: g.title, id: str(g.id), from: g.from === "picked" || g.picked === true ? "picked" : "auto" } : null,
    listed: {
      // The engine says "not_listed" with why (join_room.rs Listed); "old_hub" is one of its whys.
      state:
        listedState === "listing" || listedState === "listed" || listedState === "refused" || listedState === "old_hub"
          ? listedState
          : listedState === "not_listed"
            ? li?.why === "old_hub"
              ? "old_hub"
              : "refused"
            : "off",
      why: str(li?.why),
      untilUnix: num(li?.until_unix),
    },
    blocked: (Array.isArray(o.blocked) ? o.blocked : [])
      .map(obj)
      .filter((b): b is Record<string, unknown> => !!b && typeof b.tag === "string")
      .map((b) => ({ name: typeof b.name === "string" ? b.name : "", tag: b.tag as string, at: num(b.at) ?? num(b.at_unix) })),
    hostLink: readLink(o.host_link),
    openedAt:
      num(o.opened_at_ms) ??
      (num(o.opened_at) != null ? num(o.opened_at)! * 1000 : num(o.open_for_s) != null ? Date.now() - num(o.open_for_s)! * 1000 : null),
    known: true,
    manyWrongPasswords: o.many_wrong_passwords === true,
    hostTag: str(o.host_tag),
    lobby: str(o.lobby),
    chatLog: (Array.isArray(o.chat_log) ? o.chat_log : []).flatMap((l, i) => {
      const c = readEngineChatLine(l);
      return c ? [{ n: c.id || i + 1, x: c.x, from: c.from, host: c.host, quick: c.quick, text: c.text, at: c.t }] : [];
    }),
  };
}

/** The room facts of one player (beside playTogetherState's JoinGuest). */
export interface RoomPlayer {
  /** Where they came from (invite, Rooms…); null from an engine before rooms. */
  source: Via | null;
  /** Their device's short tag ("4F2A"). */
  tag: string | null;
  muted: { voice: boolean; chat: boolean };
  /** Came from the public Rooms list: never Player 1, PS & Create always off. */
  fromRooms: boolean;
  /** Talking now (the engine's speaking flags). */
  speaking: boolean;
  /** Said ready in the running ready check. */
  ready: boolean;
  /** Minutes in the room. */
  joinedAt: number | null;
  /** What they play with (ClientInfo kind: "ps5", "web", "app", "phone"). */
  client: string | null;
}

export function readRoomPlayer(g: Record<string, unknown>): RoomPlayer {
  const m = obj(g.muted);
  // The engine's word for how they found the room is `came`; `via` there means lan/internet (an
  // older engine's room word only when it is one).
  const source = readVia(g.came) ?? readVia(g.via);
  return {
    source,
    tag: str(g.tag),
    muted: { voice: m?.voice === true || g.muted === true, chat: m?.chat === true },
    fromRooms: g.from_rooms === true || source === "rooms",
    speaking: g.speaking === true || obj(g.voice)?.speaking === true,
    ready: g.ready === true,
    joinedAt: num(g.joined_at_ms) ?? (num(g.joined_at) != null ? num(g.joined_at)! * 1000 : null),
    client: str(g.client_kind),
  };
}

/** The room facts of one ask: where it came from, the password check, tag and a ping hint. */
export interface RoomAsk {
  source: Via | null;
  /** True: the right password; false: a wrong one (asks with a wrong one never reach the host,
   *  but an older engine may pass them); null: none needed. */
  passwordOk: boolean | null;
  tag: string | null;
  rttHint: number | null;
  /** Several controllers from one PS5 asking once (integration P2). */
  group: number | null;
  /** From the host's own device (this computer, or a PS5 whose PS5Upload Play is paired here):
   *  "It's me" lets it in as Player 1. */
  own: boolean;
}

export function readRoomAsk(a: Record<string, unknown>): RoomAsk {
  return {
    source: readVia(a.came) ?? readVia(a.via),
    passwordOk: typeof a.password_ok === "boolean" ? a.password_ok : null,
    tag: str(a.tag),
    rttHint: num(a.rtt_hint),
    // Another controller of a PS5 already in: `players` names everyone from that PS5 (with the one
    // asking); `group` is the first player's id, not a count.
    group: Array.isArray(a.players) && a.players.length > 1 ? a.players.length : null,
    own: a.own === true,
  };
}

/** A room as the engine takes it (POST /api/join/room and /api/join/open `room`). The password
 *  goes only to this computer's engine; "" removes it. */
export interface RoomBody {
  name?: string;
  visibility?: Visibility;
  size?: number;
  password?: string;
  approval?: Approval;
  chat?: ChatMode;
  voice?: VoiceMode;
  start_muted?: boolean;
  watchers?: number;
  lang?: string | null;
  region?: Region | null;
  nearby?: boolean;
  lock?: boolean;
  /** What people see before joining, all of it (the engine's PreviewShows), with `check`:
   *  "Let people check their connection before asking". */
  preview?: PreviewShows & { check: boolean };
  game?: { id: string; title: string } | null;
  /** The first-time Public confirm was given on this device. */
  public_ok?: boolean;
}

/** A room body as the engine reads it: Any language is "any", a hidden region is "hidden". */
export function engineBody(b: RoomBody): Record<string, unknown> {
  const out: Record<string, unknown> = { ...b };
  if ("lang" in b) out.lang = b.lang ?? "any";
  if ("region" in b) out.region = b.region ?? "hidden";
  return out;
}

/** Why the engine refused a room change, in words (design 3.1, 10.1). */
export type RoomRefusal =
  | "size_below_players"
  | "public_needs_anywhere"
  | "password_short"
  | "name"
  | "friends_playing"
  | "not_open"
  /** Chat and voice (join_comms): the notice first, chat off or quick only, too fast… */
  | "notice"
  | "off"
  | "quick_only"
  | "slow_down"
  | "empty"
  | "too_long"
  | "bad_quick"
  | "no_player"
  | "host"
  | "old_engine"
  | "engine";

const REFUSALS: readonly RoomRefusal[] = [
  "size_below_players",
  "public_needs_anywhere",
  "password_short",
  "name",
  "friends_playing",
  "not_open",
  "notice",
  "off",
  "quick_only",
  "slow_down",
  "empty",
  "too_long",
  "bad_quick",
  "no_player",
  "host",
];

async function request(method: "GET" | "POST", path: string, body?: object): Promise<{ status: number; json: unknown }> {
  const r = await fetch(`${getEngineUrl()}${path}`, {
    method,
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
  return { status: r.status, json };
}

/** The refusal of a 4xx answer: a known word, `old_engine` for a 404 (an engine before rooms). */
export function refusalOf(status: number, json: unknown): RoomRefusal {
  if (status === 404 || status === 405) return "old_engine";
  const e = (json as { error?: unknown } | null)?.error;
  return typeof e === "string" && (REFUSALS as readonly string[]).includes(e) ? (e as RoomRefusal) : "engine";
}

async function post(path: string, body: object): Promise<{ ok: true; json: unknown } | { ok: false; error: RoomRefusal; json: unknown }> {
  try {
    const r = await request("POST", path, body);
    if (r.status >= 200 && r.status < 300) return { ok: true, json: r.json };
    return { ok: false, error: refusalOf(r.status, r.json), json: r.json };
  } catch {
    return { ok: false, error: "engine", json: null };
  }
}

/** The host's room calls (loopback / ALLOW_IP only, like the rest of /api/join). Each answers
 *  ok or the refusal; the panel reads the whole state after (useJoinHost polls every second). */
export const roomApi = {
  /** Change the room's settings live (design 4): people in always stay. */
  update: (addr: string, body: RoomBody) => post("/api/join/room", { addr, ...engineBody(body) }),
  lock: (addr: string, lock: boolean) => post("/api/join/room", { addr, lock }),
  /** Remove a player; `block`: "room" (not back in this room) or "always" (this computer). */
  kick: (addr: string, player: number, block: "room" | "always" | null) =>
    post("/api/join/kick", block ? { addr, player, block } : { addr, player }),
  unblock: (addr: string, tag: string) => post("/api/join/unblock", { addr, tag }),
  /** Host mutes (enforced in the engine). `all`: every player's voice. */
  mute: (addr: string, m: { player?: number; voice?: boolean; chat?: boolean; all?: boolean }) =>
    post("/api/join/mute", { addr, ...m }),
  /** Move a player to another seat (2–4), give Player 1 (1) or take it back ("back"). */
  seat: (addr: string, player: number, to: number | "back") => post("/api/join/seat", { addr, player, to }),
  /** Chat and voice modes (API.md "Play Rooms chat and voice": POST /api/join/comms). */
  comms: (addr: string, m: { chat?: ChatMode; voice?: VoiceMode; start_muted?: boolean }) => post("/api/join/comms", { addr, ...m }),
  /** The host's own chat line (with the notice this device accepted). */
  chat: (addr: string, m: { quick: number } | { text: string }) => post("/api/join/chat", { addr, ...m, notice: NOTICE_VERSION }),
  /** The host's own mic on this device. */
  mic: (addr: string, mic: boolean) => post("/api/join/voice", { addr, mic: mic ? "on" : "off", notice: NOTICE_VERSION }),
  ready: (addr: string) => post("/api/join/ready", { addr, start: true }),
  /** Let everyone from one PS5 in at once (a group ask). */
  allowGroup: (addr: string, id: number) => post("/api/join/decide", { addr, id, allow: "group" }),
  /** An ask from the host's own device: in as Player 1 (the stream's controller). */
  itsMe: (addr: string, id: number) => post("/api/join/decide", { addr, id, allow: "me" }),
};

/** One of the host's recent players (GET /api/join/history; never in bug reports). */
export interface RecentPlayer {
  id: string;
  name: string;
  tag: string;
  lastUnix: number;
  minutes: number;
  source: Via | null;
}

export function readHistory(v: unknown): RecentPlayer[] {
  const list = (obj(v)?.players ?? obj(v)?.history) as unknown;
  if (!Array.isArray(list)) return [];
  return list
    .map(obj)
    .filter((p): p is Record<string, unknown> => !!p && typeof p.id === "string")
    .map((p) => ({
      id: p.id as string,
      name: typeof p.name === "string" ? p.name : "",
      tag: typeof p.tag === "string" ? p.tag : "",
      lastUnix: num(p.last_unix) ?? num(p.at) ?? 0,
      minutes: num(p.minutes) ?? 0,
      source: readVia(p.came) ?? readVia(p.via),
    }));
}

export const historyApi = {
  read: async (): Promise<RecentPlayer[] | null> => {
    try {
      const r = await request("GET", "/api/join/history");
      return r.status === 200 ? readHistory(r.json) : null;
    } catch {
      return null;
    }
  },
  forget: (id: string | "all") => post("/api/join/history/forget", id === "all" ? { all: true } : { id }),
};

/** "Playing for N min" from a start time. */
export function minutesSince(at: number | null, now: number): number | null {
  return at == null ? null : Math.max(0, Math.floor((now - at) / 60_000));
}

/** People in the room now (the host counts as one): what Close room and size changes name. */
export function peopleIn(guests: ReadonlyArray<{ status: string }>): number {
  return 1 + guests.filter((g) => g.status !== "left" && g.status !== "asking").length;
}

/** Room size choices a host may pick now: 2–4, never below the people in (design 4.4). */
export function sizeChoices(inRoom: number): Array<{ size: number; ok: boolean }> {
  return [2, 3, 4].map((size) => ({ size, ok: size >= inRoom }));
}

/** The approval switch for the password: only with a password (design 3.1, 4.3). */
export function approvalFor(a: Approval, hasPassword: boolean): Approval {
  return hasPassword ? a : { ...a, password: false };
}
