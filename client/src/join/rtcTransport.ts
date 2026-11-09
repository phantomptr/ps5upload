// Play Together over the internet: the guest page reaches the host's engine peer to peer over
// WebRTC, with the signalling hub (engine/crates/ps5upload-hub/PROTOCOL.md) in between only to swap
// the offer, the answer and ICE candidates.
//
// The session (lib/joinClient) talks to a WebSocket: this module gives it one, `rtcTransport`, a
// class shaped like WebSocket whose "socket" is the "join" data channel (the phone link's page,
// src/pad, uses it the same way with a "pad" channel for lib/padClient). Every connect (the first
// one and each resume after a drop) makes a new one: a new hub connection, a new peer connection,
// and the session's hello (with the resume token when it has one) once the channel opens. On the
// channel the messages of /api/join/ws go unchanged (join.rs); binary ones in pieces
// (join_net/chunk.rs, `Reassembler` and `framePieces` here).
//
// A connection that cannot be made closes with a CloseEvent whose reason is `net:<why>`
// (`netFailure`), which the session shows as the reason the guest is not in the game.

import { NET_CLOSE, netFailure, type InternetInvite, type NetWhy, type TrackMedia } from "../lib/joinProtocol";
import { selectedPath, type NetPath } from "./netPath";

/** What an internet invite link's fragment carries:
 *  `#room=<id>&k=<invite key>&fp=<SHA-256, upper-case hex with colons>&hub=<host[:port]>`. */
// The invite's shape and parser live with the other link parsers (lib/joinProtocol).
export { parseInternetFragment, type InternetInvite } from "../lib/joinProtocol";

/** The hub's socket: `wss://<hub>/ws`, or `ws://` to this computer only (a hub under test). */
export function hubSocketUrl(hub: string): string {
  const host = hub.replace(/:\d+$/, "").replace(/^\[|\]$/g, "");
  const local = host === "localhost" || host === "127.0.0.1" || host === "::1";
  return `${local ? "ws" : "wss"}://${hub}/ws`;
}

/** The SHA-256 DTLS fingerprint an SDP carries (`a=fingerprint:sha-256 <X>`), upper case; null
 *  when it has none, or more than one that differ (a peer must have exactly one certificate). */
export function sdpFingerprint(sdp: string): string | null {
  const found = new Set<string>();
  for (const m of sdp.matchAll(/^a=fingerprint:sha-256[ \t]+([0-9A-Fa-f:]+)[ \t]*\r?$/gim)) found.add(m[1].toUpperCase());
  // Another hash's fingerprint instead of SHA-256 is not one this link can check.
  if (/^a=fingerprint:(?!sha-256[ \t])/im.test(sdp)) return null;
  return found.size === 1 ? [...found][0] : null;
}

/** The answer's certificate is the one the link names (case does not matter). */
export function fingerprintMatches(sdp: string, fp: string): boolean {
  const got = sdpFingerprint(sdp);
  return got !== null && got === fp.toUpperCase();
}

// ── Framing (join_net/chunk.rs) ─────────────────────────────────────────────────────────────────

/** The largest data-channel message either side sends, header included. */
export const DC_MAX = 16 * 1024;
export const LAST = 0x00;
export const MORE = 0x01;
export const PING = 0x02;
export const PONG = 0x03;
/** The largest whole message this page takes (a keyframe is far under). */
export const MESSAGE_MAX = 16 * 1024 * 1024;

/** `msg` as data-channel messages, each at most DC_MAX bytes (an empty one is one `[0x00]`). */
export function framePieces(msg: Uint8Array): Uint8Array<ArrayBuffer>[] {
  const room = DC_MAX - 1;
  const n = Math.max(1, Math.ceil(msg.length / room));
  const out: Uint8Array<ArrayBuffer>[] = [];
  for (let i = 0; i < n; i++) {
    const part = msg.subarray(i * room, Math.min(msg.length, (i + 1) * room));
    const v = new Uint8Array(part.length + 1);
    v[0] = i + 1 === n ? LAST : MORE;
    v.set(part, 1);
    out.push(v);
  }
  return out;
}

export type Piece =
  | { kind: "partial" }
  | { kind: "message"; data: ArrayBuffer }
  | { kind: "ping"; data: Uint8Array }
  | { kind: "pong"; data: Uint8Array }
  | { kind: "error"; why: "empty" | "header" | "ping_size" | "too_big" };

/** Puts the pieces of binary messages back together. */
export class Reassembler {
  private parts: Uint8Array[] = [];
  private size = 0;

  constructor(private readonly max = MESSAGE_MAX) {}

  push(dc: ArrayBuffer | Uint8Array): Piece {
    const b = dc instanceof Uint8Array ? dc : new Uint8Array(dc);
    if (b.length === 0) return { kind: "error", why: "empty" };
    const head = b[0];
    const rest = b.subarray(1);
    if (head === PING || head === PONG) {
      if (rest.length !== 8) return { kind: "error", why: "ping_size" };
      return { kind: head === PING ? "ping" : "pong", data: rest.slice() };
    }
    if (head !== LAST && head !== MORE) return { kind: "error", why: "header" };
    if (this.size + rest.length > this.max) {
      this.parts = [];
      this.size = 0;
      return { kind: "error", why: "too_big" };
    }
    // A copy: the browser may reuse the event's buffer.
    this.parts.push(rest.slice());
    this.size += rest.length;
    if (head === MORE) return { kind: "partial" };
    const out = new Uint8Array(this.size);
    let o = 0;
    for (const p of this.parts) {
      out.set(p, o);
      o += p.length;
    }
    this.parts = [];
    this.size = 0;
    return { kind: "message", data: out.buffer };
  }
}

/** The answer to a ping: the same 8 bytes behind 0x03. */
export function pong(ping: Uint8Array): Uint8Array<ArrayBuffer> {
  const v = new Uint8Array(9);
  v[0] = PONG;
  v.set(ping.subarray(0, 8), 1);
  return v;
}

// ── Failures ────────────────────────────────────────────────────────────────────────────────────

export { NET_CLOSE, netFailure, type NetWhy };

// ── The transport ───────────────────────────────────────────────────────────────────────────────

/** Public STUN servers the page asks (the engine asks the same by default; no TURN). */
export const STUN_SERVERS = ["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"];
/** ICE must reach "connected" (and the channel open) this soon, or the networks can't meet. */
export const ICE_TIMEOUT_MS = 15_000;
/** The hub closed with 1001 (going away: it is restarting for an update) before the host
 *  answered: try again after this long, for at most HUB_BACK_FOR_MS. The hosts reconnect and open
 *  their rooms again within seconds, so a `no_room` in that time means "not back yet". */
export const HUB_BACK_WAIT_MS = 1_500;
export const HUB_BACK_FOR_MS = 20_000;

/** What `new` gives: the parts of WebSocket the session uses. */
export interface JoinSocket {
  readyState: number;
  binaryType: string;
  onopen: ((e: Event) => void) | null;
  onmessage: ((e: MessageEvent) => void) | null;
  onclose: ((e: CloseEvent) => void) | null;
  onerror: ((e: Event) => void) | null;
  send(data: string | ArrayBufferLike | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
}

/** A WebSocket-shaped class: the session's socket (`JoinClientOptions.WebSocketImpl`). */
export type JoinTransport = typeof WebSocket;

export interface RtcTransportOptions {
  /** Tests: fakes for the browser's classes, the clock. */
  WebSocketImpl?: typeof WebSocket;
  PeerConnection?: typeof RTCPeerConnection;
  iceTimeoutMs?: number;
  /** Diagnostics (the console): what the connection is doing. */
  log?: (m: string) => void;
  /** The data channel's label: "join" (Play Together, the default) or "pad" (the phone link's
   *  controller and watch page, engine pad_link.rs). The engine gives each room's channel only its
   *  own kind's session. */
  label?: "join" | "pad";
  /** Offer to take the picture as a video track (Play Together): the engine answers it when it
   *  can and its host allows it, and says so in `joined` (`track`); otherwise the pictures stay
   *  on the channel. */
  video?: boolean;
  /** Where to signal: the hub's socket (the default), or one that speaks its guest side for this
   *  room alone, the host's own engine (`/api/join/signal`, engine join_net/local_signal.rs), reached
   *  through this app's engine on the same Wi-Fi or tailnet. The answer is checked against the
   *  link's `fp` the same way either way. */
  signalUrl?: string;
  /** A home-network join (a Wi-Fi link, no room): the host's engine opens a room for this
   *  connection (`{"t":"join","lan":true}`). The answer is checked against `inv.fp` when the link
   *  names one; a link without (an older host, a typed address) takes the one certificate the
   *  answer has, no less than its plain WebSocket would (that has none). */
  lan?: boolean;
}

/** The video codecs the track may use, H.264 first (H.265 only where this browser has it: the
 *  engine takes it only for an H.265 session), with their retransmission entries. */
export function trackCodecs(caps: RTCRtpCapabilities | null | undefined): RTCRtpCodec[] {
  const c = caps?.codecs ?? [];
  const of = (m: string) => c.filter((x) => x.mimeType.toLowerCase() === m);
  return [...of("video/h264"), ...of("video/h265"), ...of("video/rtx")];
}

type HubMsg = { t?: string; gid?: string; why?: string; from?: string; data?: Record<string, unknown> };

/** The WebSocket-shaped transport for an internet invite. Each instance is one attempt. */
export function rtcTransport(inv: InternetInvite, o: RtcTransportOptions = {}): JoinTransport {
  const WS = o.WebSocketImpl ?? (typeof WebSocket === "undefined" ? undefined : WebSocket);
  const PC = o.PeerConnection ?? (typeof RTCPeerConnection === "undefined" ? undefined : RTCPeerConnection);
  const iceTimeout = o.iceTimeoutMs ?? ICE_TIMEOUT_MS;
  const log = o.log ?? (() => {});

  class RtcSocket implements JoinSocket {
    static readonly CONNECTING = 0;
    static readonly OPEN = 1;
    static readonly CLOSING = 2;
    static readonly CLOSED = 3;
    readonly CONNECTING = 0;
    readonly OPEN = 1;
    readonly CLOSING = 2;
    readonly CLOSED = 3;
    readyState = 0;
    binaryType = "arraybuffer";
    onopen: ((e: Event) => void) | null = null;
    onmessage: ((e: MessageEvent) => void) | null = null;
    onclose: ((e: CloseEvent) => void) | null = null;
    onerror: ((e: Event) => void) | null = null;

    private hub: WebSocket | null = null;
    private pc: RTCPeerConnection | null = null;
    private dc: RTCDataChannel | null = null;
    private timer: ReturnType<typeof setTimeout> | null = null;
    private readonly rx = new Reassembler();
    private answered = false;
    /** Candidates from the host that came before the answer. */
    private early: RTCIceCandidateInit[] = [];
    /** Set while waiting for a restarting hub: until when it is worth trying again. */
    private hubBackUntil = 0;
    /** The video track, once the answer brought it (the session reads it when joined). */
    media: TrackMedia | null = null;

    constructor(_url?: string) {
      // Asynchronous like a WebSocket's own events: the session sets its handlers first.
      setTimeout(() => this.begin(), 0);
    }

    private begin(): void {
      if (this.readyState !== 0) return;
      if (!PC || !WS) {
        this.fail("no_webrtc");
        return;
      }
      let hub: WebSocket;
      try {
        hub = new WS(o.signalUrl ?? hubSocketUrl(inv.hub));
      } catch {
        this.fail("no_hub");
        return;
      }
      this.hub = hub;
      hub.onopen = () => hub.send(JSON.stringify(o.lan ? { v: 1, t: "join", lan: true } : { v: 1, t: "join", room: inv.room }));
      hub.onmessage = (e: MessageEvent) => {
        if (typeof e.data !== "string") return;
        let m: HubMsg;
        try {
          m = JSON.parse(e.data) as HubMsg;
        } catch {
          return;
        }
        this.onHub(m);
      };
      hub.onclose = (e?: CloseEvent) => {
        if (this.hub !== hub) return;
        this.hub = null;
        // Once the channel is open the hub is not needed.
        if (this.readyState !== 0) return;
        // With the host's answer in, ICE may still connect without the hub: let it try (the
        // ICE timeout still ends it).
        if (this.answered) return;
        // The hub restarting (an update): try again once it is back.
        if (e?.code === 1001 && this.hubRestarting()) return;
        this.fail("no_hub");
      };
      hub.onerror = () => {
        // onclose follows.
      };
    }

    private signal(data: Record<string, unknown>): void {
      const hub = this.hub;
      if (hub && hub.readyState === 1) hub.send(JSON.stringify({ t: "signal", to: "host", data }));
    }

    private onHub(m: HubMsg): void {
      if (this.readyState !== 0) return;
      switch (m.t) {
        case "joined":
          log(`hub: joined room as ${m.gid ?? "?"}`);
          void this.offer();
          return;
        case "signal":
          if (m.from === "host" && m.data && typeof m.data === "object") void this.onSignal(m.data);
          return;
        case "closed":
          this.fail("no_room");
          return;
        case "error":
          log(`hub: error ${m.why ?? "?"}`);
          // Back from a restart before the host is: its room opens again in a moment.
          if (m.why === "no_room" && this.hubBackUntil && this.hubRestarting()) return;
          this.fail(m.why === "no_room" ? "no_room" : m.why === "room_full" ? "net_full" : "no_hub");
          return;
      }
    }

    private async offer(): Promise<void> {
      if (!PC) return;
      let pc: RTCPeerConnection;
      try {
        pc = new PC({ iceServers: [{ urls: STUN_SERVERS }] });
      } catch {
        this.fail("no_webrtc");
        return;
      }
      this.pc = pc;
      // Reliable and ordered, for a pad too: a release lost or overtaken would leave a button held,
      // and binary messages come in pieces that must arrive in order (join_net/chunk.rs).
      const dc = pc.createDataChannel(o.label ?? "join", { ordered: true });
      dc.binaryType = "arraybuffer";
      this.dc = dc;
      dc.onopen = () => this.opened(dc);
      dc.onmessage = (e: MessageEvent) => this.fromChannel(e.data);
      dc.onclose = () => {
        if (this.dc !== dc) return;
        this.dropped();
      };
      if (o.video && pc.addTransceiver) {
        try {
          const t = pc.addTransceiver("video", { direction: "recvonly" });
          const codecs = trackCodecs(globalThis.RTCRtpReceiver?.getCapabilities?.("video"));
          if (codecs.length && t.setCodecPreferences) t.setCodecPreferences(codecs);
        } catch {
          // No track here: the pictures come on the channel.
        }
        // The sound beside the picture as an Opus track (#720): the browser's jitter buffer, loss
        // concealment and lip sync play it. An engine without one answers it inactive; the sound
        // then stays on the channel.
        try {
          pc.addTransceiver("audio", { direction: "recvonly" });
        } catch {
          // No audio track here: the sound comes on the channel.
        }
        pc.ontrack = (e: RTCTrackEvent) => {
          if (this.pc !== pc) return;
          // One stream for both, whatever stream ids the answer gave them (one <video> plays both).
          const stream = this.media?.stream ?? e.streams[0] ?? new MediaStream();
          if (!stream.getTracks().includes(e.track)) stream.addTrack(e.track);
          this.media =
            e.track.kind === "audio"
              ? { stream, receiver: this.media?.receiver ?? e.receiver, audio: e.receiver }
              : { stream, receiver: e.receiver, ...(this.media?.audio ? { audio: this.media.audio } : {}) };
        };
      }
      pc.onicecandidate = (e: RTCPeerConnectionIceEvent) => {
        const c = e.candidate;
        // The end of gathering (null or empty) needs nothing: the engine takes what came.
        if (c && c.candidate) this.signal({ type: "candidate", candidate: c.candidate, sdpMid: c.sdpMid ?? "0" });
      };
      pc.oniceconnectionstatechange = () => {
        if (this.pc !== pc) return;
        const s = pc.iceConnectionState;
        log(`ice: ${s}`);
        if (s === "failed") {
          if (this.readyState === 0) this.fail("no_direct");
          else this.dropped();
        }
      };
      this.timer = setTimeout(() => {
        this.timer = null;
        if (this.readyState === 0) this.fail("no_direct");
      }, iceTimeout);
      try {
        const offer = await pc.createOffer();
        await pc.setLocalDescription(offer);
        if (this.pc !== pc) return;
        this.signal({ type: "offer", sdp: pc.localDescription?.sdp ?? offer.sdp ?? "" });
      } catch {
        this.fail("no_webrtc");
      }
    }

    private async onSignal(d: Record<string, unknown>): Promise<void> {
      const pc = this.pc;
      if (d.type === "bye") {
        this.fail(d.why === "full" ? "net_full" : "offer");
        return;
      }
      if (!pc) return;
      if (d.type === "answer") {
        if (this.answered || typeof d.sdp !== "string") return;
        // Only the host's own engine: its certificate is the one the link carries (a home-network
        // link without one: exactly one certificate, whatever it is).
        if (inv.fp ? !fingerprintMatches(d.sdp, inv.fp) : sdpFingerprint(d.sdp) === null) {
          log("answer: fingerprint does not match the link");
          this.fail("mismatch");
          return;
        }
        this.answered = true;
        try {
          await pc.setRemoteDescription({ type: "answer", sdp: d.sdp });
          for (const c of this.early.splice(0)) await pc.addIceCandidate(c).catch(() => {});
        } catch {
          this.fail("offer");
        }
        return;
      }
      if (d.type === "candidate" && typeof d.candidate === "string" && d.candidate) {
        const c: RTCIceCandidateInit = {
          candidate: d.candidate,
          sdpMid: typeof d.sdpMid === "string" ? d.sdpMid : "0",
          sdpMLineIndex: typeof d.sdpMLineIndex === "number" ? d.sdpMLineIndex : 0,
        };
        if (!this.answered) this.early.push(c);
        else await pc.addIceCandidate(c).catch(() => {});
      }
    }

    private opened(dc: RTCDataChannel): void {
      if (this.dc !== dc || this.readyState !== 0) return;
      this.clearTimer();
      this.readyState = 1;
      log("channel open");
      // The media is peer to peer from here: leave the room (a resume joins it again).
      this.leaveHub();
      this.onopen?.(new Event("open"));
    }

    private fromChannel(data: unknown): void {
      if (this.readyState !== 1) return;
      if (typeof data === "string") {
        this.onmessage?.(new MessageEvent("message", { data }));
        return;
      }
      if (!(data instanceof ArrayBuffer)) return;
      const p = this.rx.push(data);
      switch (p.kind) {
        case "ping":
          // At once: its round trip is the host's measure of this guest.
          try {
            this.dc?.send(pong(p.data));
          } catch {
            // Closing.
          }
          return;
        case "message":
          this.onmessage?.(new MessageEvent("message", { data: p.data }));
          return;
        case "error":
          log(`channel: bad binary message (${p.why})`);
          this.dropped();
          return;
        default:
          return;
      }
    }

    /** Which way the connection goes now (the pair ICE selected), or null before it is known. */
    async netPath(): Promise<NetPath | null> {
      const pc = this.pc;
      if (!pc || this.readyState !== 1 || typeof pc.getStats !== "function") return null;
      try {
        return selectedPath(await pc.getStats());
      } catch {
        return null;
      }
    }

    send(data: string | ArrayBufferLike | ArrayBufferView): void {
      const dc = this.dc;
      if (this.readyState !== 1 || !dc || dc.readyState !== "open") throw new Error("not open");
      if (typeof data === "string") {
        dc.send(data);
        return;
      }
      const bytes = ArrayBuffer.isView(data)
        ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
        : new Uint8Array(data);
      for (const piece of framePieces(bytes)) dc.send(piece);
    }

    close(): void {
      if (this.readyState === 3) return;
      this.teardown();
      this.emitClose(1000, "", true);
    }

    private clearTimer(): void {
      if (this.timer) clearTimeout(this.timer);
      this.timer = null;
    }

    private leaveHub(): void {
      const hub = this.hub;
      this.hub = null;
      if (!hub) return;
      try {
        if (hub.readyState === 1) hub.send(JSON.stringify({ t: "leave" }));
        hub.close(1000);
      } catch {
        // Already closing.
      }
    }

    /** The hub is restarting: drop this try and start another after a moment. False once it has
     *  been away too long (the caller then fails as before). */
    private hubRestarting(): boolean {
      const now = Date.now();
      if (!this.hubBackUntil) this.hubBackUntil = now + HUB_BACK_FOR_MS;
      if (now >= this.hubBackUntil) return false;
      log("hub: restarting, trying again");
      this.clearTimer();
      this.leaveHub();
      this.closePeer();
      this.answered = false;
      this.early = [];
      this.timer = setTimeout(() => {
        this.timer = null;
        this.begin();
      }, HUB_BACK_WAIT_MS);
      return true;
    }

    private teardown(): void {
      this.readyState = 3;
      this.clearTimer();
      this.leaveHub();
      this.closePeer();
    }

    private closePeer(): void {
      const pc = this.pc;
      const dc = this.dc;
      this.pc = null;
      this.dc = null;
      this.media = null;
      try {
        dc?.close();
      } catch {
        // Already closed.
      }
      try {
        pc?.close();
      } catch {
        // Already closed.
      }
    }

    /** The channel went away after it opened: an ordinary drop (the session resumes). */
    private dropped(): void {
      if (this.readyState === 3) return;
      this.teardown();
      this.emitClose(1006, "", false);
    }

    /** Could not connect (or must not): close with the reason the session shows. */
    private fail(why: NetWhy): void {
      if (this.readyState === 3) return;
      log(`failed: ${why}`);
      this.teardown();
      this.emitClose(NET_CLOSE, `net:${why}`, false);
    }

    private emitClose(code: number, reason: string, wasClean: boolean): void {
      const h = this.onclose;
      if (!h) return;
      const e =
        typeof CloseEvent === "undefined"
          ? ({ type: "close", code, reason, wasClean } as unknown as CloseEvent)
          : new CloseEvent("close", { code, reason, wasClean });
      setTimeout(() => h(e), 0);
    }
  }

  return RtcSocket as unknown as JoinTransport;
}
