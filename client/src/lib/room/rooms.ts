// Play Rooms: the Rooms browser (design 7.0). Public cards come from the hub's cached list
// (GET /rooms, through the engine's GET /api/join/rooms in the apps; straight from the hub on the
// website). The hub sorts and filters; this file reads its answer, refines the order on the client
// (measured pings beat estimates, friends first), and decides what each card's button says.

import { classify, type Tier } from "../../join/routeTier";
import { nearRegion, readRegion, regionEstMs, type Region } from "./regions";

/** This build's room protocol (design 10.5): 1 = 12.6 Play Together, 2 = rooms. */
export const ROOM_PROTO = 2;
/** What this build understands (design 10.5 capability flags). */
export const ROOM_CAPS = ["rooms", "knock", "sealed", "preview", "chat", "voice", "ready", "seat_move", "watch"] as const;

export type RoomSort = "best" | "latency" | "game" | "players" | "space" | "newest";
export const ROOM_SORTS: readonly RoomSort[] = ["best", "latency", "game", "players", "space", "newest"];

/** One public room card (the hub's list entry). Never an address. */
export interface RoomCard {
  /** The hub's lobby room id (22 base64url characters). */
  room: string;
  name: string;
  host: string;
  tag: string;
  game: { title: string; id: string | null } | null;
  proto: number;
  ver: string;
  caps: string[];
  /** Players in, room size. */
  inRoom: number;
  size: number;
  pw: boolean;
  lang: string | null;
  region: Region | null;
  /** The hub's estimate of this viewer's round trip, ms. */
  estMs: number | null;
  ageS: number;
  /** The room's public key, for sealed knocks. */
  pk: string | null;
}

export interface RoomList {
  rooms: RoomCard[];
  next: number | null;
  /** The viewer's region as the hub sees it. */
  you: { country: string | null; region: Region | null };
  proto: { min: number; latest: number };
  appLatest: string | null;
  /** The hub has rooms turned off (`off`), or none (an old self-hosted hub: 404). */
  off: boolean;
}

const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

/** One card, checked; null for anything that isn't one. */
export function readCard(v: unknown): RoomCard | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const room = str(o.room);
  const name = str(o.name);
  if (!room || !/^[A-Za-z0-9_-]{16,32}$/.test(room) || !name) return null;
  const slots = Array.isArray(o.slots) ? o.slots : [];
  const g = o.game as Record<string, unknown> | null | undefined;
  const size = Math.max(1, Math.min(8, num(slots[1]) ?? 4));
  return {
    room,
    name: name.slice(0, 40),
    host: (str(o.host) ?? "").slice(0, 32),
    tag: (str(o.tag) ?? "").slice(0, 8),
    game: g && typeof g === "object" && typeof g.title === "string" ? { title: g.title.slice(0, 80), id: str(g.id) } : null,
    proto: num(o.proto) ?? 1,
    ver: str(o.ver) ?? "",
    caps: Array.isArray(o.caps) ? o.caps.filter((c): c is string => typeof c === "string").slice(0, 32) : [],
    inRoom: Math.max(0, Math.min(size, num(slots[0]) ?? 0)),
    size,
    pw: o.pw === true,
    lang: str(o.lang),
    region: readRegion(o.region),
    estMs: num(o.est_ms),
    ageS: num(o.age_s) ?? 0,
    pk: str(o.pk),
  };
}

/** The hub's (or the engine proxy's) list answer. */
export function readRoomList(v: unknown): RoomList {
  const o = (v ?? {}) as Record<string, unknown>;
  const you = (o.you ?? {}) as Record<string, unknown>;
  const p = (o.proto ?? {}) as Record<string, unknown>;
  const app = (o.app ?? {}) as Record<string, unknown>;
  const rooms = (Array.isArray(o.rooms) ? o.rooms : []).map(readCard).filter((c): c is RoomCard => !!c);
  return {
    rooms,
    next: num(o.next),
    you: { country: str(you.country), region: readRegion(you.region) },
    proto: { min: num(p.min) ?? 1, latest: num(p.latest) ?? ROOM_PROTO },
    appLatest: str(app.latest),
    off: o.off === true,
  };
}

/** The list's query (design 9.1), only what differs from the plain first page. */
export interface RoomQuery {
  sort: RoomSort;
  game: string;
  free: boolean;
  nopw: boolean;
  /** "Works with my version": the viewer's protocol is sent; incompatible rooms are dropped. */
  compatible: boolean;
  lang: string | null;
  near: boolean;
  page: number;
  /** This device's own region (time zone), to refine the hub's guess. */
  me: Region | null;
}

export const DEFAULT_QUERY: RoomQuery = {
  sort: "best",
  game: "",
  free: true,
  nopw: false,
  compatible: true,
  lang: null,
  near: false,
  page: 0,
  me: null,
};

export function queryString(q: RoomQuery): string {
  const p = new URLSearchParams();
  if (q.sort !== "best") p.set("sort", q.sort);
  const game = q.game.trim();
  if (game) p.set("game", game.slice(0, 32));
  if (q.free) p.set("free", "1");
  if (q.nopw) p.set("nopw", "1");
  if (q.compatible) p.set("proto", String(ROOM_PROTO));
  if (q.lang) p.set("lang", q.lang);
  if (q.near) p.set("near", "1");
  if (q.me) p.set("me", q.me);
  if (q.page > 0) p.set("page", String(q.page));
  return p.toString();
}

/** A filter hides something: "Clear filters" shows. */
export function filtered(q: RoomQuery): boolean {
  return !!q.game.trim() || q.nopw || q.lang != null || q.near || !q.free;
}

/** What a viewer knows of a host from before (by tag): the last 3 games' round trips. */
export interface HostHistory {
  tag: string;
  rtts: number[];
  /** A real measurement in the last 10 min (Details' ping check). */
  measured?: { ms: number; at: number } | null;
}

/** The expected round trip on a card, best source first (design 6.3): a fresh measurement,
 *  your history with that host ("last time"), the hub's estimate, the region pair; null: unknown. */
export interface Expected {
  ms: number;
  kind: "measured" | "last_time" | "about";
  tier: Tier;
}

const MEASURE_FRESH_MS = 10 * 60_000;

export function expectedFor(card: Pick<RoomCard, "estMs" | "region">, me: Region | null, hist: HostHistory | null, now: number): Expected | null {
  const tierOf = (ms: number): Tier => classify({ rttMs: ms }) ?? "poor";
  if (hist?.measured && now - hist.measured.at < MEASURE_FRESH_MS) {
    return { ms: hist.measured.ms, kind: "measured", tier: tierOf(hist.measured.ms) };
  }
  if (hist && hist.rtts.length) {
    const last3 = [...hist.rtts.slice(-3)].sort((a, b) => a - b);
    const med = last3[Math.floor(last3.length / 2)];
    return { ms: med, kind: "last_time", tier: tierOf(med) };
  }
  const est = card.estMs ?? regionEstMs(me, card.region);
  return est == null ? null : { ms: est, kind: "about", tier: tierOf(est) };
}

/** What a card's button is (design 7.0): Join (they'd be let in without asking), Ask to join,
 *  Full (greyed), Update to join (this build is too old for the room), or Host needs to update. */
export type CardAction = "join" | "ask" | "full" | "update" | "host_old";

export function cardAction(
  card: Pick<RoomCard, "inRoom" | "size" | "proto" | "pw">,
  o: { minProto?: number; autoIn?: boolean } = {},
): CardAction {
  if (card.proto > ROOM_PROTO + 2 || (o.minProto != null && ROOM_PROTO < o.minProto)) return "update";
  if (card.proto < ROOM_PROTO - 2) return "host_old";
  if (card.inRoom >= card.size) return "full";
  return o.autoIn ? "join" : "ask";
}

/** Re-sorts the hub's page on the client: only measured/last-time values can move a card, and
 *  never while one is focused (design 7.0: the list doesn't jump under the pointer). */
export function refineOrder(cards: RoomCard[], sort: RoomSort, exp: (c: RoomCard) => Expected | null, frozen: boolean): RoomCard[] {
  if (frozen || (sort !== "best" && sort !== "latency")) return cards;
  const key = (c: RoomCard) => exp(c)?.ms ?? Number.MAX_SAFE_INTEGER;
  const space = (c: RoomCard) => (c.inRoom < c.size ? 0 : 1);
  const idx = new Map(cards.map((c, i) => [c.room, i]));
  return [...cards].sort((a, b) => {
    if (sort === "best" && space(a) !== space(b)) return space(a) - space(b);
    const d = key(a) - key(b);
    if (d !== 0) return d;
    // Measured beats estimated at equal values.
    const m = (c: RoomCard) => (exp(c)?.kind === "measured" ? 0 : 1);
    if (m(a) !== m(b)) return m(a) - m(b);
    return idx.get(a.room)! - idx.get(b.room)!;
  });
}

/** Client-side filters the hub doesn't apply to friends' and nearby rooms (the same chips). */
export function matchesFilters(
  card: Pick<RoomCard, "game" | "inRoom" | "size" | "pw" | "lang" | "region" | "proto">,
  q: RoomQuery,
  me: Region | null,
): boolean {
  const g = q.game.trim().toLowerCase();
  if (g && !(card.game && (card.game.title.toLowerCase().includes(g) || (card.game.id ?? "").toLowerCase() === g))) return false;
  if (q.free && card.inRoom >= card.size) return false;
  if (q.nopw && card.pw) return false;
  if (q.lang && card.lang && card.lang !== q.lang) return false;
  if (q.near && !nearRegion(me, card.region)) return false;
  if (q.compatible && cardAction(card) === "update") return false;
  return true;
}

/** Rooms hidden on this device ("Hide this room/host", by tag or room id). */
export function hiddenBy(card: Pick<RoomCard, "room" | "tag">, hidden: ReadonlySet<string>): boolean {
  return hidden.has(card.room) || (!!card.tag && hidden.has(`tag:${card.tag}`));
}

/** The four groups of the browser, top to bottom (design 7.0). */
export type RoomGroup = "friends" | "shared" | "nearby" | "public";
export const ROOM_GROUPS: readonly RoomGroup[] = ["friends", "shared", "nearby", "public"];

/** "Needs PS5Upload 13.0.0 or newer": the version a card's protocol needs (2 → 13.0.0). */
export function versionForProto(p: number): string {
  return p <= 1 ? "12.6.0" : `${11 + p}.0.0`;
}

/** a < b for dotted versions ("12.6.45" < "13.0.0"). */
export function olderVersion(a: string, b: string): boolean {
  const pa = a.split(/[.-]/).map((x) => parseInt(x, 10) || 0);
  const pb = b.split(/[.-]/).map((x) => parseInt(x, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d < 0;
  }
  return false;
}
