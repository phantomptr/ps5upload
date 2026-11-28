// Play Rooms on this device: its own random id (the source of the 4-character tag hosts see),
// rooms hidden here, and the hosts played with before (their lobby ids for "Alex's room is
// open", the last round trips for "last time 34 ms"). Kept only in this browser or app
// (localStorage); 90 days, 50 hosts at most. Nothing here leaves the device except the id, in
// the hello, and lobby ids, in the hub's status lookup.

import { b64url } from "./knock";
import type { HostHistory } from "./rooms";
import { safeGetItem, safeSetItem } from "../safeStorage";

const DEVICE = "ps5upload.join.device";
const PLAYED = "ps5upload.rooms.played";
const HIDDEN = "ps5upload.rooms.hidden";
const KEEP_MS = 90 * 24 * 3600_000;

/** This device's id for rooms (16 random bytes, base64url), made once. The person can reset it
 *  (a new tag); hosts then see a new device. */
export function deviceId(rand: (n: number) => Uint8Array = (n) => crypto.getRandomValues(new Uint8Array(n))): string {
  const have = safeGetItem(DEVICE);
  if (have && /^[A-Za-z0-9_-]{22}$/.test(have)) return have;
  const id = b64url(rand(16));
  safeSetItem(DEVICE, id);
  return id;
}

export function resetDeviceId(): void {
  safeSetItem(DEVICE, "");
}

export interface PlayedHost {
  /** The host's lobby id (from `room.lobby`), for the friends lookup. */
  lobby: string | null;
  host: string;
  tag: string;
  rtts: number[];
  at: number;
}

function readJson<T>(key: string, fallback: T): T {
  try {
    const v = JSON.parse(safeGetItem(key) ?? "null") as unknown;
    return (v ?? fallback) as T;
  } catch {
    return fallback;
  }
}

export function playedHosts(now: number = Date.now()): PlayedHost[] {
  const list = readJson<PlayedHost[]>(PLAYED, []);
  return (Array.isArray(list) ? list : [])
    .filter((p) => p && typeof p.host === "string" && typeof p.at === "number" && now - p.at < KEEP_MS)
    .slice(0, 50);
}

/** Remembers a game with a host: its lobby id (when the room gave one) and a round trip. */
export function rememberPlayed(p: { lobby: string | null; host: string; tag: string; rttMs: number | null }, now: number = Date.now()): void {
  const list = playedHosts(now);
  const key = (x: PlayedHost) => (p.lobby && x.lobby ? x.lobby === p.lobby : x.host === p.host && x.tag === p.tag);
  const old = list.find(key);
  const rtts = [...(old?.rtts ?? []), ...(p.rttMs != null ? [Math.round(p.rttMs)] : [])].slice(-3);
  const next: PlayedHost = { lobby: p.lobby ?? old?.lobby ?? null, host: p.host, tag: p.tag || old?.tag || "", rtts, at: now };
  safeSetItem(PLAYED, JSON.stringify([next, ...list.filter((x) => !key(x))].slice(0, 50)));
}

/** History for a card (by host tag), for "last time N ms". */
export function historyFor(tag: string, list: PlayedHost[] = playedHosts()): HostHistory | null {
  if (!tag) return null;
  const p = list.find((x) => x.tag === tag);
  return p ? { tag, rtts: p.rtts } : null;
}

/** Rooms and hosts hidden on this device ("Hide this room" / "Hide this person": by room id or
 *  `tag:<tag>`). */
export function hiddenRooms(): Set<string> {
  const v = readJson<string[]>(HIDDEN, []);
  return new Set(Array.isArray(v) ? v.filter((x) => typeof x === "string").slice(0, 500) : []);
}

export function hideRoom(id: string): void {
  const s = hiddenRooms();
  s.add(id);
  safeSetItem(HIDDEN, JSON.stringify([...s].slice(-500)));
}
