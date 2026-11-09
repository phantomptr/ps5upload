// The app's way to a friend's game (an internet invite): several tries at once, the first to open
// plays. Each try is a whole connection (rtcTransport): signalled through the host's own engine on
// the same Wi-Fi (`lan`) or tailnet (`tailscale`), or through the hub (`hub`). Whichever way it was
// signalled, the connection is the same kind (WebRTC, the answer checked against the link's
// fingerprint, the invite key only on the channel) and ICE picks the shortest pair it has.
//
// Why race the signalling: a guest on the host's Wi-Fi then needs neither the internet nor the
// hub (a hub outage, a router without internet, a reconnect while the hub restarts), and is in a
// little sooner. The hub's try waits LOCAL_HEAD_START_MS (or until every local try failed) so a
// guest at home does not make two connections; a guest away pays that once, then the way that
// worked goes first on every reconnect.
//
// To the session (lib/joinClient) it is one WebSocket-shaped transport: every connect races
// again, and a drop reconnects by the best way open then (the resume token works on any of them:
// they all reach the same engine).

import type { TrackMedia } from "../lib/joinProtocol";
import { netFailure } from "../lib/joinProtocol";
import type { NetPath } from "./netPath";
import type { JoinSocket, JoinTransport } from "./rtcTransport";

/** How a try signals. */
export type SignalVia = "lan" | "tailscale" | "hub";

export interface PathTry {
  via: SignalVia;
  /** Makes the try (one connection attempt, WebSocket-shaped). */
  make: () => JoinSocket;
}

/** The hub's try starts this long after the local ones (sooner when they have all failed). */
export const LOCAL_HEAD_START_MS = 1500;

/** What the race exposes beyond a WebSocket (the session reads them when joined). */
export interface RaceSocket extends JoinSocket {
  /** The try that opened, once one did. */
  readonly via: SignalVia | null;
  readonly media: TrackMedia | null;
  netPath(): Promise<NetPath | null>;
}

/** The way that last opened, by room: it goes first (no head start for the others to wait out). */
const lastWin = new Map<string, SignalVia>();

/** Tests: forget what worked. */
export function forgetPaths(): void {
  lastWin.clear();
}

/** When each try starts (ms after the connect), in order: the local ones at once, the hub's after
 *  the head start, unless the hub is what worked last time for this room. */
export function startPlan(tries: readonly SignalVia[], last: SignalVia | null, headStart = LOCAL_HEAD_START_MS): number[] {
  const hasLocal = tries.some((v) => v !== "hub");
  return tries.map((v) => (v === "hub" && hasLocal && last !== "hub" ? headStart : 0));
}

/** Which close the race ends with when every try failed: the hub's (the one the guest's words are
 *  about), unless it could not be reached and a try on the host's own network got further (it
 *  reached the host's engine: its reason is the real one). A failure on a local try is never
 *  final on its own (a device at that address that is not the host's engine, or one pretending to
 *  be, only fails its own try). */
export function finalClose(closes: ReadonlyArray<{ via: SignalVia; e: CloseEvent }>): CloseEvent | null {
  const hub = closes.find((c) => c.via === "hub");
  const further = closes.find((c) => c.via !== "hub" && ["no_direct", "net_full", "offer"].includes(netFailure(c.e) ?? ""));
  if (hub && (netFailure(hub.e) !== "no_hub" || !further)) return hub.e;
  if (further) return further.e;
  return hub?.e ?? closes[closes.length - 1]?.e ?? null;
}

export interface RaceOptions {
  /** The room (what worked is remembered by it). */
  room: string;
  /** Tests: the head start. */
  headStartMs?: number;
  log?: (m: string) => void;
  /** The try that opened (for the logs and the page). */
  onWin?: (via: SignalVia) => void;
}

/** A WebSocket-shaped transport that races `tries()` (made fresh for each connect). */
export function raceTransport(tries: () => PathTry[], o: RaceOptions): JoinTransport {
  const log = o.log ?? (() => {});
  const headStart = o.headStartMs ?? LOCAL_HEAD_START_MS;

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
      const plan = startPlan(
        list.map((t) => t.via),
        lastWin.get(o.room) ?? null,
        headStart,
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
      log(`path: ${via} failed${e?.reason ? ` (${e.reason})` : ""}`);
      this.closes.push({ via, e });
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
      log(`path: playing by ${entry.via}`);
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
