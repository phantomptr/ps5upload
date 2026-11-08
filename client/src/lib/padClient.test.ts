import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { PAD_KEEPALIVE_MS, PAD_RECONNECT_FOR_MS, PAD_SILENCE_MS, PadClient, padDeviceId, readStatus, type PadLinkStage } from "./padClient";
import { IDLE_PAD, type PadState } from "./remotePlayStream";

describe("pad link status", () => {
  it("tells a phone the PS5 waits for the passcode on the computer, nothing more", () => {
    const s = readStatus(
      JSON.stringify({ stage: "joined", session: { host: "192.168.1.100", playing: false, passcode: true } }),
    );
    expect(s).toEqual({ kind: "joined", host: "192.168.1.100", playing: false, passcode: true });
  });

  it("an older engine without the field reads as not waiting", () => {
    const s = readStatus(JSON.stringify({ stage: "joined", session: { host: "h", playing: true } }));
    expect(s).toEqual({ kind: "joined", host: "h", playing: true, passcode: false });
  });
});

/** A socket the test drives: what the page sent, and the engine's side. */
class FakeSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static all: FakeSocket[] = [];
  readyState = 0;
  binaryType = "blob";
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.all.push(this);
  }
  send(d: string) {
    this.sent.push(d);
  }
  close() {
    if (this.readyState === 3) return;
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
  get pads(): PadState[] {
    return this.sent.map((s) => (JSON.parse(s) as { pad: PadState }).pad);
  }
}

describe("the phone's link", () => {
  let now = 1_000_000;
  let stages: PadLinkStage[] = [];
  let touch: PadState = { ...IDLE_PAD };
  const JOINED = { stage: "joined", session: { host: "h", playing: true } };

  function advance(ms: number) {
    for (let left = ms; left > 0; left -= 10) {
      const step = Math.min(10, left);
      now += step;
      vi.advanceTimersByTime(step);
    }
  }
  const last = () => FakeSocket.all[FakeSocket.all.length - 1];
  const client = (extra: Partial<ConstructorParameters<typeof PadClient>[0]> = {}) => {
    const c = new PadClient({
      url: "ws://h/api/pad/ws?t=key",
      onStage: (s) => stages.push(s),
      touchPad: () => touch,
      WebSocketImpl: FakeSocket as unknown as typeof WebSocket,
      now: () => now,
      raf: () => 0,
      cancelRaf: () => {},
      ...extra,
    });
    c.start();
    return c;
  };
  const inGame = (extra: Parameters<typeof client>[0] = {}) => {
    const c = client(extra);
    last().open();
    last().say(JOINED);
    return c;
  };

  beforeEach(() => {
    vi.useFakeTimers();
    FakeSocket.all = [];
    stages = [];
    touch = { ...IDLE_PAD };
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("sends a held pad at least every 250 ms and says it keeps sending", () => {
    const c = inGame();
    touch = { ...IDLE_PAD, buttons: 1 };
    c.sendPad();
    const s = last();
    const n = s.sent.length;
    expect(JSON.parse(s.sent[n - 1])).toMatchObject({ pad: { buttons: 1 }, ka: 1 });
    advance(PAD_KEEPALIVE_MS * 4);
    expect(s.sent.length - n).toBeGreaterThanOrEqual(3);
    expect(s.pads[s.pads.length - 1].buttons).toBe(1);
    c.stop();
  });

  it("lets go when hidden and takes the pad back when in front again", () => {
    const c = inGame();
    touch = { ...IDLE_PAD, buttons: 1, lx: 3000 };
    c.sendPad();
    c.setNeutral(true);
    const s = last();
    expect(s.pads[s.pads.length - 1]).toMatchObject({ buttons: 0, lx: 0 });
    advance(PAD_KEEPALIVE_MS * 2);
    expect(s.pads[s.pads.length - 1].buttons).toBe(0);
    c.setNeutral(false);
    expect(s.pads[s.pads.length - 1].buttons).toBe(1);
    c.stop();
  });

  it("comes back by itself after a dropped socket, and the state goes at once", () => {
    const c = inGame();
    touch = { ...IDLE_PAD, buttons: 2 };
    const first = last();
    first.close();
    expect(stages[stages.length - 1]).toEqual({ kind: "reconnecting", attempt: 0 });
    advance(250);
    const second = last();
    expect(second).not.toBe(first);
    second.open();
    second.say(JOINED);
    expect(stages[stages.length - 1]).toMatchObject({ kind: "joined" });
    expect(second.pads[second.pads.length - 1].buttons).toBe(2);
    c.stop();
  });

  it("backs off, doubling to 4 s, and gives up after its window; Wake starts over", () => {
    const c = inGame();
    last().close();
    const tries = () => FakeSocket.all.length;
    advance(250);
    expect(tries()).toBe(2);
    last().close();
    advance(499);
    expect(tries()).toBe(2);
    advance(1);
    expect(tries()).toBe(3);
    last().close();
    // A page in front again tries at once.
    c.wake();
    expect(tries()).toBe(4);
    for (let i = 0; i < 200 && stages[stages.length - 1].kind !== "lost"; i++) {
      last().close();
      advance(4000);
    }
    expect(stages[stages.length - 1]).toEqual({ kind: "lost" });
    const n = tries();
    advance(PAD_RECONNECT_FOR_MS);
    expect(tries()).toBe(n);
    // Back in front after all that (a phone with its screen off): it tries again.
    c.wake();
    expect(tries()).toBe(n + 1);
    last().open();
    last().say(JOINED);
    expect(stages[stages.length - 1]).toMatchObject({ kind: "joined" });
    c.stop();
  });

  it("drops a socket that went silent without a close", () => {
    const c = inGame();
    const first = last();
    advance(PAD_SILENCE_MS - 1000);
    first.say(JOINED);
    advance(PAD_SILENCE_MS - 1000);
    expect(first.readyState).toBe(1);
    advance(3000);
    expect(first.readyState).toBe(3);
    expect(last()).not.toBe(first);
    c.stop();
  });

  it("waits calmly while the computer decides, without calling that silence", () => {
    const c = client();
    last().open();
    last().say({ stage: "asking" });
    advance(PAD_SILENCE_MS * 4);
    expect(last().readyState).toBe(1);
    expect(stages[stages.length - 1]).toEqual({ kind: "asking" });
    c.stop();
  });

  it("does not come back after a refusal, and a first connect that fails is lost at once", () => {
    const c = inGame();
    last().say({ stage: "refused", why: "kicked" });
    expect(stages[stages.length - 1]).toEqual({ kind: "refused", why: "kicked" });
    const n = FakeSocket.all.length;
    advance(10_000);
    expect(FakeSocket.all.length).toBe(n);
    c.stop();

    stages = [];
    const c2 = client();
    last().close();
    expect(stages[stages.length - 1]).toEqual({ kind: "lost" });
    c2.stop();
  });

  it("an internet phone keeps the engine's reconnect token and says it on every new socket", () => {
    const kept: string[] = [];
    const hello: Record<string, string | number> = { t: "k", name: "pixel", d: "00112233" };
    client({ hello, onReconnectToken: (t) => kept.push(t) });
    last().open();
    expect(JSON.parse(last().sent[0])).toEqual({ hello: { t: "k", name: "pixel", d: "00112233" } });
    last().say({ stage: "asking" });
    const rt = "0123456789abcdef0123456789abcdef";
    last().say({ rt });
    last().say(JOINED);
    expect(kept).toEqual([rt]);
    // Not taken for a stage.
    expect(stages[stages.length - 1].kind).toBe("joined");
    // A drop: the next socket's hello carries the token.
    last().close();
    advance(2000);
    last().open();
    expect(JSON.parse(last().sent[0])).toEqual({ hello: { t: "k", name: "pixel", d: "00112233", rt } });
    // Something that only looks like one is not kept.
    last().say({ rt: "not a token" });
    expect(kept).toEqual([rt]);
  });

  it("a watcher is back to watching on a new socket", () => {
    const control: boolean[] = [];
    const c = inGame({ watch: { onFrame: () => {}, onCodec: () => {}, onControl: (on) => control.push(on) } });
    last().say({ control: true });
    expect(control[control.length - 1]).toBe(true);
    last().close();
    advance(250);
    expect(control[control.length - 1]).toBe(false);
    c.stop();
  });
});

describe("the device's id", () => {
  it("is made once and kept", () => {
    const m = new Map<string, string>();
    const store = { get: (k: string) => m.get(k) ?? null, set: (k: string, v: string) => void m.set(k, v) };
    const id = padDeviceId(store, (a) => a.fill(0xab));
    expect(id).toBe("ab".repeat(16));
    expect(padDeviceId(store, (a) => a.fill(0xcd))).toBe(id);
    m.set("ps5upload.pad.device", "not an id");
    expect(padDeviceId(store, (a) => a.fill(0xcd))).toBe("cd".repeat(16));
  });
});
