// The phone page in its internet mode (the website's /pad/ with #room=…&k=…&fp=…&hub=…): the
// pad link's session (lib/padClient) over join/rtcTransport with a "pad" channel, against fakes of
// the hub socket and RTCPeerConnection. The engine's side is join_net/pad_tests.rs.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { framePieces, rtcTransport, type InternetInvite } from "../join/rtcTransport";
import { PadClient, type PadLinkStage } from "../lib/padClient";
import { IDLE_PAD, type PadState } from "../lib/remotePlayStream";
import { netText } from "./PadApp";

const FP = Array.from({ length: 32 }, (_, i) => (i * 7 + 16).toString(16).toUpperCase().padStart(2, "0")).join(":");
const ROOM = "AbCdEfGhIjKlMnOpQrStUv";
const KEY = "00112233445566778899aabbccddeeff";
const DEVICE = "ffeeddccbbaa99887766554433221100";
const INV: InternetInvite = { room: ROOM, key: KEY, fp: FP, hub: "127.0.0.1:8787" };

class FakeHub {
  static all: FakeHub[] = [];
  readyState = 0;
  sent: Array<Record<string, unknown>> = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeHub.all.push(this);
  }
  send(d: string) {
    this.sent.push(JSON.parse(d) as Record<string, unknown>);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  say(v: unknown) {
    this.onmessage?.({ data: JSON.stringify(v) });
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
  get texts() {
    return this.sent.filter((s): s is string => typeof s === "string").map((s) => JSON.parse(s) as Record<string, unknown>);
  }
}

class FakePC {
  static all: FakePC[] = [];
  iceConnectionState = "new";
  channel: FakeChannel | null = null;
  closed = false;
  localDescription: { type: string; sdp: string } | null = null;
  onicecandidate: (() => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  constructor(readonly config: RTCConfiguration) {
    FakePC.all.push(this);
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
  async setRemoteDescription() {}
  async addIceCandidate() {}
  close() {
    this.closed = true;
  }
  ice(s: string) {
    this.iceConnectionState = s;
    this.oniceconnectionstatechange?.();
  }
}

const answer = { type: "answer", sdp: `v=0\r\na=fingerprint:sha-256 ${FP}\r\n` };
// English, as tr falls back without a language loaded.
const tr = ((_k: string, vars?: unknown, fallback?: string) => (typeof vars === "string" ? vars : (fallback ?? _k))) as Parameters<typeof netText>[0];

describe("the phone page over the internet", () => {
  let stages: PadLinkStage[];
  let touch: PadState;
  const last = () => stages[stages.length - 1];

  beforeEach(() => {
    vi.useFakeTimers();
    FakeHub.all = [];
    FakePC.all = [];
    stages = [];
    touch = { ...IDLE_PAD };
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  function phone(watch?: { onFrame: (d: ArrayBuffer) => void }) {
    const transport = rtcTransport(INV, {
      label: "pad",
      WebSocketImpl: FakeHub as unknown as typeof WebSocket,
      PeerConnection: FakePC as unknown as typeof RTCPeerConnection,
    });
    const c = new PadClient({
      url: "ws://127.0.0.1:8787/ws",
      WebSocketImpl: transport,
      hello: { t: KEY, name: "android", d: DEVICE, ...(watch ? { w: 1 } : {}) },
      onStage: (s) => stages.push(s),
      touchPad: () => touch,
      watch: watch && { onFrame: watch.onFrame, onCodec: () => {}, onControl: () => {} },
      now: () => Date.now(),
      raf: () => 0,
      cancelRaf: () => {},
    });
    c.start();
    return c;
  }

  /** Hub joined, offer made, answer in: the channel, not open yet. */
  async function connect(n: number) {
    await vi.advanceTimersByTimeAsync(0);
    const hub = FakeHub.all[n];
    hub.open();
    expect(hub.sent[0]).toEqual({ v: 1, t: "join", room: ROOM });
    hub.say({ t: "joined", gid: "G" });
    await vi.advanceTimersByTimeAsync(0);
    const pc = FakePC.all[n];
    hub.say({ t: "signal", from: "host", data: answer });
    await vi.advanceTimersByTimeAsync(0);
    return { hub, pc, dc: pc.channel! };
  }

  it("opens a pad channel, says its hello first, then plays: touchpad and motion out, rumble back", async () => {
    const vibrate = vi.fn();
    vi.stubGlobal("navigator", { vibrate });
    const c = phone();
    const { dc } = await connect(0);
    // The phone link's own channel, reliable and ordered as Play Together's.
    expect(dc.label).toBe("pad");
    expect(dc.init).toEqual({ ordered: true });
    dc.open();
    expect(dc.texts[0]).toEqual({ hello: { t: KEY, name: "android", d: DEVICE } });
    expect(dc.texts[1].pad).toBeTruthy();
    // The computer allows an internet phone first; waiting is not silence.
    dc.recv(JSON.stringify({ stage: "asking" }));
    expect(last()).toEqual({ kind: "asking" });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(last()).toEqual({ kind: "asking" });
    dc.recv(JSON.stringify({ stage: "joined", session: { host: "10.0.0.5", playing: true } }));
    expect(last()).toEqual({ kind: "joined", host: "10.0.0.5", playing: true, passcode: false });

    // The touchpad (and the rest of the pad) on every change.
    touch = { ...IDLE_PAD, buttons: 1, touch: [960, 470] };
    c.sendPad();
    const pads = dc.texts.filter((t) => t.pad).map((t) => t.pad as PadState);
    expect(pads[pads.length - 1]).toMatchObject({ buttons: 1, touch: [960, 470] });

    // The console's rumble buzzes the phone.
    dc.recv(JSON.stringify({ rumble: [200, 0] }));
    expect(vibrate).toHaveBeenCalledWith(250);
    c.stop();
  });

  it("a watcher says w=1 and gets the picture whole from its pieces", async () => {
    const frames: ArrayBuffer[] = [];
    const c = phone({ onFrame: (d) => frames.push(d) });
    const { dc } = await connect(0);
    dc.open();
    expect(dc.texts[0]).toEqual({ hello: { t: KEY, name: "android", d: DEVICE, w: 1 } });
    dc.recv(JSON.stringify({ stage: "joined", session: { host: "10.0.0.5", playing: true } }));
    const pic = Uint8Array.from({ length: 40_000 }, (_, i) => i & 0xff);
    for (const p of framePieces(pic)) dc.recv(p);
    expect(frames).toHaveLength(1);
    expect(new Uint8Array(frames[0])).toEqual(pic);
    c.stop();
  });

  it("a link that can't connect says why in words, and stops", async () => {
    phone();
    const { pc } = await connect(0);
    pc.ice("failed");
    await vi.advanceTimersByTimeAsync(0);
    expect(last()).toEqual({ kind: "refused", why: "net:no_direct" });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(FakeHub.all).toHaveLength(1);
    expect(netText(tr, "no_direct")).toContain("A direct connection isn't possible");
    expect(netText(tr, "mismatch")).toContain("doesn't match");
    expect(netText(tr, "no_room")).toContain("expired");
    expect(netText(tr, "net_full")).toContain("Four phones");
  });

  it("told No, or nobody answered: said so, and no second try", async () => {
    for (const why of ["denied", "no_answer"]) {
      FakeHub.all = [];
      FakePC.all = [];
      stages = [];
      phone();
      const { dc } = await connect(0);
      dc.open();
      dc.recv(JSON.stringify({ stage: "asking" }));
      dc.recv(JSON.stringify({ stage: "refused", why }));
      expect(last()).toEqual({ kind: "refused", why });
      await vi.advanceTimersByTimeAsync(10_000);
      expect(FakeHub.all).toHaveLength(1);
    }
  });

  it("a dropped channel comes back through the hub with the hello again", async () => {
    const c = phone();
    const first = await connect(0);
    first.dc.open();
    first.dc.recv(JSON.stringify({ stage: "joined", session: { host: "h", playing: true } }));
    first.dc.close();
    await vi.advanceTimersByTimeAsync(0);
    expect(last().kind).toBe("reconnecting");
    // The next try (a new hub connection, a new offer) after the first backoff.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(FakeHub.all).toHaveLength(2);
    const again = await connect(1);
    again.dc.open();
    expect(again.dc.texts[0]).toEqual({ hello: { t: KEY, name: "android", d: DEVICE } });
    c.stop();
  });
});
