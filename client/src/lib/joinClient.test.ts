import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { JoinClient, tokenKey, type JoinStage, type JoinStorage } from "./joinClient";
import { IDLE_PAD, type PadState } from "./remotePlayStream";

/** A socket the test drives: what the page sent, and the engine's side. */
class FakeSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static all: FakeSocket[] = [];
  readyState = 0;
  binaryType = "blob";
  sent: Array<string | Uint8Array> = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.all.push(this);
  }
  send(d: string | Uint8Array) {
    this.sent.push(d);
  }
  close() {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.();
  }
  // The engine's side.
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  say(v: unknown) {
    this.onmessage?.({ data: JSON.stringify(v) });
  }
  drop() {
    this.close();
  }
  get texts() {
    return this.sent.filter((s): s is string => typeof s === "string").map((s) => JSON.parse(s) as Record<string, unknown>);
  }
  get frames() {
    return this.sent.filter((s): s is Uint8Array => typeof s !== "string");
  }
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

let now = 1_000_000;
let pad: PadState = { ...IDLE_PAD };
let stages: JoinStage[] = [];
let storage: MemStorage;

function client(extra: Partial<ConstructorParameters<typeof JoinClient>[0]> = {}) {
  const c = new JoinClient({
    url: "ws://h/api/join/ws",
    invite: "INVITE",
    name: "Ann",
    readPad: () => pad,
    onStage: (s) => stages.push(s),
    storage,
    WebSocketImpl: FakeSocket as unknown as typeof WebSocket,
    now: () => now,
    raf: () => 0,
    cancelRaf: () => {},
    ...extra,
  });
  return c;
}

/** Move both clocks on. */
function advance(ms: number) {
  for (let left = ms; left > 0; left -= 10) {
    const step = Math.min(10, left);
    now += step;
    vi.advanceTimersByTime(step);
  }
}

const last = () => FakeSocket.all[FakeSocket.all.length - 1];
const kind = () => stages[stages.length - 1]?.kind;

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.all = [];
  stages = [];
  pad = { ...IDLE_PAD };
  storage = new MemStorage();
  now = 1_000_000;
});
afterEach(() => vi.useRealTimers());

function joinNow(c: JoinClient, player = 2) {
  c.start();
  last().open();
  last().say({ stage: "asking" });
  last().say({ stage: "joined", player, token: "TOK", hevc: false });
}

describe("join client", () => {
  it("says hello with the invite and name, asks, then joins and keeps the token", () => {
    const c = client();
    c.start();
    expect(kind()).toBe("connecting");
    last().open();
    expect(last().texts[0]).toEqual({ hello: { name: "Ann", invite: "INVITE" } });
    last().say({ stage: "asking" });
    expect(kind()).toBe("asking");
    last().say({ stage: "joined", player: 3, token: "TOK", hevc: true });
    expect(stages[stages.length - 1]).toEqual({ kind: "joined", player: 3, hevc: true });
    expect(storage.getItem(tokenKey("INVITE"))).toBe("TOK");
    // The first frame goes out at once.
    expect(last().frames.length).toBe(1);
  });

  it("asks for a keyframe only once joined (#648)", () => {
    const c = client();
    c.start();
    last().open();
    c.askKeyframe();
    expect(last().texts.some((t) => "keyframe" in t)).toBe(false);
    last().say({ stage: "joined", player: 2, token: "TOK", hevc: false });
    c.askKeyframe();
    expect(last().texts.filter((t) => t.keyframe === true).length).toBe(1);
  });

  it("sends nothing before it is joined", () => {
    const c = client();
    c.start();
    last().open();
    pad = { ...IDLE_PAD, buttons: 1 };
    c.sendPad(true);
    advance(500);
    expect(last().frames.length).toBe(0);
  });

  it("sends on change and keeps alive every 100 ms with a rising seq", () => {
    const c = client();
    joinNow(c);
    const ws = last();
    const before = ws.frames.length;
    c.sendPad(false); // same pad, too soon: nothing
    expect(ws.frames.length).toBe(before);
    pad = { ...IDLE_PAD, buttons: 1 };
    c.sendPad(false);
    expect(ws.frames.length).toBe(before + 1);
    expect(ws.frames[ws.frames.length - 1][5]).toBe(1);
    advance(100);
    expect(ws.frames.length).toBe(before + 2);
    advance(300);
    expect(ws.frames.length).toBeGreaterThanOrEqual(before + 5);
    const seqs = ws.frames.map((f) => new DataView(f.buffer).getUint32(1, true));
    for (let i = 1; i < seqs.length; i++) expect(seqs[i]).toBe(seqs[i - 1] + 1);
  });

  it("lets go of everything while hidden", () => {
    const c = client();
    joinNow(c);
    pad = { ...IDLE_PAD, buttons: 1, lx: 30000 };
    c.sendPad(true);
    c.setNeutral(true);
    const f = last().frames[last().frames.length - 1];
    expect(Array.from(f.subarray(5, 15))).toEqual([0, 0, 0, 0, 128, 128, 128, 128, 0, 0]);
  });

  it("pings every second and reports the round trip", () => {
    const rtts: number[] = [];
    const c = client({ onRtt: (ms) => rtts.push(ms) });
    joinNow(c);
    advance(1000);
    const ping = last().texts.find((t) => "ping" in t)!;
    now += 35;
    last().say({ pong: ping.ping });
    expect(rtts).toEqual([35]);
  });

  it("reconnects with backoff and resumes with the token, without asking", () => {
    const c = client();
    joinNow(c, 2);
    last().drop();
    expect(kind()).toBe("reconnecting");
    const n0 = FakeSocket.all.length;
    advance(249);
    expect(FakeSocket.all.length).toBe(n0);
    advance(1);
    expect(FakeSocket.all.length).toBe(n0 + 1);
    // That attempt fails: the next waits 500 ms.
    last().drop();
    advance(499);
    expect(FakeSocket.all.length).toBe(n0 + 1);
    advance(1);
    expect(FakeSocket.all.length).toBe(n0 + 2);
    last().open();
    expect(last().texts[0]).toEqual({ hello: { name: "Ann", invite: "INVITE", resume: "TOK" } });
    // Nothing goes out while disconnected, and nothing until joined again.
    expect(last().frames.length).toBe(0);
    last().say({ stage: "joined", player: 2, token: "TOK", hevc: false });
    expect(stages[stages.length - 1]).toEqual({ kind: "joined", player: 2, hevc: false });
  });

  it("backs off to at most 4 s and gives up after the 60 s window", () => {
    const c = client();
    joinNow(c);
    last().drop();
    const waits: number[] = [];
    let at = now;
    let count = FakeSocket.all.length;
    for (let i = 0; i < 2000 && kind() === "reconnecting"; i++) {
      advance(50);
      if (FakeSocket.all.length > count) {
        waits.push(now - at);
        at = now;
        count = FakeSocket.all.length;
        last().drop();
      }
    }
    expect(waits.slice(0, 6)).toEqual([250, 500, 1000, 2000, 4000, 4000]);
    // About 60 s of trying, then no more.
    expect(waits.reduce((a, b) => a + b, 0)).toBeLessThanOrEqual(60_000);
    expect(waits.reduce((a, b) => a + b, 0)).toBeGreaterThan(55_000);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "gave_up" });
    expect(storage.getItem(tokenKey("INVITE"))).toBeNull();
  });

  it("gives up when a reconnect socket opens but the engine never answers", () => {
    const c = client();
    joinNow(c);
    last().drop();
    advance(250);
    last().open(); // up, but no answer ever comes
    for (let i = 0; i < 80 && kind() === "reconnecting"; i++) advance(1000);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "gave_up" });
  });

  it("treats a silent link as dropped", () => {
    const c = client();
    joinNow(c);
    advance(4000);
    expect(kind()).toBe("reconnecting");
  });

  it("says hello with a typed code instead of an invite", () => {
    const c = client({ invite: null, code: "K7M2QX" });
    c.start();
    last().open();
    expect(last().texts[0]).toEqual({ hello: { name: "Ann", code: "K7M2QX" } });
    last().say({ stage: "joined", player: 4, token: "C", hevc: false });
    expect(storage.getItem(tokenKey("K7M2QX"))).toBe("C");
  });

  it("starts from a saved place on a reload", () => {
    storage.setItem(tokenKey("INVITE"), "SAVED");
    const c = client();
    c.start();
    last().open();
    expect(last().texts[0]).toEqual({ hello: { name: "Ann", invite: "INVITE", resume: "SAVED" } });
  });

  it("shows refusals and forgets the token", () => {
    for (const why of ["key", "full", "denied", "closed", "old_helper", "not_playing"] as const) {
      stages = [];
      storage.setItem(tokenKey("INVITE"), "OLD");
      const c = client();
      c.start();
      last().open();
      last().say({ stage: "refused", why });
      expect(stages[stages.length - 1]).toEqual({ kind: "refused", why });
      expect(storage.getItem(tokenKey("INVITE"))).toBeNull();
      // No reconnect after a refusal.
      const n = FakeSocket.all.length;
      advance(5000);
      expect(FakeSocket.all.length).toBe(n);
    }
  });

  it("ends on bye (kicked, closed) without reconnecting", () => {
    for (const why of ["kicked", "closed"] as const) {
      stages = [];
      const c = client();
      joinNow(c);
      last().say({ bye: why });
      expect(stages[stages.length - 1]).toEqual({ kind: "ended", why });
      const n = FakeSocket.all.length;
      advance(5000);
      expect(FakeSocket.all.length).toBe(n);
    }
  });

  it("waits for a host who stopped playing, keeps the place, and plays on when they start again", () => {
    const c = client();
    joinNow(c);
    const at = now;
    last().say({ bye: "host_stopped" });
    expect(stages[stages.length - 1]).toEqual({
      kind: "reconnecting",
      player: 2,
      attempt: 0,
      deadline: at + 60_000,
      hostStopped: true,
    });
    expect(storage.getItem(tokenKey("INVITE"))).toBe("TOK");
    expect(c.resumeToken).toBe("TOK");
    // back with the token after the first backoff; the engine holds it until the host plays
    const n = FakeSocket.all.length;
    advance(250);
    expect(FakeSocket.all.length).toBe(n + 1);
    last().open();
    expect(last().texts[0]).toEqual({ hello: { name: "Ann", invite: "INVITE", resume: "TOK" } });
    last().say({ stage: "waiting", why: "host_stopped" });
    expect(stages[stages.length - 1]).toMatchObject({ kind: "reconnecting", hostStopped: true, deadline: at + 60_000 });
    // a quiet socket while waiting is not dropped, and no other one is opened
    advance(20_000);
    expect(FakeSocket.all.length).toBe(n + 1);
    expect(kind()).toBe("reconnecting");
    // the host plays again
    last().say({ stage: "joined", player: 2, token: "TOK", hevc: false });
    expect(stages[stages.length - 1]).toEqual({ kind: "joined", player: 2, hevc: false });
    // a later drop is a plain reconnect again (not the host)
    last().drop();
    expect(stages[stages.length - 1]).toMatchObject({ kind: "reconnecting", attempt: 0 });
    expect((stages[stages.length - 1] as { hostStopped?: boolean }).hostStopped).toBeUndefined();
  });

  it("keeps trying with backoff while the host is away, then asks to join again", () => {
    const c = client();
    joinNow(c);
    last().say({ bye: "host_stopped" });
    const waits: number[] = [];
    let at = now;
    let count = FakeSocket.all.length;
    for (let i = 0; i < 2000 && kind() === "reconnecting"; i++) {
      advance(50);
      if (FakeSocket.all.length > count) {
        waits.push(now - at);
        at = now;
        count = FakeSocket.all.length;
        last().drop(); // the engine is not reachable
      }
    }
    expect(waits.slice(0, 5)).toEqual([250, 500, 1000, 2000, 4000]);
    expect(waits.reduce((a, b) => a + b, 0)).toBeGreaterThan(55_000);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "host_stopped" });
    expect(storage.getItem(tokenKey("INVITE"))).toBeNull();
  });

  it("gives up when Play Together closes while waiting for the host, or the window runs out", () => {
    // the engine closes it (the host did not come back): its refusal ends the wait
    let c = client();
    joinNow(c);
    last().say({ bye: "host_stopped" });
    advance(250);
    last().open();
    last().say({ stage: "waiting", why: "host_stopped" });
    last().say({ stage: "refused", why: "closed" });
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "host_stopped" });
    expect(storage.getItem(tokenKey("INVITE"))).toBeNull();
    // no answer at all: the page stops waiting a little after the window
    stages = [];
    c = client();
    joinNow(c);
    last().say({ bye: "host_stopped" });
    advance(250);
    last().open();
    last().say({ stage: "waiting", why: "host_stopped" });
    advance(60_000);
    expect(kind()).toBe("reconnecting");
    advance(6_000);
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "host_stopped" });
  });

  it("learns on a reload that the host is away", () => {
    storage.setItem(tokenKey("INVITE"), "SAVED");
    const c = client();
    c.start();
    last().open();
    last().say({ stage: "waiting", why: "host_stopped" });
    expect(stages[stages.length - 1]).toMatchObject({ kind: "reconnecting", hostStopped: true, deadline: now + 60_000 });
  });

  it("a socket that never comes up is lost, not retried forever", () => {
    const c = client();
    c.start();
    last().drop();
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "lost" });
  });

  it("leave lets go, says leave, and forgets the place", () => {
    const c = client();
    joinNow(c);
    const ws = last();
    pad = { ...IDLE_PAD, buttons: 1 };
    c.sendPad(true);
    c.leave();
    const f = ws.frames[ws.frames.length - 1];
    expect(new DataView(f.buffer).getUint32(5, true)).toBe(0);
    expect(ws.texts[ws.texts.length - 1]).toEqual({ leave: 1 });
    expect(stages[stages.length - 1]).toEqual({ kind: "ended", why: "left" });
    expect(storage.getItem(tokenKey("INVITE"))).toBeNull();
  });

  it("passes rumble, light, status and picture on", () => {
    const got: unknown[] = [];
    const c = client({
      onRumble: (a, b) => got.push(["rumble", a, b]),
      onLight: (l) => got.push(["light", l]),
      onStatus: (s) => got.push(["status", s.state]),
      onFrame: (d) => got.push(["frame", d.byteLength]),
    });
    joinNow(c);
    last().say({ rumble: [10, 20] });
    last().say({ light: [1, 2, 3] });
    last().say({ status: { player: 2, state: "playing", rtt_ms: 9, host: "Ann", block_system: false } });
    last().onmessage?.({ data: new ArrayBuffer(4) });
    expect(got).toEqual([["rumble", 10, 20], ["light", [1, 2, 3]], ["status", "playing"], ["frame", 4]]);
  });
});
