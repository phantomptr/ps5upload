// A guest's way to the host's game: several tries at once, the first to open plays (Happy
// Eyeballs). Each WebRTC try is a whole connection (rtcTransport): signalled through the host's
// own engine on the same Wi-Fi (`lan`) or tailnet (`tailscale`), or through the hub (`hub`).
// Whichever way it was signalled, the connection is the same kind (the answer checked against the
// link's fingerprint when it names one, the invite key only on the channel) and ICE picks the
// shortest pair it has. Last comes `socket`: the plain WebSocket to the host's page (TCP, the
// picture through the page's own player), only when WebRTC could not connect.
//
// Why race the signalling: a guest on the host's Wi-Fi then needs neither the internet nor the
// hub (a hub outage, a router without internet, a reconnect while the hub restarts), and is in
// sooner. The hub's try waits LOCAL_HEAD_START_MS (or until every local try failed), the socket
// SOCKET_AFTER_MS; the way that worked goes first on every reconnect.
//
// Never weaker: a pinned try (its link names the host's fingerprint) that meets another
// certificate rules out the socket for this connect (it would hand the key to whoever answered);
// WebRTC tries checked against the fingerprint may still go on.
//
// To the session (lib/joinClient) it is one WebSocket-shaped transport: every connect races
// again, and a drop reconnects by the best way open then (the resume token works on any of them:
// they all reach the same engine).

import type { InternetInvite, TrackMedia } from "../lib/joinProtocol";
import { netFailure } from "../lib/joinProtocol";
import type { NetPath } from "./netPath";
import type { JoinSocket, JoinTransport, RtcTransportOptions } from "./rtcTransport";

/** How a try connects: WebRTC signalled on the host's Wi-Fi, tailnet or the hub, or the plain
 *  WebSocket. */
export type SignalVia = "lan" | "tailscale" | "hub" | "socket";

export interface PathTry {
  via: SignalVia;
  /** Makes the try (one connection attempt, WebSocket-shaped). */
  make: () => JoinSocket;
  /** The answer is checked against the link's fingerprint: its `mismatch` means someone else
   *  answered, and the socket is not tried. */
  pinned?: boolean;
}

/** The hub's try starts this long after the local ones (sooner when they have all failed). */
export const LOCAL_HEAD_START_MS = 300;
/** The plain socket starts this long after the WebRTC tries (sooner when they have all failed). */
export const SOCKET_AFTER_MS = 3000;

/** What the race exposes beyond a WebSocket (the session reads them when joined). */
export interface RaceSocket extends JoinSocket {
  /** The try that opened, once one did. */
  readonly via: SignalVia | null;
  readonly media: TrackMedia | null;
  netPath(): Promise<NetPath | null>;
}

/** The host engine's socket `route` (`api/join/signal`, `api/join/ws`) beside its page
 *  (`http://host:port[/prefix]/join` → `ws://host:port[/prefix]/<route>`), or null when it is not
 *  such a page. */
export function socketBeside(page: string, route: "api/join/signal" | "api/join/ws"): string | null {
  if (!/^https?:\/\/[^/?#]+(\/[^?#]*)?\/join\/?$/.test(page)) return null;
  return page.replace(/^http/, "ws").replace(/\/join\/?$/, `/${route}`);
}

/** The host engine's signalling socket beside its page. */
export const signalSocketUrl = (page: string) => socketBeside(page, "api/join/signal");

export interface TryPlan {
  /** The internet invite, when the link is one. */
  net: InternetInvite | null;
  /** The host's page on their Wi-Fi (an internet link's `lan`; a Wi-Fi link's own) and on
   *  Tailscale (`ts`). */
  lan: string | null;
  ts: string | null;
  /** The engine's certificate the link names (an internet link's always; a Wi-Fi link's since
   *  12.4), or null. */
  fp: string | null;
  /** This page has WebRTC. */
  rtc: boolean;
  /** How this page reaches a ws:// address on another machine: as it is (the host's own page),
   *  or through this app's engine (`/api/join/relay`). */
  reach: (ws: string) => string;
  rtcMake: (inv: InternetInvite, o: Pick<RtcTransportOptions, "signalUrl" | "lan">) => JoinTransport;
  /** The plain WebSocket to `url`. */
  socket: (url: string) => JoinSocket;
}

/** The ways to try, best first: WebRTC signalled on the host's Wi-Fi and tailnet pages (an
 *  internet link's room, or a Wi-Fi link's own join), the hub (an internet link), and the plain
 *  socket to the host's Wi-Fi page last. */
export function joinTries(p: TryPlan): PathTry[] {
  const out: PathTry[] = [];
  if (p.rtc) {
    const inv: InternetInvite = p.net ?? { room: "", key: "", fp: p.fp ?? "", hub: "" };
    for (const [via, page] of [
      ["lan", p.lan],
      ["tailscale", p.ts],
    ] as const) {
      const sig = page ? signalSocketUrl(page) : null;
      if (!sig) continue;
      const T = p.rtcMake(inv, { signalUrl: p.reach(sig), ...(p.net ? {} : { lan: true }) });
      out.push({ via, make: () => new T("") as unknown as JoinSocket, pinned: !!inv.fp });
    }
    if (p.net) {
      const H = p.rtcMake(p.net, {});
      out.push({ via: "hub", make: () => new H("") as unknown as JoinSocket, pinned: true });
    }
  }
  const ws = p.lan ? socketBeside(p.lan, "api/join/ws") : null;
  if (ws) {
    const url = p.reach(ws);
    out.push({ via: "socket", make: () => p.socket(url) });
  }
  return out;
}

/** The way that last opened, by room: it goes first (no head start for the others to wait out). */
const lastWin = new Map<string, SignalVia>();

/** Tests: forget what worked. */
export function forgetPaths(): void {
  lastWin.clear();
}

/** When each try starts (ms after the connect), in order: the local ones at once, the hub's after
 *  the head start, the socket after SOCKET_AFTER_MS; whatever worked last time for this room at
 *  once. */
export function startPlan(
  tries: readonly SignalVia[],
  last: SignalVia | null,
  headStart = LOCAL_HEAD_START_MS,
  socketAfter = SOCKET_AFTER_MS,
): number[] {
  const hasLocal = tries.some((v) => v === "lan" || v === "tailscale");
  const hasRtc = tries.some((v) => v !== "socket");
  return tries.map((v) => {
    if (v === last) return 0;
    if (v === "hub" && hasLocal) return headStart;
    if (v === "socket" && hasRtc) return socketAfter;
    return 0;
  });
}

/** Which close the race ends with when every try failed: the hub's (the one the guest's words are
 *  about), unless it could not be reached and a try on the host's own network got further (it
 *  reached the host's engine: its reason is the real one). A failure on a local try is never
 *  final on its own (a device at that address that is not the host's engine, or one pretending to
 *  be, only fails its own try). */
export function finalClose(closes: ReadonlyArray<{ via: SignalVia; e: CloseEvent }>): CloseEvent | null {
  const hub = closes.find((c) => c.via === "hub");
  const further = closes.find(
    (c) => (c.via === "lan" || c.via === "tailscale") && ["no_direct", "net_full", "offer"].includes(netFailure(c.e) ?? ""),
  );
  if (hub && (netFailure(hub.e) !== "no_hub" || !further)) return hub.e;
  if (further) return further.e;
  return hub?.e ?? closes[closes.length - 1]?.e ?? null;
}

export interface RaceOptions {
  /** The room (what worked is remembered by it). */
  room: string;
  /** Tests: the head start, the socket's wait. */
  headStartMs?: number;
  socketAfterMs?: number;
  log?: (m: string) => void;
  /** The try that opened (for the logs and the page). */
  onWin?: (via: SignalVia) => void;
}

/** A WebSocket-shaped transport that races `tries()` (made fresh for each connect). */
export function raceTransport(tries: () => PathTry[], o: RaceOptions): JoinTransport {
  const log = o.log ?? (() => {});
  const headStart = o.headStartMs ?? LOCAL_HEAD_START_MS;
  const socketAfter = o.socketAfterMs ?? SOCKET_AFTER_MS;

  class Race implements RaceSocket {
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

    private live: Array<{ via: SignalVia; s: JoinSocket }> = [];
    private waiting: Array<{ via: SignalVia; make: () => JoinSocket; timer: ReturnType<typeof setTimeout> }> = [];
    private pinned = new Set<SignalVia>();
    private t0 = 0;
    private closes: Array<{ via: SignalVia; e: CloseEvent }> = [];
    private winner: { via: SignalVia; s: JoinSocket } | null = null;

    constructor(_url?: string) {
      // The session sets its handlers first.
      setTimeout(() => this.begin(), 0);
    }

    get via(): SignalVia | null {
      return this.winner?.via ?? null;
    }

    get media(): TrackMedia | null {
      return (this.winner?.s as { media?: TrackMedia | null } | undefined)?.media ?? null;
    }

    async netPath(): Promise<NetPath | null> {
      const s = this.winner?.s as { netPath?: () => Promise<NetPath | null> } | undefined;
      return s?.netPath ? s.netPath() : null;
    }

    private begin(): void {
      if (this.readyState !== 0) return;
      const list = tries();
      if (!list.length) {
        this.end({ type: "close", code: 1006, reason: "", wasClean: false } as CloseEvent);
        return;
      }
      this.t0 = Date.now();
      for (const t of list) if (t.pinned) this.pinned.add(t.via);
      const plan = startPlan(
        list.map((t) => t.via),
        lastWin.get(o.room) ?? null,
        headStart,
        socketAfter,
      );
      list.forEach((t, i) => {
        if (plan[i] === 0) this.launch(t.via, t.make);
        else this.waiting.push({ via: t.via, make: t.make, timer: setTimeout(() => this.startWaiting(t.via), plan[i]) });
      });
    }

    private startWaiting(via: SignalVia): void {
      const i = this.waiting.findIndex((w) => w.via === via);
      if (i < 0) return;
      const [w] = this.waiting.splice(i, 1);
      clearTimeout(w.timer);
      this.launch(w.via, w.make);
    }

    private launch(via: SignalVia, make: () => JoinSocket): void {
      if (this.readyState !== 0) return;
      let s: JoinSocket;
      try {
        s = make();
      } catch {
        this.tryClosed(via, { type: "close", code: 1006, reason: "", wasClean: false } as CloseEvent);
        return;
      }
      log(`path: trying ${via}`);
      const entry = { via, s };
      this.live.push(entry);
      s.binaryType = this.binaryType;
      s.onopen = (e) => this.won(entry, e);
      s.onclose = (e) => {
        this.live = this.live.filter((x) => x !== entry);
        this.tryClosed(via, e);
      };
      s.onerror = () => {};
      s.onmessage = null;
    }

    private tryClosed(via: SignalVia, e: CloseEvent): void {
      if (this.readyState !== 0) return;
      log(`path: ${via} failed after ${Date.now() - this.t0} ms${e?.reason ? ` (${e.reason})` : ""}`);
      this.closes.push({ via, e });
      // Someone else answered for the host: never the unchecked socket now.
      if (this.pinned.has(via) && netFailure(e) === "mismatch") {
        for (const w of this.waiting.filter((x) => x.via === "socket")) clearTimeout(w.timer);
        this.waiting = this.waiting.filter((x) => x.via !== "socket");
      }
      // Nothing left trying: the next waiting one now, rather than at its time.
      if (!this.live.length && this.waiting.length) {
        this.startWaiting(this.waiting[0].via);
        return;
      }
      if (!this.live.length && !this.waiting.length) this.end(finalClose(this.closes));
    }

    private won(entry: { via: SignalVia; s: JoinSocket }, e: Event): void {
      if (this.readyState !== 0) return;
      this.winner = entry;
      lastWin.set(o.room, entry.via);
      log(`path: playing by ${entry.via} after ${Date.now() - this.t0} ms`);
      for (const w of this.waiting.splice(0)) clearTimeout(w.timer);
      for (const other of this.live.splice(0)) {
        if (other === entry) continue;
        other.s.onopen = other.s.onclose = other.s.onmessage = other.s.onerror = null;
        try {
          other.s.close();
        } catch {
          // Already closing.
        }
      }
      const s = entry.s;
      s.onmessage = (m) => this.onmessage?.(m);
      s.onerror = (x) => this.onerror?.(x);
      s.onclose = (c) => {
        if (this.readyState === 3) return;
        this.readyState = 3;
        this.onclose?.(c);
      };
      this.readyState = 1;
      o.onWin?.(entry.via);
      this.onopen?.(e);
    }

    send(data: string | ArrayBufferLike | ArrayBufferView): void {
      if (this.readyState !== 1 || !this.winner) throw new Error("not open");
      this.winner.s.send(data);
    }

    close(code?: number, reason?: string): void {
      if (this.readyState === 3) return;
      const wasOpen = this.readyState === 1;
      this.readyState = 3;
      for (const w of this.waiting.splice(0)) clearTimeout(w.timer);
      const all = this.winner ? [this.winner, ...this.live] : this.live.splice(0);
      for (const x of all) {
        x.s.onopen = x.s.onclose = x.s.onmessage = x.s.onerror = null;
        try {
          x.s.close(code, reason);
        } catch {
          // Already closing.
        }
      }
      const h = this.onclose;
      if (h) {
        const c = { type: "close", code: code ?? 1000, reason: reason ?? "", wasClean: wasOpen } as CloseEvent;
        setTimeout(() => h(c), 0);
      }
    }

    private end(e: CloseEvent | null): void {
      if (this.readyState === 3) return;
      this.readyState = 3;
      const h = this.onclose;
      const c = e ?? ({ type: "close", code: 1006, reason: "", wasClean: false } as CloseEvent);
      if (h) setTimeout(() => h(c), 0);
    }
  }

  return Race as unknown as JoinTransport;
}
