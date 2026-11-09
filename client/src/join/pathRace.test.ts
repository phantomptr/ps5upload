import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { JoinClient, type JoinStage } from "../lib/joinClient";
import { NET_CLOSE } from "../lib/joinProtocol";
import { IDLE_PAD } from "../lib/remotePlayStream";
import { joinRelayUrl } from "../state/joinSession";
import {
  finalClose,
  forgetPaths,
  joinTries,
  LOCAL_HEAD_START_MS,
  raceTransport,
  signalSocketUrl,
  socketBeside,
  SOCKET_AFTER_MS,
  startPlan,
  type PathTry,
  type RaceSocket,
  type SignalVia,
  type TryPlan,
} from "./pathRace";
import { rtcTransport, type InternetInvite, type JoinSocket } from "./rtcTransport";

const FP = Array.from({ length: 32 }, (_, i) => (i * 5 + 17).toString(16).toUpperCase().padStart(2, "0")).join(":");
const ROOM = "Demo0Room0Not0Real0000";
// A made-up invite key, built here so it does not read as a real one.
const KEY = "0f".repeat(16);
const INV: InternetInvite = { room: ROOM, key: KEY, fp: FP, hub: "hub.example.com" };
const close = (why: string | null): CloseEvent =>
  ({ type: "close", code: why ? NET_CLOSE : 1006, reason: why ? `net:${why}` : "", wasClean: false }) as CloseEvent;

describe("the plan", () => {
  it("starts the local tries at once, the hub's after the head start, the socket last; what worked last at once", () => {
    expect(startPlan(["lan", "tailscale", "hub", "socket"], null)).toEqual([0, 0, LOCAL_HEAD_START_MS, SOCKET_AFTER_MS]);
    expect(startPlan(["lan", "hub"], "lan")).toEqual([0, LOCAL_HEAD_START_MS]);
    expect(startPlan(["lan", "hub"], "hub")).toEqual([0, 0]);
    expect(startPlan(["lan", "socket"], "socket")).toEqual([0, 0]);
    // A link with no page of the host's: the hub at once; no WebRTC here: the socket at once.
    expect(startPlan(["hub"], null)).toEqual([0]);
    expect(startPlan(["socket"], null)).toEqual([0]);
  });

  it("ends with the hub's reason, unless only a try at the host's engine got further", () => {
    const hubDown = { via: "hub" as SignalVia, e: close("no_hub") };
    const lanNoRoom = { via: "lan" as SignalVia, e: close("no_room") };
    const lanIce = { via: "lan" as SignalVia, e: close("no_direct") };
    const lanMismatch = { via: "lan" as SignalVia, e: close("mismatch") };
    expect(finalClose([lanNoRoom, { via: "hub", e: close("no_direct") }])?.reason).toBe("net:no_direct");
    expect(finalClose([lanIce, hubDown])?.reason).toBe("net:no_direct");
    // Something at the LAN address that is not the host's engine says nothing about the invite…
    expect(finalClose([lanNoRoom, hubDown])?.reason).toBe("net:no_hub");
    // …and one that answers with another certificate never ends the game on its own.
    expect(finalClose([lanMismatch, hubDown])?.reason).toBe("net:no_hub");
    expect(finalClose([{ via: "hub", e: close("mismatch") }])?.reason).toBe("net:mismatch");
    expect(finalClose([lanIce])?.reason).toBe("net:no_direct");
    expect(finalClose([])).toBeNull();
  });
});

describe("the ways to try", () => {
  const LAN = "http://192.168.1.20:19113/join";
  const TS = "http://100.101.2.3:19113/join";
  const ENGINE = "http://127.0.0.1:19113";
  const made: Array<{ inv: InternetInvite; o: Record<string, unknown> }> = [];
  const sockets: string[] = [];
  const plan = (o: Partial<TryPlan>): TryPlan => ({
    net: null,
    lan: null,
    ts: null,
    fp: null,
    rtc: true,
    reach: (ws) => joinRelayUrl(ENGINE, ws),
    rtcMake: (inv, opts) => {
      made.push({ inv, o: opts });
      return class {} as unknown as typeof WebSocket;
    },
    socket: (url) => {
      sockets.push(url);
      return {} as JoinSocket;
    },
    ...o,
  });
  beforeEach(() => {
    made.length = 0;
    sockets.length = 0;
  });

  it("finds the host's sockets beside its page, under its path", () => {
    expect(signalSocketUrl(LAN)).toBe("ws://192.168.1.20:19113/api/join/signal");
    expect(signalSocketUrl("https://nas.local/ps5/join/")).toBe("wss://nas.local/ps5/api/join/signal");
    expect(socketBeside(LAN, "api/join/ws")).toBe("ws://192.168.1.20:19113/api/join/ws");
    expect(signalSocketUrl("http://192.168.1.20:19113/pad")).toBeNull();
    expect(signalSocketUrl("javascript:alert(1)//join")).toBeNull();
    expect(signalSocketUrl("http://192.168.1.20:19113/join?x=/join")).toBeNull();
  });

  it("an internet link: its room on the Wi-Fi and tailnet pages, the hub, the plain socket last", () => {
    const tries = joinTries(plan({ net: INV, lan: LAN, ts: TS, fp: FP }));
    expect(tries.map((t) => [t.via, !!t.pinned])).toEqual([
      ["lan", true],
      ["tailscale", true],
      ["hub", true],
      ["socket", false],
    ]);
    expect(made.map((m) => m.o)).toEqual([
      { signalUrl: joinRelayUrl(ENGINE, "ws://192.168.1.20:19113/api/join/signal") },
      { signalUrl: joinRelayUrl(ENGINE, "ws://100.101.2.3:19113/api/join/signal") },
      {},
    ]);
    expect(made.every((m) => m.inv === INV)).toBe(true);
    tries[3].make();
    expect(sockets).toEqual([joinRelayUrl(ENGINE, "ws://192.168.1.20:19113/api/join/ws")]);
    // Without a page of the host's: the hub alone.
    expect(joinTries(plan({ net: INV })).map((t) => t.via)).toEqual(["hub"]);
  });

  it("a Wi-Fi link: WebRTC signalled on the host's page (its certificate pinned when the link has it), then the socket", () => {
    const tries = joinTries(plan({ lan: LAN, fp: FP }));
    expect(tries.map((t) => [t.via, !!t.pinned])).toEqual([
      ["lan", true],
      ["socket", false],
    ]);
    expect(made[0].o).toEqual({ signalUrl: joinRelayUrl(ENGINE, "ws://192.168.1.20:19113/api/join/signal"), lan: true });
    expect(made[0].inv.fp).toBe(FP);
    expect(joinTries(plan({ lan: LAN }))[0].pinned).toBe(false);
    // No WebRTC in this web view: the socket alone.
    expect(joinTries(plan({ lan: LAN, rtc: false })).map((t) => t.via)).toEqual(["socket"]);
  });
});

// ── The race against fake tries ─────────────────────────────────────────────────────────────────

class FakeTry implements JoinSocket {
  static all: FakeTry[] = [];
  readyState = 0;
  binaryType = "blob";
  sent: unknown[] = [];
  closed = false;
  media = null;
  onopen: ((e: Event) => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onclose: ((e: CloseEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;
  constructor(readonly via: SignalVia) {
    FakeTry.all.push(this);
  }
  send(d: unknown) {
    this.sent.push(d);
  }
  close() {
    this.closed = true;
    this.readyState = 3;
  }
  open() {
    this.readyState = 1;
    this.onopen?.({ type: "open" } as Event);
  }
  fail(why: string | null) {
    this.readyState = 3;
    this.onclose?.(close(why));
  }
  msg(data: string) {
    this.onmessage?.({ data } as MessageEvent);
  }
  async netPath() {
    return this.via === "hub" ? ("internet" as const) : ("lan" as const);
  }
}

const of = (via: SignalVia) => FakeTry.all.filter((t) => t.via === via);

function tries(vias: SignalVia[]): () => PathTry[] {
  return () => vias.map((via) => ({ via, make: () => new FakeTry(via) }));
}

describe("the race", () => {
  let stages: JoinStage[];
  beforeEach(() => {
    vi.useFakeTimers();
    FakeTry.all = [];
    forgetPaths();
    stages = [];
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function client(T: typeof WebSocket) {
    return new JoinClient({
      url: "ws://unused",
      invite: KEY,
      name: "Ben",
      readPad: () => ({ ...IDLE_PAD }),
      onStage: (s) => stages.push(s),
      storage: null,
      WebSocketImpl: T,
      now: () => Date.now(),
      raf: () => 0,
      cancelRaf: () => {},
    });
  }

  it("on the host's Wi-Fi: the local try plays, the hub is never asked", async () => {
    const c = client(raceTransport(tries(["lan", "hub"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan"]);
    of("lan")[0].open();
    expect(of("lan")[0].sent[0]).toBe(JSON.stringify({ hello: { name: "Ben", invite: KEY } }));
    await vi.advanceTimersByTimeAsync(LOCAL_HEAD_START_MS * 2);
    expect(of("hub")).toHaveLength(0);
    of("lan")[0].msg(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false }));
    expect(stages[stages.length - 1]).toEqual({ kind: "joined", player: 2, hevc: false });
    const s = c.socket as unknown as RaceSocket;
    expect(s.via).toBe("lan");
    expect(await s.netPath()).toBe("lan");
    c.stop();
  });

  it("away: the hub after the head start, or at once when the local tries failed", async () => {
    const c = client(raceTransport(tries(["lan", "tailscale", "hub"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan", "tailscale"]);
    of("lan")[0].fail("no_hub");
    expect(of("hub")).toHaveLength(0);
    of("tailscale")[0].fail("no_room");
    // Nothing left trying: no waiting out the head start.
    expect(of("hub")).toHaveLength(1);
    of("hub")[0].open();
    expect((c.socket as unknown as RaceSocket).via).toBe("hub");
    c.stop();
  });

  it("the first to open plays; the others are closed and never hear the hello", async () => {
    const c = client(raceTransport(tries(["lan", "hub"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(LOCAL_HEAD_START_MS);
    // The LAN try still going (its ICE slow), the hub's started too: the hub opens first.
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan", "hub"]);
    of("hub")[0].open();
    expect(of("lan")[0].closed).toBe(true);
    expect(of("lan")[0].sent).toEqual([]);
    expect(of("hub")[0].sent).toHaveLength(1);
    c.stop();
  });

  it("a drop reconnects by racing again, the way that worked first", async () => {
    const c = client(raceTransport(tries(["lan", "hub"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    of("lan")[0].fail("no_hub");
    of("hub")[0].open();
    of("hub")[0].msg(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false }));
    // The channel drops: reconnecting, with the resume token, and the hub tried at once.
    of("hub")[0].fail(null);
    expect(stages[stages.length - 1].kind).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(300);
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan", "hub", "lan", "hub"]);
    // Back home meanwhile: the LAN try opens and plays, with the resume token.
    of("lan")[1].open();
    expect(of("hub")[1].closed).toBe(true);
    expect(JSON.parse(String(of("lan")[1].sent[0]))).toEqual({ hello: { name: "Ben", invite: KEY, resume: "T" } });
    c.stop();
  });

  it("every way failed: the session ends with the hub's reason", async () => {
    const c = client(raceTransport(tries(["lan", "hub"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    of("lan")[0].fail("mismatch");
    of("hub")[0].fail("no_room");
    await vi.advanceTimersByTimeAsync(0);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "no_room" });
  });

  it("a Wi-Fi link: WebRTC first, the plain socket only when it can't connect", async () => {
    const c = client(raceTransport(tries(["lan", "socket"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan"]);
    // ICE slow: the socket after its wait…
    await vi.advanceTimersByTimeAsync(SOCKET_AFTER_MS);
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan", "socket"]);
    of("socket")[0].open();
    expect(of("lan")[0].closed).toBe(true);
    // …and on a reconnect the socket goes at once (it is what worked).
    of("socket")[0].msg(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false }));
    of("socket")[0].fail(null);
    await vi.advanceTimersByTimeAsync(300);
    expect(FakeTry.all.map((t) => t.via)).toEqual(["lan", "socket", "lan", "socket"]);
    c.stop();
  });

  it("WebRTC failed outright (an older host, no UDP): the socket at once", async () => {
    const c = client(raceTransport(tries(["lan", "socket"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    of("lan")[0].fail("no_hub");
    expect(of("socket")).toHaveLength(1);
    c.stop();
  });

  it("another certificate on a pinned try: never the socket (it would hand over the key)", async () => {
    const pinned = (vias: SignalVia[]) => () => vias.map((via) => ({ via, make: () => new FakeTry(via), pinned: via !== "socket" }));
    const c = client(raceTransport(pinned(["lan", "socket"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    of("lan")[0].fail("mismatch");
    await vi.advanceTimersByTimeAsync(SOCKET_AFTER_MS * 2);
    expect(of("socket")).toHaveLength(0);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "mismatch" });
    // An internet link: the hub (pinned too) still goes on; the socket never.
    FakeTry.all = [];
    stages = [];
    forgetPaths();
    const c2 = client(raceTransport(pinned(["lan", "hub", "socket"]), { room: ROOM }));
    c2.start();
    await vi.advanceTimersByTimeAsync(0);
    of("lan")[0].fail("mismatch");
    expect(of("hub")).toHaveLength(1);
    of("hub")[0].fail("no_hub");
    await vi.advanceTimersByTimeAsync(SOCKET_AFTER_MS * 2);
    expect(of("socket")).toHaveLength(0);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "no_hub" });
  });

  it("leaving closes every try", async () => {
    const c = client(raceTransport(tries(["lan", "hub"]), { room: ROOM }));
    c.start();
    await vi.advanceTimersByTimeAsync(LOCAL_HEAD_START_MS);
    c.stop();
    expect(FakeTry.all.every((t) => t.closed)).toBe(true);
  });
});

// ── A local try is the same connection: the answer's certificate checked ───────────────────────

class FakeSig {
  static all: FakeSig[] = [];
  readyState = 0;
  sent: Array<Record<string, unknown>> = [];
  onopen: (() => void) | null = null;
  onclose: ((e?: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeSig.all.push(this);
  }
  send(d: string) {
    this.sent.push(JSON.parse(d) as Record<string, unknown>);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  say(v: unknown) {
    this.onmessage?.({ data: JSON.stringify(v) });
  }
}

class FakeChan {
  readyState = "connecting";
  binaryType = "blob";
  sent: unknown[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  send(d: unknown) {
    this.sent.push(d);
  }
  close() {
    this.readyState = "closed";
  }
  open() {
    this.readyState = "open";
    this.onopen?.();
  }
}

class FakePeer {
  static all: FakePeer[] = [];
  channel = new FakeChan();
  remote: RTCSessionDescriptionInit | null = null;
  localDescription: { type: string; sdp: string } | null = null;
  closed = false;
  onicecandidate: unknown = null;
  oniceconnectionstatechange: unknown = null;
  constructor() {
    FakePeer.all.push(this);
  }
  createDataChannel() {
    return this.channel;
  }
  async createOffer() {
    return { type: "offer", sdp: "v=0\r\nOFFER\r\n" };
  }
  async setLocalDescription(d: { type: string; sdp: string }) {
    this.localDescription = d;
  }
  async setRemoteDescription(d: RTCSessionDescriptionInit) {
    this.remote = d;
  }
  async addIceCandidate() {}
  close() {
    this.closed = true;
  }
  async getStats() {
    return [
      { type: "transport", id: "T", selectedCandidatePairId: "P" },
      { type: "candidate-pair", id: "P", localCandidateId: "L", remoteCandidateId: "R" },
      { type: "local-candidate", id: "L", address: "abc.local" },
      { type: "remote-candidate", id: "R", address: "192.168.1.20" },
    ];
  }
}

describe("a try signalled through the host's engine", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeSig.all = [];
    FakePeer.all = [];
    forgetPaths();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const rtcLogs: string[] = [];
  const SIG = joinRelayUrl("http://127.0.0.1:19113", "ws://192.168.1.20:19113/api/join/signal");
  const make = (signalUrl?: string) =>
    rtcTransport(INV, {
      WebSocketImpl: FakeSig as unknown as typeof WebSocket,
      PeerConnection: FakePeer as unknown as typeof RTCPeerConnection,
      ...(signalUrl ? { signalUrl } : {}),
      log: (m: string) => rtcLogs.push(m),
    });
  const answer = (fp: string) => ({ t: "signal", from: "host", data: { type: "answer", sdp: `v=0\r\na=fingerprint:sha-256 ${fp}\r\n` } });

  it("signals there, checks the link's fingerprint, and plays on the home network", async () => {
    const stages: JoinStage[] = [];
    const logs: string[] = [];
    const L = make(SIG);
    const H = make();
    const T = raceTransport(
      () => [
        { via: "lan", make: () => new L("") as unknown as JoinSocket },
        { via: "hub", make: () => new H("") as unknown as JoinSocket },
      ],
      { room: ROOM, log: (m) => logs.push(m) },
    );
    const c = new JoinClient({
      url: "ws://unused",
      invite: KEY,
      name: "Ben",
      readPad: () => ({ ...IDLE_PAD }),
      onStage: (s) => stages.push(s),
      storage: null,
      WebSocketImpl: T,
      raf: () => 0,
      cancelRaf: () => {},
    });
    c.start();
    await vi.advanceTimersByTimeAsync(1);
    await vi.advanceTimersByTimeAsync(1);
    const sig = FakeSig.all[0];
    expect(sig?.url, [...logs, ...rtcLogs].join("\n")).toBe(SIG);
    sig.readyState = 1;
    sig.onopen?.();
    expect(sig.sent[0]).toEqual({ v: 1, t: "join", room: ROOM });
    sig.say({ t: "joined", gid: "lan-x" });
    await vi.advanceTimersByTimeAsync(1);
    // Another certificate on the home network: refused like the hub's would be.
    sig.say(answer(FP.replace(/^../, "FF")));
    await vi.advanceTimersByTimeAsync(1);
    expect(FakePeer.all[0].remote).toBeNull();
    expect(FakePeer.all[0].closed).toBe(true);
    // Not the end: the hub's try goes on at once, and its answer is the host's.
    expect(stages.some((s) => s.kind === "ended")).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    const hub = FakeSig.all[1];
    expect(hub.url).toBe("wss://hub.example.com/ws");
    hub.readyState = 1;
    hub.onopen?.();
    hub.say({ t: "joined", gid: "G" });
    await vi.advanceTimersByTimeAsync(1);
    hub.say(answer(FP));
    await vi.advanceTimersByTimeAsync(1);
    FakePeer.all[1].channel.open();
    // The key only ever went on the checked channel.
    expect(FakePeer.all[0].channel.sent).toEqual([]);
    expect(String(FakePeer.all[1].channel.sent[0])).toContain(KEY);
    expect(sig.sent.some((m) => JSON.stringify(m).includes(KEY))).toBe(false);
    expect(await (c.socket as unknown as RaceSocket).netPath()).toBe("lan");
    c.stop();
  });
});
