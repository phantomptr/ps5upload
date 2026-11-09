// Play Together (#641), the guest's side: one friend's link to the host's engine
// (/api/join/ws). It asks to join, plays as Player N, and keeps that place through a network
// blip: a dropped socket reconnects on its own (250 ms, doubling to 4 s) with the resume token,
// for as long as the host keeps the place (60 s). The host stopping playing keeps it too: the
// guest waits (and reconnects) for 60 s for the host to start again. The token lives in
// sessionStorage, never in the address.
//
// Input: the pad is sent when it changes (checked every animation frame) and at least every
// 100 ms; let go (neutral) while the page is hidden or loses the focus; never while the socket
// is down.

import {
  backoffMs,
  encodePadFrame,
  HOST_RESTART_WINDOW_MS,
  netFailure,
  readServerMessage,
  RESUME_WINDOW_MS,
  type ByeWhy,
  type JoinStatus,
  type NetWhy,
  type TrackMedia,
  type RefuseWhy,
} from "./joinProtocol";
import { IDLE_PAD, type PadState } from "./remotePlayStream";
import { samePad } from "./padClient";

export type JoinStage =
  | { kind: "connecting" }
  /** Waiting for the host to allow this guest. */
  | { kind: "asking" }
  | { kind: "joined"; player: number; hevc: boolean }
  /** The socket dropped; trying again until `deadline` (ms, the client's clock). `hostStopped`:
   *  the host stopped playing, and the guest waits for them to start again. */
  | { kind: "reconnecting"; player: number; attempt: number; deadline: number; hostStopped?: boolean }
  | { kind: "refused"; why: RefuseWhy }
  /** `gave_up`: the place was lost (60 s without the host); `lost`: never got in; `left`: Leave;
   *  a NetWhy: an internet invite's connection could not be made (join/rtcTransport). */
  | { kind: "ended"; why: ByeWhy | "gave_up" | "lost" | "left" | NetWhy };

type Timer = ReturnType<typeof setTimeout>;

export interface JoinStorage {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
  removeItem(k: string): void;
}

export interface JoinClientOptions {
  url: string;
  invite: string | null;
  /** A short code typed instead of an invite (`{"hello": {"code": …}}`). */
  code?: string | null;
  name: string;
  /** The pad this device is now (gamepads + on-screen controller). */
  readPad: () => PadState;
  onStage: (s: JoinStage) => void;
  onStatus?: (s: JoinStatus) => void;
  /** Round trip measured by this page's own pings, in ms. */
  onRtt?: (ms: number) => void;
  onFrame?: (data: ArrayBuffer) => void;
  /** The session's codec, and its video track when the pictures come on one (an internet socket
   *  that has it, and the engine said `track`); null: they come on the socket. */
  onCodec?: (hevc: boolean, track: TrackMedia | null, audio?: boolean) => void;
  onRumble?: (strong: number, weak: number) => void;
  onLight?: (rgb: [number, number, number]) => void;
  /** The picture's numbers (drawn, dropped, stalls…), sent to the host every STATS_EVERY_PINGS
   *  pings as `{"stats": …}` so the host's panel can say how the picture looks here. */
  readStats?: () => Record<string, unknown> | null;
  /** Where the resume token is kept (sessionStorage by default; null: nowhere). */
  storage?: JoinStorage | null;
  /** Tests: a fake socket and clock. */
  WebSocketImpl?: typeof WebSocket;
  now?: () => number;
  raf?: (cb: () => void) => number;
  cancelRaf?: (id: number) => void;
}

/** sessionStorage, when this page has one it may use. */
function defaultStorage(): JoinStorage | null {
  try {
    return typeof sessionStorage !== "undefined" ? sessionStorage : null;
  } catch {
    return null;
  }
}

/** Where the token for an invite is kept. */
export const tokenKey = (invite: string | null) => `ps5upload.join.token.${invite ?? "app"}`;

/** Keepalive: the pad is sent at least this often (the helper goes neutral after 400 ms). */
export const KEEPALIVE_MS = 100;
/** No message for this long while joined (status and pong come every second): the link is dead. */
export const SILENCE_MS = 3500;
/** Waiting for the host past the window this much longer, the engine's own answer not come: give up. */
export const HOST_WAIT_SLACK_MS = 5000;
/** The picture's report goes with every this-many pings (one a second): every 2 s. */
export const STATS_EVERY_PINGS = 2;

export class JoinClient {
  private ws: WebSocket | null = null;
  private stage: JoinStage = { kind: "connecting" };
  private token: string | null = null;
  private player = 0;
  private seq: number;
  private last: PadState = { ...IDLE_PAD };
  private lastSent = 0;
  private neutral = false;
  private stopped = false;
  private attempt = 0;
  private deadline = 0;
  private raf = 0;
  private keepalive: Timer | null = null;
  private pinger: Timer | null = null;
  private retry: Timer | null = null;
  private lastHeard = 0;
  /** The host stopped playing: the place waits for them until `deadline`. */
  private hostAway = false;
  private readonly now: () => number;
  private readonly storage: JoinStorage | null;
  private readonly WS: typeof WebSocket;

  constructor(private o: JoinClientOptions) {
    this.now = o.now ?? (() => Date.now());
    this.storage = o.storage === undefined ? defaultStorage() : o.storage;
    this.WS = o.WebSocketImpl ?? WebSocket;
    // Monotonic across page reloads too: frames go out far slower than the clock's 1000/s.
    this.seq = this.now() >>> 0;
    try {
      this.token = this.storage?.getItem(this.key) ?? null;
    } catch {
      this.token = null;
    }
  }

  /** Where this place's token is kept. */
  private get key(): string {
    return tokenKey(this.o.invite ?? this.o.code ?? null);
  }

  get current(): JoinStage {
    return this.stage;
  }

  /** The socket now (an internet invite's transport says which way it goes: join/pathRace). */
  get socket(): WebSocket | null {
    return this.ws;
  }

  /** The token this guest resumes with (tests). */
  get resumeToken(): string | null {
    return this.token;
  }

  private setStage(s: JoinStage): void {
    this.stage = s;
    this.o.onStage(s);
  }

  start(): void {
    this.stopped = false;
    this.setStage({ kind: "connecting" });
    this.connect();
    const tick = () => {
      this.sendPad(false);
      this.raf = (this.o.raf ?? ((cb) => window.requestAnimationFrame(cb)))(tick);
    };
    this.raf = (this.o.raf ?? ((cb) => window.requestAnimationFrame(cb)))(tick);
    this.keepalive = setInterval(() => this.sendPad(false), KEEPALIVE_MS);
    this.pinger = setInterval(() => this.ping(), 1000);
  }

  private connect(): void {
    let ws: WebSocket;
    try {
      ws = new this.WS(this.o.url);
    } catch {
      this.dropped();
      return;
    }
    this.ws = ws;
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.lastHeard = this.now();
      const hello: Record<string, string> = { name: this.o.name };
      if (this.o.invite) hello.invite = this.o.invite;
      if (this.o.code) hello.code = this.o.code;
      if (this.token) hello.resume = this.token;
      ws.send(JSON.stringify({ hello }));
    };
    ws.onmessage = (e: MessageEvent) => {
      if (this.ws !== ws) return;
      this.lastHeard = this.now();
      if (typeof e.data !== "string") {
        if (e.data instanceof ArrayBuffer && this.stage.kind === "joined") this.o.onFrame?.(e.data);
        return;
      }
      this.onText(e.data);
    };
    ws.onclose = (e?: CloseEvent) => {
      if (this.ws !== ws) return;
      this.ws = null;
      // An internet invite's transport says why it could not connect (join/rtcTransport).
      const why = netFailure(e);
      if (why === "mismatch") {
        // Not the host's engine: never with this link, resume or not.
        this.forgetToken();
        this.finish({ kind: "ended", why });
        return;
      }
      if (why && this.stage.kind !== "joined" && this.stage.kind !== "reconnecting") {
        this.finish({ kind: "ended", why });
        return;
      }
      this.dropped();
    };
    ws.onerror = () => {
      // onclose follows.
    };
  }

  private onText(text: string): void {
    const m = readServerMessage(text);
    if (!m) return;
    switch (m.kind) {
      case "pong": {
        const rtt = this.now() - m.t;
        if (rtt >= 0 && rtt < 60_000) this.o.onRtt?.(rtt);
        return;
      }
      case "asking":
        if (this.stage.kind !== "reconnecting") this.setStage({ kind: "asking" });
        return;
      case "waiting":
        // The engine holds this resume until the host plays again (a reload after they stopped
        // learns it here).
        if (!this.hostAway) {
          this.hostAway = true;
          this.deadline = this.now() + HOST_RESTART_WINDOW_MS;
        }
        this.attempt = 0;
        this.setStage(this.reconnectingStage());
        return;
      case "joined":
        this.token = m.token;
        this.player = m.player;
        this.attempt = 0;
        this.hostAway = false;
        try {
          this.storage?.setItem(this.key, m.token);
        } catch {
          // Private mode: a reload asks again.
        }
        {
          const media = (m.track && (this.ws as { media?: TrackMedia | null } | null)?.media) || null;
          // The sound on the track too: the engine said so and the answer brought it.
          this.o.onCodec?.(m.hevc, media, !!media && m.audio && !!media.audio);
        }
        this.setStage({ kind: "joined", player: m.player, hevc: m.hevc });
        this.sendPad(true);
        return;
      case "refused":
        // A resume the engine no longer knows: the place is gone, not this invite's fault.
        this.forgetToken();
        // Waiting for the host, the place went with Play Together: they did not play again.
        // (Not when the host came back and needed their stream's controller: that says so.)
        if (this.hostAway && m.why !== "seat_taken") this.finish({ kind: "ended", why: "host_stopped" });
        else this.finish({ kind: "refused", why: m.why });
        return;
      case "bye":
        if (m.why === "host_stopped" && this.token) {
          // The place is kept while the host may start again: wait for them, coming back with
          // the token (the engine answers "waiting" until they do).
          this.hostWait();
          return;
        }
        this.forgetToken();
        this.finish({ kind: "ended", why: m.why });
        return;
      case "status":
        // Moved to another player while in (off the host's stream's controller onto a
        // controller of their own, #720): the place is the new one.
        if (this.stage.kind === "joined" && m.status.player > 0 && m.status.player !== this.stage.player) {
          this.player = m.status.player;
          this.setStage({ ...this.stage, player: m.status.player });
        }
        this.o.onStatus?.(m.status);
        return;
      case "feedback":
        if (m.rumble) this.o.onRumble?.(m.rumble[0], m.rumble[1]);
        if (m.light) this.o.onLight?.(m.light);
        return;
    }
  }

  /** The host stopped playing: drop this socket and come back with the token, for as long as the
   *  engine keeps the place. */
  private hostWait(): void {
    this.hostAway = true;
    this.deadline = this.now() + HOST_RESTART_WINDOW_MS;
    this.attempt = 0;
    const ws = this.ws;
    this.ws = null;
    try {
      ws?.close();
    } catch {
      // Already closing.
    }
    this.scheduleRetry();
  }

  private reconnectingStage(): JoinStage {
    return {
      kind: "reconnecting",
      player: this.player,
      attempt: this.attempt,
      deadline: this.deadline,
      ...(this.hostAway ? { hostStopped: true } : {}),
    };
  }

  /** Out of time: the host did not play again, or the connection did not come back. */
  private giveUp(): void {
    this.forgetToken();
    this.finish({ kind: "ended", why: this.hostAway ? "host_stopped" : "gave_up" });
  }

  /** The socket is gone (or never came up): keep the place if there is one, else stop. */
  private dropped(): void {
    if (this.stopped) return;
    const s = this.stage;
    if (s.kind === "joined" || (s.kind === "reconnecting" && this.token)) {
      if (s.kind === "joined") this.deadline = this.now() + RESUME_WINDOW_MS;
      this.scheduleRetry();
      return;
    }
    this.finish({ kind: "ended", why: "lost" });
  }

  private scheduleRetry(): void {
    if (this.stopped) return;
    if (this.now() >= this.deadline) {
      this.giveUp();
      return;
    }
    const wait = backoffMs(this.attempt);
    this.setStage(this.reconnectingStage());
    this.attempt += 1;
    if (this.retry) clearTimeout(this.retry);
    this.retry = setTimeout(() => {
      this.retry = null;
      if (this.stopped) return;
      if (this.now() >= this.deadline) {
        this.giveUp();
        return;
      }
      this.connect();
    }, wait);
  }

  private forgetToken(): void {
    this.token = null;
    try {
      this.storage?.removeItem(this.key);
    } catch {
      // Nothing kept.
    }
  }

  private ping(): void {
    if (this.hostAway && !this.stopped && this.now() >= this.deadline + HOST_WAIT_SLACK_MS) {
      // Still waiting past the window (the engine's answer did not come): stop waiting.
      this.giveUp();
      return;
    }
    if (!this.stopped && this.stage.kind === "reconnecting" && this.now() >= this.deadline + HOST_WAIT_SLACK_MS) {
      // A socket came back but the engine never answered it: the place is gone by now.
      this.giveUp();
      return;
    }
    const ws = this.ws;
    if (!ws || ws.readyState !== this.WS.OPEN) return;
    if (this.stage.kind === "joined" && this.now() - this.lastHeard > SILENCE_MS) {
      // Wi-Fi gone quiet without a close: drop it and reconnect.
      this.ws = null;
      try {
        ws.close();
      } catch {
        // Already closing.
      }
      this.dropped();
      return;
    }
    ws.send(JSON.stringify({ ping: this.now() }));
    if (this.stage.kind === "joined" && this.o.readStats && ++this.pings % STATS_EVERY_PINGS === 0) {
      const stats = this.o.readStats();
      if (stats) ws.send(JSON.stringify({ stats }));
    }
  }
  private pings = 0;

  /** Let go of everything (the page is hidden or lost the focus), or take the pad back. */
  setNeutral(on: boolean): void {
    if (this.neutral === on) return;
    this.neutral = on;
    this.sendPad(true);
  }

  /** Send the pad when it changed, the keepalive is due, or `force`. */
  sendPad(force: boolean): void {
    const ws = this.ws;
    if (!ws || ws.readyState !== this.WS.OPEN || this.stage.kind !== "joined") return;
    const p = this.neutral ? { ...IDLE_PAD, touch: null, motion: null } : this.o.readPad();
    const now = this.now();
    if (!force && samePad(p, this.last) && now - this.lastSent < KEEPALIVE_MS) return;
    this.last = p;
    this.lastSent = now;
    this.seq = (this.seq + 1) >>> 0;
    ws.send(encodePadFrame(this.seq, p));
  }

  private clearTimers(): void {
    (this.o.cancelRaf ?? ((id) => window.cancelAnimationFrame(id)))(this.raf);
    if (this.keepalive) clearInterval(this.keepalive);
    if (this.pinger) clearInterval(this.pinger);
    if (this.retry) clearTimeout(this.retry);
    this.keepalive = this.pinger = this.retry = null;
  }

  private finish(s: JoinStage): void {
    this.stopped = true;
    this.clearTimers();
    const ws = this.ws;
    this.ws = null;
    try {
      ws?.close();
    } catch {
      // Already closed.
    }
    this.setStage(s);
  }

  /** The picture waits for a keyframe (#648): ask the host's engine to get one from the PS5.
   *  The player spaces these out; the engine does too. */
  askKeyframe(): void {
    const ws = this.ws;
    if (ws && ws.readyState === this.WS.OPEN && this.stage.kind === "joined") {
      ws.send(JSON.stringify({ keyframe: true }));
    }
  }

  /** Leave: let go of the pad, say so, and forget the place. */
  leave(): void {
    const ws = this.ws;
    if (ws && ws.readyState === this.WS.OPEN) {
      if (this.stage.kind === "joined") {
        this.seq = (this.seq + 1) >>> 0;
        ws.send(encodePadFrame(this.seq, IDLE_PAD));
      }
      ws.send(JSON.stringify({ leave: 1 }));
    }
    this.forgetToken();
    this.finish({ kind: "ended", why: "left" });
  }

  /** Stop without leaving (the page is going away): the place stays for a reload. */
  stop(): void {
    const ws = this.ws;
    if (ws && ws.readyState === this.WS.OPEN && this.stage.kind === "joined") {
      this.seq = (this.seq + 1) >>> 0;
      ws.send(encodePadFrame(this.seq, IDLE_PAD));
    }
    this.stopped = true;
    this.clearTimers();
    this.ws = null;
    try {
      ws?.close();
    } catch {
      // Already closed.
    }
  }
}
