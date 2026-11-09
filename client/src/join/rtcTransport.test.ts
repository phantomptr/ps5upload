import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { JoinClient, type JoinStage, type JoinStorage } from "../lib/joinClient";
import { IDLE_PAD } from "../lib/remotePlayStream";
import { endText } from "./JoinView";
import {
  DC_MAX,
  framePieces,
  fingerprintMatches,
  HUB_BACK_FOR_MS,
  HUB_BACK_WAIT_MS,
  hubSocketUrl,
  ICE_TIMEOUT_MS,
  LAST,
  MORE,
  NET_CLOSE,
  netFailure,
  parseInternetFragment,
  PING,
  PONG,
  Reassembler,
  rtcTransport,
  sdpFingerprint,
  STUN_SERVERS,
  trackCodecs,
  type InternetInvite,
} from "./rtcTransport";

const FP = Array.from({ length: 32 }, (_, i) => (i * 7 + 16).toString(16).toUpperCase().padStart(2, "0")).join(":");
const ROOM = "AbCdEfGhIjKlMnOpQrStUv";
const KEY = "00112233445566778899aabbccddeeff";

describe("internet invite link", () => {
  it("reads room, key, fingerprint and hub from the fragment", () => {
    const inv = parseInternetFragment(`#room=${ROOM}&k=${KEY}&fp=${FP.toLowerCase()}&hub=hub.phantomptr.com`);
    expect(inv).toEqual({ room: ROOM, key: KEY, fp: FP, hub: "hub.phantomptr.com" });
    expect(parseInternetFragment(`room=${ROOM}&k=${KEY}&fp=${FP}&hub=127.0.0.1:8787`)?.hub).toBe("127.0.0.1:8787");
    expect(parseInternetFragment(`#room=${ROOM}&k=${KEY}&fp=${FP}&hub=[::1]:8787`)?.hub).toBe("[::1]:8787");
  });

  it("is not one without every part, or with a part that could point anywhere else", () => {
    const ok = { room: ROOM, k: KEY, fp: FP, hub: "hub.phantomptr.com" };
    const frag = (o: Record<string, string>) => `#${new URLSearchParams(o)}`;
    expect(parseInternetFragment("#i=00112233445566778899aabbccddeeff&n=Ann")).toBeNull();
    for (const k of Object.keys(ok)) {
      const missing: Record<string, string> = { ...ok };
      delete missing[k];
      expect(parseInternetFragment(frag(missing)), k).toBeNull();
    }
    for (const bad of [
      { room: "short" },
      { room: `${ROOM}/` },
      { k: "a b" },
      { fp: FP.slice(3) },
      { fp: FP.replace(/:/g, "") },
      { hub: "evil.example/ws?x=" },
      { hub: "user@evil.example" },
      { hub: "wss://evil.example" },
      { hub: "" },
    ]) {
      expect(parseInternetFragment(frag({ ...ok, ...bad })), JSON.stringify(bad)).toBeNull();
    }
  });

  it("uses wss:// for the hub, ws:// only to this computer", () => {
    expect(hubSocketUrl("hub.phantomptr.com")).toBe("wss://hub.phantomptr.com/ws");
    expect(hubSocketUrl("hub.example:9000")).toBe("wss://hub.example:9000/ws");
    expect(hubSocketUrl("127.0.0.1:8787")).toBe("ws://127.0.0.1:8787/ws");
    expect(hubSocketUrl("localhost:8787")).toBe("ws://localhost:8787/ws");
    expect(hubSocketUrl("[::1]:8787")).toBe("ws://[::1]:8787/ws");
    expect(hubSocketUrl("127.0.0.1.evil.example")).toBe("wss://127.0.0.1.evil.example/ws");
  });
});

describe("the answer's fingerprint", () => {
  const sdp = (lines: string[]) => ["v=0", "o=- 1 2 IN IP4 0.0.0.0", "s=-", ...lines, "m=application 9 UDP/DTLS/SCTP webrtc-datachannel", ""].join("\r\n");

  it("is read from a=fingerprint:sha-256 and compared without case", () => {
    const s = sdp([`a=fingerprint:sha-256 ${FP.toLowerCase()}`]);
    expect(sdpFingerprint(s)).toBe(FP);
    expect(fingerprintMatches(s, FP)).toBe(true);
    expect(fingerprintMatches(s, FP.toLowerCase())).toBe(true);
    // Session level and media level, the same certificate: fine.
    expect(fingerprintMatches(sdp([`a=fingerprint:sha-256 ${FP}`, `a=fingerprint:SHA-256 ${FP.toLowerCase()}`]), FP)).toBe(true);
  });

  it("refuses another certificate, none, two, or another hash", () => {
    const other = FP.replace(/^../, "FF");
    expect(fingerprintMatches(sdp([`a=fingerprint:sha-256 ${other}`]), FP)).toBe(false);
    expect(fingerprintMatches(sdp([]), FP)).toBe(false);
    expect(fingerprintMatches(sdp([`a=fingerprint:sha-256 ${FP}`, `a=fingerprint:sha-256 ${other}`]), FP)).toBe(false);
    expect(fingerprintMatches(sdp([`a=fingerprint:sha-256 ${FP}`, `a=fingerprint:sha-1 AA:BB`]), FP)).toBe(false);
    expect(fingerprintMatches(sdp([`a=fingerprint:sha-256 ${FP}x`]), FP)).toBe(false);
  });
});

describe("data-channel framing (join_net/chunk.rs)", () => {
  it("cuts messages of every size into pieces of at most 16 KiB and puts them back whole", () => {
    for (const len of [0, 1, DC_MAX - 2, DC_MAX - 1, DC_MAX, 3 * (DC_MAX - 1), 3 * (DC_MAX - 1) + 1, 200_000]) {
      const msg = Uint8Array.from({ length: len }, (_, i) => (i * 7 + len) & 0xff);
      const parts = framePieces(msg);
      expect(parts.length, String(len)).toBe(Math.max(1, Math.ceil(len / (DC_MAX - 1))));
      expect(parts.every((p) => p.length <= DC_MAX)).toBe(true);
      expect(parts.slice(0, -1).every((p) => p[0] === MORE)).toBe(true);
      expect(parts[parts.length - 1][0]).toBe(LAST);
      const r = new Reassembler();
      let got: ArrayBuffer | null = null;
      parts.forEach((p, i) => {
        const piece = r.push(p.buffer);
        if (piece.kind === "partial") expect(i).toBeLessThan(parts.length - 1);
        else if (piece.kind === "message") got = piece.data;
        else throw new Error(piece.kind);
      });
      expect(new Uint8Array(got!)).toEqual(msg);
    }
  });

  it("a pad frame goes as one piece behind 0x00", () => {
    const pad = Uint8Array.from([1, 2, 3, 4, 5, 6]);
    expect(framePieces(pad)).toEqual([Uint8Array.from([LAST, 1, 2, 3, 4, 5, 6])]);
  });

  it("pings and pongs carry eight bytes; a bad header or an oversize message is refused", () => {
    const r = new Reassembler(100);
    const eight = [1, 2, 3, 4, 5, 6, 7, 8];
    expect(r.push(Uint8Array.from([PING, ...eight]))).toEqual({ kind: "ping", data: Uint8Array.from(eight) });
    expect(r.push(Uint8Array.from([PONG, ...eight]))).toEqual({ kind: "pong", data: Uint8Array.from(eight) });
    expect(r.push(Uint8Array.from([PING, 1, 2]))).toEqual({ kind: "error", why: "ping_size" });
    expect(r.push(Uint8Array.from([0x7f, 1]))).toEqual({ kind: "error", why: "header" });
    expect(r.push(new Uint8Array(0))).toEqual({ kind: "error", why: "empty" });
    const half = new Uint8Array(61);
    half[0] = MORE;
    expect(r.push(half).kind).toBe("partial");
    expect(r.push(half)).toEqual({ kind: "error", why: "too_big" });
    // Starts over clean after a refusal.
    expect(new Uint8Array((r.push(Uint8Array.from([LAST, 9])) as { data: ArrayBuffer }).data)).toEqual(Uint8Array.from([9]));
  });
});

describe("what the guest is told", () => {
  const tr = ((_key: string, vars?: Record<string, string | number> | string, fallback?: string) => {
    const text = typeof vars === "string" ? vars : (fallback ?? _key);
    const v = typeof vars === "object" ? vars : {};
    return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in v ? String(v[k]) : m));
  }) as Parameters<typeof endText>[0];

  it("no direct path: the networks, Wi-Fi, UPnP", () => {
    expect(endText(tr, { kind: "ended", why: "no_direct" }, "Ann")).toBe(
      "A direct connection isn't possible from this network (both sides' routers are strict). Try Wi-Fi instead of mobile data, or ask the host to turn on UPnP.",
    );
  });

  it("a link that is not the host's", () => {
    expect(endText(tr, { kind: "ended", why: "mismatch" }, "Ann")).toBe("This link doesn't match the host; ask them for a new one.");
    expect(endText(tr, { kind: "ended", why: "no_room" }, "Ann")).toContain("Ask Ann for a new link");
  });

  it("a channel dropped while waiting: the internet advice, not the LAN's", () => {
    expect(endText(tr, { kind: "ended", why: "lost" }, "Ann", true)).toBe(
      "The connection to Ann dropped. Check your internet connection, then ask again.",
    );
    expect(endText(tr, { kind: "ended", why: "lost" }, "Ann")).toContain("same network");
  });

  it("a close carries its reason only with the transport's code", () => {
    expect(netFailure({ code: NET_CLOSE, reason: "net:no_direct" })).toBe("no_direct");
    expect(netFailure({ code: 1006, reason: "net:no_direct" })).toBeNull();
    expect(netFailure({ code: NET_CLOSE, reason: "net:whatever" })).toBeNull();
    expect(netFailure(undefined)).toBeNull();
  });
});

// ── The transport against fakes of the hub socket and RTCPeerConnection ────────────────────────

class FakeHub {
  static all: FakeHub[] = [];
  readyState = 0;
  sent: Array<Record<string, unknown>> = [];
  closed = false;
  onopen: (() => void) | null = null;
  onclose: ((e?: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeHub.all.push(this);
  }
  send(d: string) {
    this.sent.push(JSON.parse(d) as Record<string, unknown>);
  }
  close() {
    if (this.closed) return;
    this.closed = true;
    this.readyState = 3;
    this.onclose?.();
  }
  /** The hub closing the socket with a code (1001: it is shutting down). */
  closeWith(code: number) {
    if (this.closed) return;
    this.closed = true;
    this.readyState = 3;
    this.onclose?.({ code });
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  say(v: unknown) {
    this.onmessage?.({ data: JSON.stringify(v) });
  }
  signals(type: string) {
    return this.sent.filter((m) => m.t === "signal" && (m.data as { type?: string }).type === type);
  }
}

class FakeChannel {
  readyState = "connecting";
  binaryType = "blob";
  sent: Array<string | Uint8Array> = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  constructor(
    readonly label: string,
    readonly init: RTCDataChannelInit | undefined,
  ) {}
  send(d: string | Uint8Array) {
    this.sent.push(typeof d === "string" ? d : Uint8Array.from(d));
  }
  close() {
    if (this.readyState === "closed") return;
    this.readyState = "closed";
    this.onclose?.();
  }
  open() {
    this.readyState = "open";
    this.onopen?.();
  }
  recv(d: string | Uint8Array) {
    this.onmessage?.({ data: typeof d === "string" ? d : d.slice().buffer });
  }
  get binary() {
    return this.sent.filter((s): s is Uint8Array => typeof s !== "string");
  }
  get texts() {
    return this.sent.filter((s): s is string => typeof s === "string").map((s) => JSON.parse(s) as Record<string, unknown>);
  }
}

class FakePC {
  static all: FakePC[] = [];
  iceConnectionState = "new";
  channel: FakeChannel | null = null;
  remote: RTCSessionDescriptionInit | null = null;
  candidates: RTCIceCandidateInit[] = [];
  localDescription: { type: string; sdp: string } | null = null;
  closed = false;
  onicecandidate: ((e: { candidate: { candidate: string; sdpMid: string | null } | null }) => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  ontrack: ((e: unknown) => void) | null = null;
  /** The video transceivers the page added: kind, init, the codecs it preferred. */
  transceivers: Array<{ kind: string; init: unknown; prefs: unknown[] | null }> = [];
  constructor(readonly config: RTCConfiguration) {
    FakePC.all.push(this);
  }
  addTransceiver(kind: string, init: unknown) {
    const t = { kind, init, prefs: null as unknown[] | null };
    this.transceivers.push(t);
    return { setCodecPreferences: (c: unknown[]) => (t.prefs = c) };
  }
  createDataChannel(label: string, init?: RTCDataChannelInit) {
    this.channel = new FakeChannel(label, init);
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
  async addIceCandidate(c: RTCIceCandidateInit) {
    this.candidates.push(c);
  }
  close() {
    this.closed = true;
  }
  ice(s: string) {
    this.iceConnectionState = s;
    this.oniceconnectionstatechange?.();
  }
}

const INV: InternetInvite = { room: ROOM, key: KEY, fp: FP, hub: "127.0.0.1:8787" };
const answer = (fp = FP) => ({ type: "answer", sdp: `v=0\r\na=fingerprint:sha-256 ${fp}\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n` });

function transport(video = false) {
  return rtcTransport(INV, {
    WebSocketImpl: FakeHub as unknown as typeof WebSocket,
    PeerConnection: FakePC as unknown as typeof RTCPeerConnection,
    video,
  });
}

class MemStorage implements JoinStorage {
  m = new Map<string, string>();
  getItem(k: string) {
    return this.m.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.m.set(k, v);
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
}

describe("the WebRTC transport", () => {
  let stages: JoinStage[];
  let frames: ArrayBuffer[];

  beforeEach(() => {
    vi.useFakeTimers();
    FakeHub.all = [];
    FakePC.all = [];
    stages = [];
    frames = [];
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function client(storage = new MemStorage()) {
    return new JoinClient({
      url: "ws://127.0.0.1:8787/ws",
      invite: KEY,
      name: "Ben",
      readPad: () => ({ ...IDLE_PAD }),
      onStage: (s) => stages.push(s),
      onFrame: (d) => frames.push(d),
      storage,
      WebSocketImpl: transport(),
      now: () => Date.now(),
      raf: () => 0,
      cancelRaf: () => {},
    });
  }

  /** Hub joined, offer made, answer in, channel open: the session's hello is on the channel. */
  async function connect(n: number) {
    await vi.advanceTimersByTimeAsync(0);
    const hub = FakeHub.all[n];
    expect(hub.url).toBe("ws://127.0.0.1:8787/ws");
    hub.open();
    expect(hub.sent[0]).toEqual({ v: 1, t: "join", room: ROOM });
    hub.say({ t: "joined", gid: "G" });
    await vi.advanceTimersByTimeAsync(0);
    const pc = FakePC.all[n];
    return { hub, pc, dc: pc.channel! };
  }

  it("offers over the hub, checks the answer, and runs the session over the channel", async () => {
    const c = client();
    c.start();
    const { hub, pc, dc } = await connect(0);
    expect(pc.config.iceServers).toEqual([{ urls: STUN_SERVERS }]);
    expect(dc.label).toBe("join");
    expect(dc.init).toEqual({ ordered: true });
    expect(dc.binaryType).toBe("arraybuffer");
    expect(hub.signals("offer")).toEqual([{ t: "signal", to: "host", data: { type: "offer", sdp: "v=0\r\nOFFER\r\n" } }]);
    pc.onicecandidate?.({ candidate: { candidate: "candidate:1 1 udp 1 192.0.2.1 5000 typ host", sdpMid: "0" } });
    pc.onicecandidate?.({ candidate: null });
    expect(hub.signals("candidate")).toEqual([
      { t: "signal", to: "host", data: { type: "candidate", candidate: "candidate:1 1 udp 1 192.0.2.1 5000 typ host", sdpMid: "0" } },
    ]);
    // A candidate before the answer waits for it.
    hub.say({ t: "signal", from: "host", data: { type: "candidate", candidate: "candidate:2 1 udp 1 198.51.100.7 6000 typ srflx", sdpMid: "0", sdpMLineIndex: 0 } });
    hub.say({ t: "signal", from: "host", data: answer(FP.toLowerCase()) });
    await vi.advanceTimersByTimeAsync(0);
    expect(pc.remote?.type).toBe("answer");
    expect(pc.candidates.map((x) => x.candidate)).toEqual(["candidate:2 1 udp 1 198.51.100.7 6000 typ srflx"]);

    dc.open();
    // Open: the hub is left (the media is peer to peer), the hello goes on the channel.
    expect(hub.sent[hub.sent.length - 1]).toEqual({ t: "leave" });
    expect(dc.texts[0]).toEqual({ hello: { name: "Ben", invite: KEY } });
    dc.recv(JSON.stringify({ stage: "asking" }));
    expect(stages[stages.length - 1]).toEqual({ kind: "asking" });
    dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "TOKEN-1", hevc: false }));
    expect(stages[stages.length - 1]).toEqual({ kind: "joined", player: 2, hevc: false });

    // The engine's ping is answered at once with the same 8 bytes.
    dc.recv(Uint8Array.from([PING, 1, 2, 3, 4, 5, 6, 7, 8]));
    expect(dc.binary.some((b) => b.length === 9 && b[0] === PONG && b[8] === 8)).toBe(true);
    // A picture in three pieces comes out whole.
    const pic = Uint8Array.from({ length: 40_000 }, (_, i) => i & 0xff);
    for (const p of framePieces(pic)) dc.recv(p);
    expect(frames).toHaveLength(1);
    expect(new Uint8Array(frames[0])).toEqual(pic);
    // The pad goes behind 0x00.
    c.sendPad(true);
    const pad = dc.binary.filter((b) => b[0] === LAST);
    expect(pad.length).toBeGreaterThan(0);
    expect(pad[pad.length - 1][1]).toBe(0x01);
    c.stop();
  });

  it("refuses an answer with another certificate: everything closed, the link's error", async () => {
    const storage = new MemStorage();
    const c = client(storage);
    c.start();
    const { hub, pc } = await connect(0);
    hub.say({ t: "signal", from: "host", data: answer(FP.replace(/^../, "FF")) });
    await vi.advanceTimersByTimeAsync(0);
    expect(pc.remote).toBeNull();
    expect(pc.closed).toBe(true);
    expect(hub.closed).toBe(true);
    expect(hub.sent[hub.sent.length - 1]).toEqual({ t: "leave" });
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "mismatch" });
    // No second try.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeHub.all).toHaveLength(1);
  });

  it("ICE that does not connect in time: the network message, and the hub is told", async () => {
    const c = client();
    c.start();
    const { hub, pc } = await connect(0);
    hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(ICE_TIMEOUT_MS - 100);
    expect(stages[stages.length - 1]).toEqual({ kind: "connecting" });
    await vi.advanceTimersByTimeAsync(200);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "no_direct" });
    expect(hub.sent[hub.sent.length - 1]).toEqual({ t: "leave" });
    expect(pc.closed).toBe(true);
  });

  it("ICE failed: the same message at once", async () => {
    const c = client();
    c.start();
    const { hub, pc } = await connect(0);
    hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(0);
    pc.ice("failed");
    await vi.advanceTimersByTimeAsync(0);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "no_direct" });
    expect(hub.sent[hub.sent.length - 1]).toEqual({ t: "leave" });
  });

  it("a hub that restarts (1001) before the answer: tried again until the host's room is back", async () => {
    const c = client();
    c.start();
    const first = await connect(0);
    first.hub.closeWith(1001);
    await vi.advanceTimersByTimeAsync(0);
    expect(first.pc.closed).toBe(true);
    expect(stages[stages.length - 1]).toEqual({ kind: "connecting" });
    // Back, but the host has not opened its room again yet: wait and try once more.
    await vi.advanceTimersByTimeAsync(HUB_BACK_WAIT_MS);
    expect(FakeHub.all).toHaveLength(2);
    FakeHub.all[1].open();
    expect(FakeHub.all[1].sent[0]).toEqual({ v: 1, t: "join", room: ROOM });
    FakeHub.all[1].say({ t: "error", why: "no_room" });
    await vi.advanceTimersByTimeAsync(HUB_BACK_WAIT_MS);
    expect(FakeHub.all).toHaveLength(3);
    const hub = FakeHub.all[2];
    hub.open();
    hub.say({ t: "joined", gid: "G2" });
    await vi.advanceTimersByTimeAsync(0);
    // The try that met no room made no peer connection: this is the second one.
    expect(FakePC.all).toHaveLength(2);
    const pc = FakePC.all[1];
    const dc = pc.channel!;
    expect(hub.signals("offer")).toHaveLength(1);
    hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(0);
    expect(pc.remote?.type).toBe("answer");
    dc.open();
    expect(dc.texts[0]).toEqual({ hello: { name: "Ben", invite: KEY } });
    c.stop();
  });

  it("a hub that stays away, or closes without 1001: the hub message", async () => {
    const c = client();
    c.start();
    const { hub } = await connect(0);
    hub.closeWith(1001);
    for (let n = 1; ; n++) {
      await vi.advanceTimersByTimeAsync(HUB_BACK_WAIT_MS);
      const last = stages[stages.length - 1];
      if (last.kind === "ended") break;
      expect(n * HUB_BACK_WAIT_MS).toBeLessThanOrEqual(HUB_BACK_FOR_MS + HUB_BACK_WAIT_MS);
      // Each try's socket goes away the same way.
      FakeHub.all[FakeHub.all.length - 1].closeWith(1001);
    }
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "no_hub" });

    FakeHub.all = [];
    FakePC.all = [];
    stages = [];
    const c2 = client();
    c2.start();
    const two = await connect(0);
    two.hub.closeWith(1006);
    await vi.advanceTimersByTimeAsync(0);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "no_hub" });
  });

  it("the hub gone after the answer: ICE carries on without it", async () => {
    const c = client();
    c.start();
    const { hub, pc, dc } = await connect(0);
    hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(0);
    hub.closeWith(1001);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(pc.closed).toBe(false);
    dc.open();
    expect(dc.texts[0]).toEqual({ hello: { name: "Ben", invite: KEY } });
    c.stop();
  });

  it("the hub's answers: no room, full, the engine's bye", async () => {
    for (const [msg, why] of [
      [{ t: "error", why: "no_room" }, "no_room"],
      [{ t: "error", why: "room_full" }, "net_full"],
      [{ t: "signal", from: "host", data: { type: "bye", why: "full" } }, "net_full"],
      [{ t: "signal", from: "host", data: { type: "bye", why: "offer" } }, "offer"],
    ] as const) {
      FakeHub.all = [];
      FakePC.all = [];
      stages = [];
      const c = client();
      c.start();
      await connect(0);
      FakeHub.all[0].say(msg);
      await vi.advanceTimersByTimeAsync(0);
      expect(stages[stages.length - 1], JSON.stringify(msg)).toEqual({ kind: "ended", why });
    }
  });

  it("a channel drop re-signals through the hub and resumes with the token", async () => {
    const c = client();
    c.start();
    const first = await connect(0);
    first.hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(0);
    first.dc.open();
    first.dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "TOKEN-1", hevc: false }));
    first.dc.close();
    await vi.advanceTimersByTimeAsync(0);
    expect(stages[stages.length - 1].kind).toBe("reconnecting");
    // The next try: a new hub connection, a new offer, the hello with the resume token.
    await vi.advanceTimersByTimeAsync(300);
    const again = await connect(1);
    expect(again.hub.signals("offer")).toHaveLength(1);
    again.hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(0);
    again.dc.open();
    expect(again.dc.texts[0]).toEqual({ hello: { name: "Ben", invite: KEY, resume: "TOKEN-1" } });
    again.dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "TOKEN-1", hevc: false }));
    expect(stages[stages.length - 1]).toEqual({ kind: "joined", player: 2, hevc: false });
    c.stop();
  });
});

describe("the picture as a video track", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    FakeHub.all = [];
    FakePC.all = [];
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("prefers H.264, then H.265, with their resends; nothing else", () => {
    const c = (mimeType: string) => ({ mimeType, clockRate: 90000 });
    const caps = { codecs: [c("video/VP8"), c("video/rtx"), c("video/H265"), c("video/H264"), c("video/AV1")], headerExtensions: [] };
    expect(trackCodecs(caps).map((x) => x.mimeType)).toEqual(["video/H264", "video/H265", "video/rtx"]);
    expect(trackCodecs(null)).toEqual([]);
  });

  /** A MediaStream as far as the transport uses it. */
  const fakeStream = () => {
    const tracks: unknown[] = [];
    return { getTracks: () => tracks, addTrack: (t: unknown) => void tracks.push(t) };
  };

  /** A session over a transport that offers the track (`video`), up to the channel's open. */
  async function joinOver(video: boolean) {
    const got: Array<{ hevc: boolean; track: unknown; audio?: boolean }> = [];
    const c = new JoinClient({
      url: "ws://127.0.0.1:8787/ws",
      invite: KEY,
      name: "Ben",
      readPad: () => ({ ...IDLE_PAD }),
      onStage: () => {},
      onCodec: (hevc, track, audio) => got.push({ hevc, track, ...(track ? { audio } : {}) }),
      storage: new MemStorage(),
      WebSocketImpl: transport(video),
      now: () => Date.now(),
      raf: () => 0,
      cancelRaf: () => {},
    });
    c.start();
    await vi.advanceTimersByTimeAsync(0);
    const hub = FakeHub.all[0];
    hub.open();
    hub.say({ t: "joined", gid: "G" });
    await vi.advanceTimersByTimeAsync(0);
    const pc = FakePC.all[0];
    hub.say({ t: "signal", from: "host", data: answer() });
    await vi.advanceTimersByTimeAsync(0);
    return { c, pc, dc: pc.channel!, got };
  }

  it("offers a receive-only video section, and a newer engine's track plays it", async () => {
    const { c, pc, dc, got } = await joinOver(true);
    // The picture's section, and the sound's beside it (#720).
    expect(pc.transceivers.map((t) => [t.kind, t.init])).toEqual([
      ["video", { direction: "recvonly" }],
      ["audio", { direction: "recvonly" }],
    ]);
    // The answer's track: the browser's stream and receiver.
    const stream = fakeStream();
    const receiver = { getStats: async () => new Map() };
    const video = { kind: "video" };
    pc.ontrack?.({ streams: [stream], track: video, receiver });
    dc.open();
    dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false, track: true }));
    expect(got).toEqual([{ hevc: false, track: { stream, receiver }, audio: false }]);
    expect(stream.getTracks()).toEqual([video]);
    c.stop();
  });

  it("plays the sound from its audio track when the engine sends it there, in the picture's stream", async () => {
    const { c, pc, dc, got } = await joinOver(true);
    const stream = fakeStream();
    const receiver = { getStats: async () => new Map() };
    const sound = { getStats: async () => new Map() };
    const video = { kind: "video" };
    const audio = { kind: "audio" };
    pc.ontrack?.({ streams: [stream], track: video, receiver });
    // Its own stream id: still the one stream (one <video> plays both, in sync).
    pc.ontrack?.({ streams: [fakeStream()], track: audio, receiver: sound });
    dc.open();
    dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false, track: true, audio: true }));
    expect(got).toEqual([{ hevc: false, track: { stream, receiver, audio: sound }, audio: true }]);
    expect(stream.getTracks()).toEqual([video, audio]);
    c.stop();
  });

  it("an older engine (no `track` in joined) keeps the pictures on the channel, track or not", async () => {
    const { c, pc, dc, got } = await joinOver(true);
    // An older engine still answers the video section (str0m takes it) but never sends on it.
    pc.ontrack?.({ streams: [fakeStream()], track: { kind: "video" }, receiver: {} });
    dc.open();
    dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false }));
    expect(got).toEqual([{ hevc: false, track: null }]);
    c.stop();
  });

  it("an engine that says `track` with no track here (none answered) gets the channel's", async () => {
    const { c, dc, got } = await joinOver(true);
    dc.open();
    dc.recv(JSON.stringify({ stage: "joined", player: 2, token: "T", hevc: false, track: true }));
    expect(got).toEqual([{ hevc: false, track: null }]);
    c.stop();
  });

  it("the phone link's transport (no `video`) offers no track, as before", async () => {
    const { c, pc } = await joinOver(false);
    expect(pc.transceivers).toEqual([]);
    c.stop();
  });
});
