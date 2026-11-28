// Play Together (#641): the wire format between a friend's page (the guest) and the host's
// engine (engine/crates/ps5upload-engine/src/join.rs, socket /api/join/ws).
//
// Guest → engine: `{"hello":{invite?, resume?, name}}` first, then binary pad frames, `{"ping":t}`
// every second and `{"leave":1}`. Engine → guest: stages, status, rumble/light, bye, and the
// picture and sound as binary messages (the watch party's kinds, pad/watchPlayer.ts).

import type { PadState } from "./remotePlayStream";
import { lanPage } from "../join/lanHint";
import { parseRoomFragment } from "../join/roomFragment";

/** Why the host's engine did not let a guest in. Anything else reads as "refused". */
export type RefuseWhy =
  | "key"
  /** A place kept for this guest (the resume token) is gone. */
  | "expired"
  | "full"
  | "denied"
  /** The host did not answer the ask in time (not a no). */
  | "no_answer"
  | "closed"
  | "old_helper"
  | "not_playing"
  | "unreachable"
  | "address"
  | "slow_down"
  /** A resume to the stream's controller, which the host needed back, with no controller free. */
  | "seat_taken"
  /** Play Rooms (13.0.0): the room isn't taking players, the host isn't there to answer, a wrong
   *  password (or too many), the room went private, the guest's version is too old, or someone
   *  in the room already has their name (a suggestion comes with it). */
  | "locked"
  | "host_away"
  | "password"
  | "locked_out"
  | "not_listed"
  | "old"
  | "name_taken"
  | "refused";

/** How a joined guest's game ended, from the engine (`{"bye": …}`). */
export type ByeWhy =
  | "kicked"
  | "closed"
  | "host_stopped"
  /** The same place was opened in another tab or on another device. */
  | "replaced"
  /** The console no longer has this guest's controller (`{"bye": "lost"}`). */
  | "pad_lost"
  /** The host needed their stream's controller back and no other controller was free. */
  | "seat_taken"
  /** The host removed them from the room (Play Rooms' word for kicked). */
  | "removed";

/** Why an internet invite's connection could not be made (or was refused), join/rtcTransport. */
export type NetWhy =
  /** The answer's certificate is not the link's: someone in the middle, or an old link. */
  | "mismatch"
  /** ICE failed or did not finish in time: no direct path between the two networks. */
  | "no_direct"
  /** The hub has no such room (the invite closed, or the host is away). */
  | "no_room"
  /** The room (hub) or the host's engine has no space for another guest now. */
  | "net_full"
  /** The hub could not be reached, or dropped the page while connecting. */
  | "no_hub"
  /** The host's engine would not take this browser's offer. */
  | "offer"
  /** This browser has no WebRTC. */
  | "no_webrtc";

const NET_WHYS: readonly NetWhy[] = ["mismatch", "no_direct", "no_room", "net_full", "no_hub", "offer", "no_webrtc"];

/** The close code a failed internet connection closes with (its reason `net:<why>`). */
export const NET_CLOSE = 4100;

/** The NetWhy a transport's close carried, or null (an ordinary close or drop). */
export function netFailure(e: { code?: number; reason?: string } | null | undefined): NetWhy | null {
  if (!e || e.code !== NET_CLOSE || typeof e.reason !== "string" || !e.reason.startsWith("net:")) return null;
  const w = e.reason.slice(4) as NetWhy;
  return NET_WHYS.includes(w) ? w : null;
}

/** What a guest is doing on the host's console, as the host's engine sees it. */
export type GuestState = "asking" | "picking_user" | "playing" | "reconnecting" | "left";

/** `{"status": …}`, once a second while joined. */
export interface JoinStatus {
  player: number;
  state: GuestState | null;
  rttMs: number | null;
  host: string | null;
  blockSystem: boolean;
  /** On the host's stream's own controller (Player 1): playing as the host's PS5 user. */
  streamSeat: boolean;
  /** Seated in the PS5 profile the host chose for them: its name (no user to pick). */
  playsAs: string | null;
  /** The host chose a profile for them that couldn't be used: the TV asks who they are. */
  seatNote: string | null;
  /** The TV may be asking who they are: ✕ reaches it only after a move (d-pad or left stick),
   *  so the user it offers first (the host's own) is never picked by a plain ✕. */
  moveFirst: boolean;
  /** While they pick: the name of the host's own PS5 user (picking it ends the host's game). */
  hostUser: string | null;
  /** The host's game ended while they picked a user: most likely they picked the host's. */
  tookHost: boolean;
}

/** The picture as an internet connection's video track (join/rtcTransport): the browser's own
 *  stream, and its receiver for the numbers (getStats). */
export interface TrackMedia {
  stream: MediaStream;
  receiver: RTCRtpReceiver;
  /** The sound's track, when the answer has one (the engine says `audio` in `joined`). */
  audio?: RTCRtpReceiver;
}

export type ServerMessage =
  | { kind: "asking" }
  /** A resume held until the host plays again (`{"stage":"waiting","why":"host_stopped"}`). */
  | { kind: "waiting" }
  /** `track`: the pictures come on the internet connection's video track (join/rtcTransport). */
  | { kind: "joined"; player: number; token: string; hevc: boolean; track: boolean; audio: boolean }
  | { kind: "refused"; why: RefuseWhy; triesLeft?: number | null; suggest?: string | null }
  | { kind: "status"; status: JoinStatus }
  | { kind: "feedback"; rumble: [number, number] | null; light: [number, number, number] | null }
  | { kind: "bye"; why: ByeWhy }
  | { kind: "pong"; t: number }
  /** Play Rooms: the room as this guest sees it (every 2 s), a chat line, who talks, host mutes. */
  | { kind: "room"; room: GuestRoom }
  | { kind: "chat"; line: EngineChatLine }
  | { kind: "chat_history"; lines: EngineChatLine[] }
  | { kind: "voice"; voice: VoiceStateMsg }
  | { kind: "muted"; voice: boolean; chat: boolean }
  | { kind: "comms"; comms: CommsMsg }
  | { kind: "comms_refused"; refused: CommsRefused }
  /** A ready check started (seconds to answer). */
  | { kind: "ready"; leftS: number };

/** A chat line from the engine (API.md "Play Rooms chat and voice"). `x` > 1: the same line again
 *  from the same person; it replaces the line with this `id`. */
export interface EngineChatLine {
  id: number;
  sid: number | null;
  from: string;
  player: number | null;
  host: boolean;
  t: number;
  quick: number | null;
  text: string | null;
  x: number;
}

/** `{"comms": …}`: the room's chat and voice modes and whether this person accepted the notice. */
export interface CommsMsg {
  notice: number;
  accepted: boolean;
  chat: "off" | "quick" | "on" | null;
  voice: "off" | "ptt" | "open" | null;
  caps: string[];
}

/** One person in the voice state. */
export interface VoicePerson {
  sid: number;
  name: string;
  player: number | null;
  host: boolean;
  muted: boolean;
  paused: boolean;
  mic: boolean;
}

/** `{"voice": …}`: the full state (`full`), or only who speaks. */
export interface VoiceStateMsg {
  full: boolean;
  mode: "off" | "ptt" | "open" | null;
  you: number | null;
  mic: boolean;
  ptt: boolean;
  paused: "poor" | null;
  stepped: boolean;
  kbps: number;
  frameMs: number;
  speaking: number[];
  people: VoicePerson[];
}

/** `{"comms_refused": …}`. */
export interface CommsRefused {
  what: "chat" | "voice" | "mic" | "notice";
  why: string;
  notice: number | null;
  retryMs: number | null;
}

export function readEngineChatLine(v: unknown): EngineChatLine | null {
  if (!v || typeof v !== "object") return null;
  const c = v as Record<string, unknown>;
  const quick = typeof c.quick === "number" && Number.isInteger(c.quick) ? c.quick : null;
  const text = typeof c.text === "string" ? c.text : null;
  if (quick == null && !text) return null;
  return {
    id: numOr(c.id) ?? 0,
    sid: numOr(c.sid),
    from: typeof c.from === "string" ? c.from.slice(0, 40) : "",
    player: numOr(c.player),
    host: c.host === true,
    t: numOr(c.t) ?? 0,
    quick,
    text,
    x: Math.max(1, numOr(c.x) ?? 1),
  };
}

const modeOr = <T extends string>(v: unknown, list: readonly T[]): T | null => (typeof v === "string" && (list as readonly string[]).includes(v) ? (v as T) : null);

/** The room push to a guest (design 10.2): seats, their own ping and the host's PS5 link. */
export interface GuestRoom {
  name: string;
  size: number;
  players: Array<{ name: string; color: [number, number, number] | null; you: boolean; id: number | null; seat: number | null }>;
  you: { rttMs: number | null; tier: string | null } | null;
  hostLink: { rttMs: number | null; tier: string | null } | null;
  chat: "off" | "quick" | "on" | null;
  voice: "off" | "ptt" | "open" | null;
  muted: { voice: boolean; chat: boolean };
  caps: string[];
  /** The hub's lobby id, kept with a friend pass for "Alex's room is open". */
  lobby: string | null;
  visibility: "private" | "friends" | "public" | null;
}

const numOr = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const strOr = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

function readLinkish(v: unknown): { rttMs: number | null; tier: string | null } | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  return { rttMs: numOr(o.rtt_ms), tier: strOr(o.tier) };
}

/** `{"room": …}` to a guest, checked; null when it isn't one. */
export function readGuestRoom(v: unknown): GuestRoom | null {
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const o = v as Record<string, unknown>;
  const m = (o.muted ?? {}) as Record<string, unknown>;
  const chat = o.chat === "text" ? "on" : o.chat === "off" || o.chat === "quick" || o.chat === "on" ? o.chat : null;
  const voice = o.voice === "off" || o.voice === "ptt" || o.voice === "open" ? o.voice : null;
  const vis = o.visibility === "private" || o.visibility === "friends" || o.visibility === "public" ? o.visibility : null;
  return {
    name: typeof o.name === "string" ? o.name.slice(0, 40) : "",
    size: Math.max(1, Math.min(8, numOr(o.size) ?? 4)),
    players: (Array.isArray(o.players) ? o.players : [])
      .filter((p): p is Record<string, unknown> => !!p && typeof p === "object")
      .slice(0, 8)
      .map((p) => ({
        name: typeof p.name === "string" ? p.name.slice(0, 32) : "",
        color: Array.isArray(p.color) && p.color.length === 3 ? ([byte(p.color[0]), byte(p.color[1]), byte(p.color[2])] as [number, number, number]) : null,
        you: p.you === true,
        id: numOr(p.id),
        seat: numOr(p.seat) ?? numOr(p.player),
      })),
    you: readLinkish(o.you),
    hostLink: readLinkish(o.host_link),
    chat,
    voice,
    muted: { voice: m.voice === true || o.muted === true, chat: m.chat === true },
    caps: Array.isArray(o.caps) ? o.caps.filter((c): c is string => typeof c === "string") : [],
    lobby: strOr(o.lobby),
    visibility: vis,
  };
}

const REFUSE: ReadonlySet<string> = new Set([
  "key",
  "expired",
  "full",
  "denied",
  "no_answer",
  "closed",
  "old_helper",
  "not_playing",
  "unreachable",
  "slow_down",
  "address",
  "seat_taken",
  "locked",
  "host_away",
  "password",
  "locked_out",
  "not_listed",
  "old",
  "name_taken",
]);
const BYE: Readonly<Record<string, ByeWhy>> = {
  kicked: "kicked",
  host_stopped: "host_stopped",
  replaced: "replaced",
  lost: "pad_lost",
  seat_taken: "seat_taken",
  removed: "removed",
};
const GUEST_STATES: ReadonlySet<string> = new Set(["asking", "picking_user", "playing", "reconnecting", "left"]);

const byte = (v: unknown) => Math.max(0, Math.min(255, Math.round(Number(v) || 0)));

/** One text message from the engine, checked field by field (null: not one of ours). */
export function readServerMessage(text: string): ServerMessage | null {
  let v: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(text);
    if (!parsed || typeof parsed !== "object") return null;
    v = parsed as Record<string, unknown>;
  } catch {
    return null;
  }
  if (typeof v.pong === "number") return { kind: "pong", t: v.pong };
  if (v.stage === "asking") return { kind: "asking" };
  if (v.stage === "waiting") return { kind: "waiting" };
  if (v.stage === "joined" && typeof v.player === "number" && typeof v.token === "string") {
    return { kind: "joined", player: v.player, token: v.token, hevc: v.hevc === true, track: v.track === true, audio: v.audio === true };
  }
  if (v.stage === "refused") {
    const why = String(v.why ?? "");
    const nt = v.name_taken as { suggest?: unknown } | undefined;
    const triesLeft = numOr(v.tries_left);
    const suggest = strOr(v.suggest) ?? strOr(nt?.suggest);
    return {
      kind: "refused",
      why: REFUSE.has(why) ? (why as RefuseWhy) : "refused",
      ...(triesLeft != null ? { triesLeft } : {}),
      ...(suggest ? { suggest } : {}),
    };
  }
  // The hello's name is taken (design 3.5): `{"name_taken": {"suggest": "Sam (2)"}}`.
  if (v.name_taken && typeof v.name_taken === "object") {
    return { kind: "refused", why: "name_taken", suggest: strOr((v.name_taken as { suggest?: unknown }).suggest) };
  }
  if (v.room && typeof v.room === "object") {
    const room = readGuestRoom(v.room);
    if (room) return { kind: "room", room };
  }
  if (v.chat && typeof v.chat === "object") {
    const line = readEngineChatLine(v.chat);
    if (line) return { kind: "chat", line };
  }
  if (Array.isArray(v.chat_history)) {
    return { kind: "chat_history", lines: v.chat_history.map(readEngineChatLine).filter((l): l is EngineChatLine => !!l).slice(-50) };
  }
  if (v.comms && typeof v.comms === "object") {
    const o = v.comms as Record<string, unknown>;
    return {
      kind: "comms",
      comms: {
        notice: numOr(o.notice) ?? 1,
        accepted: o.accepted === true,
        chat: modeOr(o.chat, ["off", "quick", "on"] as const),
        voice: modeOr(o.voice, ["off", "ptt", "open"] as const),
        caps: Array.isArray(o.caps) ? o.caps.filter((c): c is string => typeof c === "string") : [],
      },
    };
  }
  if (v.comms_refused && typeof v.comms_refused === "object") {
    const o = v.comms_refused as Record<string, unknown>;
    const what = modeOr(o.what, ["chat", "voice", "mic", "notice"] as const) ?? "chat";
    return { kind: "comms_refused", refused: { what, why: typeof o.why === "string" ? o.why : "", notice: numOr(o.notice), retryMs: numOr(o.retry_ms) } };
  }
  if (v.voice && typeof v.voice === "object") {
    const o = v.voice as Record<string, unknown>;
    const full = "people" in o || "mode" in o;
    return {
      kind: "voice",
      voice: {
        full,
        mode: modeOr(o.mode, ["off", "ptt", "open"] as const),
        you: numOr(o.you),
        mic: o.mic === "on",
        ptt: o.ptt === true,
        paused: o.paused === "poor" ? "poor" : null,
        stepped: o.stepped === true,
        kbps: numOr(o.kbps) ?? 24,
        frameMs: numOr(o.frame_ms) ?? 20,
        speaking: Array.isArray(o.speaking) ? o.speaking.filter((x): x is number => typeof x === "number") : [],
        people: (Array.isArray(o.people) ? o.people : [])
          .filter((p): p is Record<string, unknown> => !!p && typeof p === "object" && typeof p.sid === "number")
          .slice(0, 16)
          .map((p) => ({
            sid: p.sid as number,
            name: typeof p.name === "string" ? p.name.slice(0, 32) : "",
            player: numOr(p.player),
            host: p.host === true,
            muted: p.muted === true,
            paused: p.paused === true || p.paused === "poor",
            mic: p.mic === true || p.mic === "on",
          })),
      },
    };
  }
  if (v.muted && typeof v.muted === "object") {
    const o = v.muted as Record<string, unknown>;
    return { kind: "muted", voice: o.voice === true, chat: o.chat === true };
  }
  if (v.ready && typeof v.ready === "object") {
    return { kind: "ready", leftS: numOr((v.ready as Record<string, unknown>).left_s) ?? 30 };
  }
  if (typeof v.bye === "string") {
    return { kind: "bye", why: Object.prototype.hasOwnProperty.call(BYE, v.bye) ? BYE[v.bye] : "closed" };
  }
  if (v.status && typeof v.status === "object") {
    const s = v.status as Record<string, unknown>;
    const state = typeof s.state === "string" && GUEST_STATES.has(s.state) ? (s.state as GuestState) : null;
    return {
      kind: "status",
      status: {
        player: typeof s.player === "number" ? s.player : 0,
        state,
        rttMs: typeof s.rtt_ms === "number" ? s.rtt_ms : null,
        host: typeof s.host === "string" && s.host ? s.host : null,
        blockSystem: s.block_system === true,
        streamSeat: s.stream_seat === true,
        playsAs: typeof s.plays_as === "string" && s.plays_as ? s.plays_as : null,
        seatNote: typeof s.seat_note === "string" && s.seat_note ? s.seat_note : null,
        moveFirst: s.move_first === true,
        hostUser: typeof s.host_user === "string" && s.host_user ? s.host_user : null,
        tookHost: s.took_host === true,
      },
    };
  }
  const rumble = Array.isArray(v.rumble) ? ([byte(v.rumble[0]), byte(v.rumble[1])] as [number, number]) : null;
  const light = Array.isArray(v.light)
    ? ([byte(v.light[0]), byte(v.light[1]), byte(v.light[2])] as [number, number, number])
    : null;
  if (rumble || light) return { kind: "feedback", rumble, light };
  return null;
}

/** A stick from the app's signed 16-bit range to the controller's byte (0 left/up, 128 centre). */
export function stickByte(v: number): number {
  const c = Math.max(-32768, Math.min(32767, Math.round(v)));
  return (c + 32768) >> 8;
}

/** The touchpad in the app is 1920×942 (lib/remotePlayStream touchAt); a DualSense's is
 *  1920×1080, which the frame carries. */
export function touchToDualSense([x, y]: [number, number]): [number, number] {
  const cx = Math.max(0, Math.min(1919, Math.round(x)));
  const cy = Math.max(0, Math.min(1079, Math.round((y * 1079) / 941)));
  return [cx, cy];
}

/**
 * One pad frame, exactly:
 * `[0x01][u32 LE seq][u32 LE buttons][u8 lx ly rx ry l2 r2][u8 ntouch][ntouch × (u8 id, u16 LE x,
 * u16 LE y)][optional 0x02 + 6 × f32 LE: gyro x y z (rad/s), accel x y z (g)]`.
 * Buttons are the app's PadButton bits (Cross 1 … PS 32768).
 */
export function encodePadFrame(seq: number, pad: PadState): Uint8Array<ArrayBuffer> {
  const touches: Array<[number, number, number]> = pad.touch ? [[0, ...touchToDualSense(pad.touch)]] : [];
  const motion = pad.motion ?? null;
  const size = 1 + 4 + 4 + 6 + 1 + touches.length * 5 + (motion ? 1 + 24 : 0);
  const buf = new ArrayBuffer(size);
  const dv = new DataView(buf);
  let o = 0;
  dv.setUint8(o++, 0x01);
  dv.setUint32(o, seq >>> 0, true);
  o += 4;
  dv.setUint32(o, pad.buttons >>> 0, true);
  o += 4;
  for (const v of [pad.lx, pad.ly, pad.rx, pad.ry]) dv.setUint8(o++, stickByte(v));
  dv.setUint8(o++, byte(pad.l2));
  dv.setUint8(o++, byte(pad.r2));
  dv.setUint8(o++, touches.length);
  for (const [id, x, y] of touches) {
    dv.setUint8(o++, id);
    dv.setUint16(o, x, true);
    o += 2;
    dv.setUint16(o, y, true);
    o += 2;
  }
  if (motion) {
    dv.setUint8(o++, 0x02);
    // lib/motion reads mrad/s and thousandths of g.
    for (const v of [...motion.gyro, ...motion.accel]) {
      dv.setFloat32(o, v / 1000, true);
      o += 4;
    }
  }
  return new Uint8Array(buf);
}

/** The wait before reconnect attempt `n` (0 first): 250 ms, doubling, at most 4 s. */
export function backoffMs(n: number): number {
  return Math.min(4000, 250 * 2 ** Math.max(0, Math.min(n, 10)));
}

/** How long a dropped guest keeps its place (the engine holds the slot as long). */
export const RESUME_WINDOW_MS = 60_000;

/** How long the guests keep their places after the host stops playing (join.rs
 *  HOST_RESTART_WINDOW): they wait for the host to play again. */
export const HOST_RESTART_WINDOW_MS = 60_000;

/** An internet invite: the hub's room, the invite key, the host's DTLS fingerprint and the hub. */
export interface InternetInvite {
  room: string;
  key: string;
  fp: string;
  hub: string;
}

/** A SHA-256 DTLS fingerprint as a link carries it (32 hex pairs with colons), upper case; null
 *  when it is not one. */
export function fpOf(v: string | null | undefined): string | null {
  return v && /^[0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){31}$/.test(v) ? v.toUpperCase() : null;
}

/** The internet invite in a link's fragment (`location.hash`), or null when it is not one. */
export function parseInternetFragment(hash: string): InternetInvite | null {
  const p = new URLSearchParams(hash.replace(/^#/, ""));
  const room = p.get("room") ?? "";
  const key = p.get("k") ?? "";
  const fp = p.get("fp") ?? "";
  const hub = p.get("hub") ?? "";
  if (!/^[A-Za-z0-9_-]{22}$/.test(room)) return null;
  if (!/^[A-Za-z0-9_-]{8,128}$/.test(key)) return null;
  if (!fpOf(fp)) return null;
  // A host name or address, an optional port: nothing that could add a path, user or scheme.
  if (!/^([A-Za-z0-9.-]{1,253}|\[[0-9A-Fa-f:.]{2,45}\])(:\d{1,5})?$/.test(hub)) return null;
  return { room, key, fp: fp.toUpperCase(), hub };
}

/** A PS5 waiting for an invite (PS5Upload Play's QR code on the TV, apps/play/docs/EASY_JOIN.md):
 *  its hub room, the nonce the TV shows and the hub. */
export interface Ps5Target {
  room: string;
  t: string;
  hub: string;
}

/** The PS5 in a QR code's fragment (`#ps5=<room>&t=<nonce>&hub=<host[:port]>`), or null. */
export function parsePs5Fragment(hash: string): Ps5Target | null {
  const p = new URLSearchParams(hash.replace(/^#/, ""));
  const room = p.get("ps5") ?? "";
  const t = p.get("t") ?? "";
  const hub = p.get("hub") ?? "";
  if (!/^[A-Za-z0-9_-]{22}$/.test(room) || !/^[A-Za-z0-9_-]{8,32}$/.test(t)) return null;
  // The hub as parseInternetFragment takes it.
  if (!/^([A-Za-z0-9.-]{1,253}|\[[0-9A-Fa-f:.]{2,45}\])(:\d{1,5})?$/.test(hub)) return null;
  return { room, t, hub };
}

/** A Play Together link a friend shared: `http://<computer>:<port>/join#i=<key>[&n=<name>]`, or
 *  since 12.1 the one link for everywhere, `https://ps5upload.phantomptr.com/join/#room=…&k=…`
 *  (its `net`, with the host's own page as `lan` when the link names it). */
export interface JoinLink {
  /** `http://<computer>:<port>` */
  origin: string;
  /** The invite key (a link or QR code): no Allow needed for it to be valid… the host still allows. */
  invite: string | null;
  /** The short code a friend typed instead (`K7M2QX`), matched to a host playing nearby. */
  code?: string | null;
  /** The host's name, when the link carries it. */
  name: string | null;
  /** An internet invite: joined peer to peer through the hub (join/rtcTransport). */
  net?: InternetInvite;
  /** The host's own page on their Wi-Fi (`lan`) and on Tailscale (`ts`), when the internet link
   *  names them: where the app signals without the hub (join/pathRace). */
  lan?: string | null;
  ts?: string | null;
  /** A Wi-Fi link's engine certificate (`&fp=`, since 12.4): its WebRTC join checks the answer
   *  against it (join/pathRace). */
  fp?: string | null;
  /** Play Rooms: a room found in Rooms, Friends or Play again (no invite): its hub lobby, how it
   *  was found, the room key and what the knock carries. */
  room?: RoomJoin;
}

/** A room to knock on (design 7.0, 9.3): the same join flow as an invite, the card supplies the
 *  room instead of an invite key. */
export interface RoomJoin {
  /** The hub's lobby room id. */
  lobby: string;
  /** The hub's address as a link carries it (`hub.phantomptr.com`). */
  hub: string;
  via: "rooms" | "friends" | "nearby" | "again";
  /** The room key (for sealing a password or a friend pass); null: none. */
  pk: string | null;
  password: string | null;
  /** A friend pass for Play again / Friends. */
  pass?: string | null;
  /** The host's tag, for "last time" and hiding. */
  tag?: string;
}

/** The link pasted into the app (or scanned), or null when it is not one. */
export function parseJoinLink(text: string): JoinLink | null {
  const raw = text.trim();
  if (!raw) return null;
  let u: URL;
  try {
    u = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) ? raw : `http://${raw}`);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  const path = /^(.*)\/join\/?$/.exec(u.pathname);
  if (!path) return null;
  // The one link for everywhere (12.1): its origin is the host's own page when the link names it
  // (an app on the same Wi-Fi can fall back to it), else the hub's.
  const net = parseInternetFragment(u.hash);
  if (net) {
    const lan = lanPage(u.hash, "join");
    const name = new URLSearchParams(u.hash.replace(/^#/, "")).get("n")?.trim() || null;
    const ts = lanPage(u.hash, "join", "ts");
    return {
      origin: lan ? lan.replace(/\/join\/?$/, "") : `https://${net.hub}`,
      invite: net.key,
      name,
      code: null,
      net,
      lan,
      ts,
    };
  }
  // A Play Room handed over from the website's Rooms page: knock on its lobby (no invite).
  const room = parseRoomFragment(u.hash);
  if (room) {
    return {
      origin: `https://${room.hub}`,
      invite: null,
      name: room.host,
      code: null,
      room: { lobby: room.lobby, hub: room.hub, via: room.via, pk: null, password: null, tag: room.tag },
    };
  }
  const hash = new URLSearchParams(u.hash.replace(/^#/, ""));
  const invite = hash.get("i") ?? "";
  if (!/^[A-Za-z0-9_-]{8,128}$/.test(invite)) return null;
  const name = hash.get("n")?.trim() || null;
  // With the path the engine is served under (PS5UPLOAD_BASE_URL), if any.
  return { origin: `${u.origin}${path[1]}`, invite, name, code: null, fp: fpOf(hash.get("fp")) };
}

/** "Open in the PS5Upload app": an invite link as the ps5upload:// link the app opens. */
export function joinAppLink(link: string): string {
  return `ps5upload://join?link=${encodeURIComponent(link)}`;
}

/** The invite link inside a `ps5upload://join?link=…` link, or null when it isn't one. */
export function joinLinkFromAppLink(url: string): string | null {
  let u: URL;
  try {
    u = new URL(url.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "ps5upload:") return null;
  // "join" is the host as written; a system that hands it over as `ps5upload:/join` has it as the path.
  const what = (u.host || u.pathname.replace(/^\/+/, "")).replace(/\/+$/, "").toLowerCase();
  if (what !== "join") return null;
  const link = (u.searchParams.get("link") ?? "").trim();
  return parseJoinLink(link) ? link : null;
}

/** A short code as typed ("k7m 2qx", "K7M-2QX") → "K7M2QX"; null when it can't be one. */
export function normalizeCode(text: string): string | null {
  const c = text.toUpperCase().replace(/[\s-]/g, "");
  return /^[A-Z0-9]{6}$/.test(c) ? c : null;
}

/** "K7M2QX" → "K7M-2QX" (easier to read out). */
export function formatCode(code: string): string {
  const c = code.toUpperCase().replace(/[\s-]/g, "");
  return c.length === 6 ? `${c.slice(0, 3)}-${c.slice(3)}` : c;
}

/** A host playing nearby (mesh presence) as a link: no invite, so the host is asked. */
export function linkFromSocket(ws: string, name: string | null, code: string | null = null): JoinLink | null {
  try {
    const u = new URL(ws);
    if (u.protocol !== "ws:" && u.protocol !== "wss:") return null;
    const prefix = u.pathname.replace(/\/api\/join\/ws\/?$/, "").replace(/\/$/, "");
    return { origin: `${u.protocol === "wss:" ? "https:" : "http:"}//${u.host}${prefix}`, invite: null, code, name };
  } catch {
    return null;
  }
}

/** The guest socket on the host's engine for a link. */
export function joinSocketUrl(link: Pick<JoinLink, "origin">): string {
  return `${link.origin.replace(/^http/, "ws")}/api/join/ws`;
}


/** The light-bar colour a player number has on a DualSense (Player 1 blue, 2 red, 3 green, 4 pink). */
export function playerColor(player: number): [number, number, number] {
  switch (player) {
    case 1:
      return [0, 0, 255];
    case 2:
      return [255, 0, 0];
    case 3:
      return [0, 255, 0];
    default:
      return [255, 0, 255];
  }
}
