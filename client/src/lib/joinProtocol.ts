// Play Together (#641): the wire format between a friend's page (the guest) and the host's
// engine (engine/crates/ps5upload-engine/src/join.rs, socket /api/join/ws).
//
// Guest → engine: `{"hello":{invite?, resume?, name}}` first, then binary pad frames, `{"ping":t}`
// every second and `{"leave":1}`. Engine → guest: stages, status, rumble/light, bye, and the
// picture and sound as binary messages (the watch party's kinds, pad/watchPlayer.ts).

import type { PadState } from "./remotePlayStream";
import { lanPage } from "../join/lanHint";

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
  | "seat_taken";

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
  | { kind: "refused"; why: RefuseWhy }
  | { kind: "status"; status: JoinStatus }
  | { kind: "feedback"; rumble: [number, number] | null; light: [number, number, number] | null }
  | { kind: "bye"; why: ByeWhy }
  | { kind: "pong"; t: number };

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
]);
const BYE: Readonly<Record<string, ByeWhy>> = {
  kicked: "kicked",
  host_stopped: "host_stopped",
  replaced: "replaced",
  lost: "pad_lost",
  seat_taken: "seat_taken",
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
    return { kind: "refused", why: REFUSE.has(why) ? (why as RefuseWhy) : "refused" };
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

/** Delay in ms as a colour: under 60 ms good, under 120 ms fair, else slow. */
export function delayTone(ms: number | null | undefined): "good" | "fair" | "slow" | "none" {
  if (ms == null || !Number.isFinite(ms)) return "none";
  if (ms < 60) return "good";
  if (ms < 120) return "fair";
  return "slow";
}

/** A delay a friend notices: the host is told how to make it smoother. */
export const HIGH_DELAY_MS = 120;

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
