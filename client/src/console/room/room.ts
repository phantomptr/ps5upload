import { useEffect, useState } from "react";
import { call } from "../api";
import { t } from "../strings";
import { pollWhileVisible } from "../ui/visiblePoll";

/**
 * The Play Room an engine hosts on this console (13.0.0): the helper keeps what the host's
 * PS5Upload sends it (payload/include/play_host.h, `play.room`) and serves it read only at
 * POST /api/room. Minion shows it ("Your room", Home) and names the players before anything that
 * would end their game (close a game, stop a process: those calls then carry `room_ok`).
 */

export interface RoomPlayer {
  name: string;
  ping_ms: number | null;
  seat: number | null;
  /** The rooms design's tier: 4 Excellent .. 1 Poor, 0 no reading; null: from ping_ms. */
  bars: number | null;
  via: string;
}

export interface Room {
  engine_id: string;
  hosted_from: string;
  kind: string;
  /** The host device's session with this console is live. */
  online: boolean;
  name: string;
  visibility: "private" | "friends" | "public";
  locked: boolean;
  game: { name: string | null; title_id: string | null } | null;
  size: number | null;
  playing: number;
  asks: number;
  chat: boolean;
  voice: boolean;
  players: RoomPlayer[];
  age_s: number;
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);

function parsePlayer(v: unknown): RoomPlayer | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const name = str(o.name);
  if (!name) return null;
  return { name, ping_ms: num(o.ping_ms), seat: num(o.seat), bars: num(o.bars), via: str(o.via) };
}

/** /api/room's "room" (or null for anything else: no room, an older helper). */
export function parseRoom(v: unknown): Room | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  const name = str(o.name);
  if (!name) return null;
  const vis = str(o.visibility);
  const g = o.game && typeof o.game === "object" ? (o.game as Record<string, unknown>) : null;
  const players = Array.isArray(o.players) ? o.players.map(parsePlayer).filter((p): p is RoomPlayer => !!p) : [];
  return {
    engine_id: str(o.engine_id),
    hosted_from: str(o.hosted_from),
    kind: str(o.kind),
    online: o.online === true,
    name,
    visibility: vis === "public" || vis === "friends" ? vis : "private",
    locked: o.locked === true,
    game: g ? { name: str(g.name) || null, title_id: str(g.title_id) || null } : null,
    size: num(o.size),
    playing: Math.max(num(o.playing) ?? 0, players.length),
    asks: num(o.asks) ?? 0,
    chat: o.chat === true,
    voice: o.voice === true,
    players,
    age_s: num(o.age_s) ?? 0,
  };
}

/** Bars for a player: the engine's tier when it sent one, else from the round trip (design 6.1). */
export function pingBars(p: Pick<RoomPlayer, "bars" | "ping_ms">): number {
  if (p.bars != null) return Math.max(0, Math.min(4, Math.round(p.bars)));
  if (p.ping_ms == null) return 0;
  if (p.ping_ms <= 20) return 4;
  if (p.ping_ms <= 40) return 3;
  if (p.ping_ms <= 80) return 2;
  return 1;
}

/** "Mia, Lee and 2 more" (at most `max` names). */
export function playerNames(room: Room, max = 3): string {
  const names = room.players.map((p) => p.name);
  if (!names.length) return "";
  const shown = names.slice(0, max).join(", ");
  const rest = Math.max(room.playing, names.length) - Math.min(max, names.length);
  return rest > 0 ? t("cw_room_and_more", { names: shown, n: rest }) : shown;
}

/** The line a confirmation shows while friends play (null with no room, or nobody in it). */
export function roomWarning(room: Room | null): string | null {
  if (!room || room.playing <= 0) return null;
  const names = playerNames(room);
  const first = names ? t("cw_room_warn", { names }) : t("cw_room_warn_count", { n: room.playing });
  return room.hosted_from ? `${first} ${t("cw_room_warn_host", { device: room.hosted_from })}` : first;
}

/* ---- one store for the page: Home's card and every confirmation read the same answer ---- */

/** How often the room is read while something shows it. */
export const ROOM_POLL_MS = 10_000;

let current: Room | null = null;
const listeners = new Set<(r: Room | null) => void>();

function set(r: Room | null) {
  current = r;
  for (const fn of Array.from(listeners)) fn(r);
}

/** The last answer (null: none, or not read yet). */
export function currentRoom(): Room | null {
  return current;
}

/** Reads /api/room now. An older helper (no route) or an error reads as "no room". */
export function refreshRoom(): Promise<Room | null> {
  return call<{ room?: unknown }>("/api/room").then(
    (r) => {
      const room = parseRoom(r?.room);
      set(room);
      return room;
    },
    () => {
      set(null);
      return null;
    },
  );
}

/** Tests: forget what was read. */
export function resetRoomStore(room: Room | null = null): void {
  current = room;
  listeners.clear();
}

/** The room, read every ROOM_POLL_MS while the page shows the caller (`poll` false: no reads of its own). */
export function useRoom(poll = true): Room | null {
  const [room, setRoom] = useState<Room | null>(current);
  useEffect(() => {
    listeners.add(setRoom);
    setRoom(current);
    const stop = poll ? pollWhileVisible(() => refreshRoom(), ROOM_POLL_MS) : () => {};
    return () => {
      listeners.delete(setRoom);
      stop();
    };
  }, [poll]);
  return room;
}
