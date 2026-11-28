// Play Rooms: the hub's HTTP side (ps5upload-hub PROTOCOL.md "Rooms" › HTTP): the public list,
// the friends lookup and the preview. The apps call the hub through their engine
// (GET /api/join/rooms, one place for the custom hub address); the website calls the hub straight
// (CORS for our origins only). A 429 is never shown to a person: the client waits Retry-After
// (with a little jitter) and asks again, silently, a few times.

import { readPreview, type RoomPreview } from "./preview";
import { readRoomList, queryString, type RoomList, type RoomQuery } from "./rooms";

/** One room in the friends lookup (`GET /rooms/status`). */
export interface RoomStatus {
  listed: boolean;
  locked: boolean;
  inRoom: number;
  size: number;
  pw: boolean;
  proto: number;
  game: { title: string; id: string | null } | null;
  pk: string | null;
}

export interface StatusAnswer {
  /** How often to ask again (60 s, 300 in cut-back mode). */
  everyS: number;
  rooms: Record<string, RoomStatus>;
}

const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

export function readStatus(v: unknown): StatusAnswer {
  const o = (v ?? {}) as Record<string, unknown>;
  const rooms: Record<string, RoomStatus> = {};
  const r = o.rooms;
  if (r && typeof r === "object" && !Array.isArray(r)) {
    for (const [id, x] of Object.entries(r as Record<string, unknown>).slice(0, 16)) {
      if (!x || typeof x !== "object" || !/^[A-Za-z0-9_-]{16,32}$/.test(id)) continue;
      const s = x as Record<string, unknown>;
      const slots = Array.isArray(s.slots) ? s.slots : [];
      const g = s.game as Record<string, unknown> | null | undefined;
      rooms[id] = {
        listed: s.listed === true,
        locked: s.locked === true,
        inRoom: num(slots[0]) ?? 0,
        size: num(slots[1]) ?? 4,
        pw: s.pw === true,
        proto: num(s.proto) ?? 2,
        game: g && typeof g.title === "string" ? { title: g.title, id: str(g.id) } : null,
        pk: str(s.pk),
      };
    }
  }
  return { everyS: Math.max(30, Math.min(3600, num(o.every_s) ?? 60)), rooms };
}

/** Why a call gave nothing: the hub has no rooms (an old self-hosted one), it's off, previews
 *  need a direct connection, the host didn't answer, or the hub can't be reached. */
export type HubMiss = "old_hub" | "off" | "no_room" | "needs_direct" | "no_answer" | "unreachable";

export interface HubOptions {
  fetchImpl?: typeof fetch;
  wait?: (ms: number) => Promise<void>;
  random?: () => number;
  /** Tries on 429 before giving up quietly (as "unreachable"). */
  tries?: number;
}

/** How long to wait after a 429: Retry-After (or retry_s), at least 1 s, plus up to 30 % jitter. */
export function retryDelayMs(retryAfter: string | null, body: unknown, random: () => number = Math.random): number {
  const fromHeader = retryAfter != null && /^\d+$/.test(retryAfter.trim()) ? Number(retryAfter) : null;
  const fromBody = num((body as { retry_s?: unknown } | null)?.retry_s);
  const s = Math.max(1, Math.min(120, fromHeader ?? fromBody ?? 2));
  return Math.round(s * 1000 * (1 + 0.3 * random()));
}

/** GET with the quiet 429 handling. */
export async function hubGet(url: string, o: HubOptions = {}): Promise<{ status: number; json: unknown } | null> {
  const f = o.fetchImpl ?? fetch;
  const wait = o.wait ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const tries = o.tries ?? 3;
  for (let i = 0; i < tries; i++) {
    let r: Response;
    try {
      r = await f(url, { credentials: "omit" });
    } catch {
      return null;
    }
    let json: unknown;
    try {
      json = await r.json();
    } catch {
      json = null;
    }
    if (r.status !== 429) return { status: r.status, json };
    await wait(retryDelayMs(r.headers.get("retry-after"), json, o.random));
  }
  return null;
}

/** The base the calls go to: the hub's own https origin (the website), or the engine's proxy
 *  (`<engine>/api/join/rooms`, which takes the same paths after it). */
export interface RoomsSource {
  /** `https://hub…` (wss:// hub addresses are turned into https://). */
  base: string;
  /** The engine proxy: paths go as `?path=`-free suffixes: /api/join/rooms, /api/join/rooms/status… */
  viaEngine: boolean;
}

/** The hub's HTTP origin from its WebSocket address (`wss://hub.example/ws` → https://hub.example). */
export function hubHttpBase(hubUrl: string): string | null {
  try {
    const u = new URL(hubUrl);
    const proto = u.protocol === "wss:" || u.protocol === "https:" ? "https:" : u.protocol === "ws:" || u.protocol === "http:" ? "http:" : null;
    return proto ? `${proto}//${u.host}` : null;
  } catch {
    return null;
  }
}

function path(src: RoomsSource, p: "" | "/status" | `/${string}/preview`): string {
  return src.viaEngine ? `${src.base}/api/join/rooms${p}` : `${src.base}/rooms${p}`;
}

/** The public list, or why there is none. */
export async function fetchRooms(src: RoomsSource, q: RoomQuery, o: HubOptions = {}): Promise<RoomList | HubMiss> {
  const qs = queryString(q);
  const r = await hubGet(`${path(src, "")}${qs ? `?${qs}` : ""}`, o);
  if (!r) return "unreachable";
  if (r.status === 404) return "old_hub";
  if (r.status !== 200) return "unreachable";
  const list = readRoomList(r.json);
  return list.off ? "off" : list;
}

/** Friends' rooms by lobby id (at most 16). */
export async function fetchStatus(src: RoomsSource, ids: string[], o: HubOptions = {}): Promise<StatusAnswer | HubMiss> {
  const list = ids.filter((id) => /^[A-Za-z0-9_-]{16,32}$/.test(id)).slice(0, 16);
  if (!list.length) return { everyS: 60, rooms: {} };
  const r = await hubGet(`${path(src, "/status")}?r=${list.join(",")}`, o);
  if (!r) return "unreachable";
  if (r.status === 404) return "old_hub";
  if (r.status !== 200) return "unreachable";
  return readStatus(r.json);
}

/** A room's preview through the hub (cached there 10 s). */
export async function fetchHubPreview(src: RoomsSource, room: string, o: HubOptions = {}): Promise<RoomPreview | HubMiss> {
  if (!/^[A-Za-z0-9_-]{16,32}$/.test(room)) return "no_room";
  const r = await hubGet(path(src, `/${room}/preview`), { ...o, tries: 1 });
  if (!r) return "unreachable";
  if (r.status === 404) return "no_room";
  if (r.status === 503) return "needs_direct";
  if (r.status === 504 || r.status === 502) return "no_answer";
  if (r.status !== 200) return "unreachable";
  return readPreview(r.json) ?? "no_answer";
}

/** A preview straight from the host engine (home network / Tailscale), `GET /api/join/preview`. */
export async function fetchEnginePreview(origin: string, room: string | null, o: HubOptions = {}): Promise<RoomPreview | HubMiss> {
  const r = await hubGet(`${origin.replace(/\/$/, "")}/api/join/preview${room ? `?room=${encodeURIComponent(room)}` : ""}`, { ...o, tries: 1 });
  if (!r) return "unreachable";
  if (r.status === 404) return "no_room";
  if (r.status !== 200) return "no_answer";
  return readPreview(r.json) ?? "no_answer";
}

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** A miss in words, with what to do (design 8.1). */
export function hubMissText(tr: Tr, m: HubMiss): string {
  switch (m) {
    case "old_hub":
      return tr("rooms_old_hub", undefined, "Your rooms service has no public rooms. Check Settings › Play Room.");
    case "off":
      return tr("rooms_off", undefined, "Public rooms are switched off on the rooms service right now.");
    case "needs_direct":
      return tr("rooms_needs_direct", undefined, "Details need a direct connection right now.");
    case "no_answer":
      return tr("rooms_no_answer", undefined, "The host didn't answer. Try again in a moment.");
    case "no_room":
      return tr("rooms_no_room", undefined, "This room is closed.");
    case "unreachable":
      return tr("rooms_unreachable", undefined, "Can't reach the rooms list. Check your internet.");
  }
}
