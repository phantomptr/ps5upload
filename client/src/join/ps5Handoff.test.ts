import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { parsePs5Fragment, type Ps5Target } from "../lib/joinProtocol";
import {
  freshInvite,
  freshTarget,
  hubAllowed,
  INVITE_FRESH_MS,
  INVITE_KEY,
  saveInvite,
  saveTarget,
  sendToPs5,
  TARGET_FRESH_MS,
  TARGET_KEY,
} from "./ps5Handoff";

const ROOM = "A".repeat(21) + "h";
const NONCE = "n0nce-AB_9";
const KEY = "0f".repeat(16);
const FP = Array.from({ length: 32 }, () => "AB").join(":");
const NET_LINK = `https://ps5upload.phantomptr.com/join/#room=${"r".repeat(22)}&k=${KEY}&fp=${FP}&hub=hub.example.net&n=Alex`;
const LAN_LINK = `http://192.168.1.10:18800/join#i=${KEY}&n=Sam`;
const TARGET: Ps5Target = { room: ROOM, t: NONCE, hub: "hub.phantomptr.com" };

describe("the QR code on PS5Upload Play's screen", () => {
  it("reads the room, the nonce and the hub", () => {
    expect(parsePs5Fragment(`#ps5=${ROOM}&t=${NONCE}&hub=hub.phantomptr.com`)).toEqual(TARGET);
    expect(parsePs5Fragment(`ps5=${ROOM}&t=${NONCE}&hub=[::1]:8443`)?.hub).toBe("[::1]:8443");
    expect(parsePs5Fragment(`#ps5=${ROOM}&t=${"x".repeat(32)}&hub=localhost:8080`)?.t).toHaveLength(32);
  });

  it("refuses anything else", () => {
    for (const bad of [
      "",
      `#ps5=${ROOM}&t=${NONCE}`,
      `#ps5=${ROOM.slice(1)}&t=${NONCE}&hub=hub.phantomptr.com`,
      `#ps5=${ROOM}x&t=${NONCE}&hub=hub.phantomptr.com`,
      `#ps5=${ROOM}&t=short&hub=hub.phantomptr.com`,
      `#ps5=${ROOM}&t=${"x".repeat(33)}&hub=hub.phantomptr.com`,
      `#ps5=${ROOM}&t=${NONCE}&hub=evil.com/path`,
      `#ps5=${ROOM}&t=${NONCE}&hub=user@evil.com`,
      `#ps5=${ROOM}&t=${NONCE}&hub=https://evil.com`,
      `#room=${ROOM}&t=${NONCE}&hub=hub.phantomptr.com`,
    ]) {
      expect(parsePs5Fragment(bad), bad).toBeNull();
    }
  });
});

describe("what the phone remembers", () => {
  let store: Map<string, string>;
  beforeEach(() => {
    store = new Map();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
        removeItem: (k: string) => void store.delete(k),
      },
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("a scanned PS5 for 10 minutes", () => {
    saveTarget(TARGET, 1_000);
    expect(JSON.parse(store.get(TARGET_KEY)!)).toEqual({ ...TARGET, at: 1_000 });
    expect(freshTarget(1_000 + TARGET_FRESH_MS - 1)).toEqual(TARGET);
    expect(freshTarget(1_000 + TARGET_FRESH_MS)).toBeNull();
    // A clock that went back, or a stored value that isn't one.
    expect(freshTarget(500)).toBeNull();
    store.set(TARGET_KEY, JSON.stringify({ room: "x", t: NONCE, hub: "hub.phantomptr.com", at: 1_000 }));
    expect(freshTarget(2_000)).toBeNull();
    store.set(TARGET_KEY, "{not json");
    expect(freshTarget(2_000)).toBeNull();
  });

  it("an invite opened on this phone for 30 minutes, with the host's name", () => {
    expect(saveInvite(NET_LINK, 1_000)).toEqual({ link: NET_LINK, name: "Alex" });
    expect(freshInvite(1_000 + INVITE_FRESH_MS - 1)).toEqual({ link: NET_LINK, name: "Alex" });
    expect(freshInvite(1_000 + INVITE_FRESH_MS)).toBeNull();
    expect(saveInvite(LAN_LINK, 5)?.name).toBe("Sam");
    expect(saveInvite("https://example.com/", 6)).toBeNull();
    expect(freshInvite(10)?.link).toBe(LAN_LINK);
    store.set(INVITE_KEY, JSON.stringify({ link: "javascript:alert(1)", at: 10 }));
    expect(freshInvite(10)).toBeNull();
  });

  it("works without storage", () => {
    vi.stubGlobal("window", {
      localStorage: {
        getItem: () => {
          throw new Error("denied");
        },
        setItem: () => {
          throw new Error("denied");
        },
      },
    });
    vi.spyOn(console, "warn").mockImplementation(() => {});
    saveTarget(TARGET);
    expect(freshTarget()).toBeNull();
    expect(saveInvite(LAN_LINK)).toEqual({ link: LAN_LINK, name: "Sam" });
    expect(freshInvite()).toBeNull();
  });
});

describe("which hub a QR code may name", () => {
  it("the default hub, or the invite's own", () => {
    expect(hubAllowed(TARGET, LAN_LINK)).toBe(true);
    expect(hubAllowed({ ...TARGET, hub: "HUB.phantomptr.com" }, LAN_LINK)).toBe(true);
    expect(hubAllowed({ ...TARGET, hub: "hub.example.net" }, NET_LINK)).toBe(true);
    expect(hubAllowed({ ...TARGET, hub: "hub.example.net" }, LAN_LINK)).toBe(false);
    expect(hubAllowed({ ...TARGET, hub: "evil.example" }, NET_LINK)).toBe(false);
    expect(hubAllowed({ ...TARGET, hub: "hub.phantomptr.com:8443" }, LAN_LINK)).toBe(false);
  });
});

/** A WebSocket stand-in: records what is sent, the test plays the hub. */
class FakeWs {
  static last: FakeWs | null = null;
  static made: string[] = [];
  readyState = 0;
  sent: unknown[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    FakeWs.last = this;
    FakeWs.made.push(url);
  }
  send(text: string) {
    this.sent.push(JSON.parse(text));
  }
  close() {
    this.closed = true;
    this.readyState = 3;
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  hub(m: unknown) {
    this.onmessage?.({ data: typeof m === "string" ? m : JSON.stringify(m) });
  }
}

const WS = FakeWs as unknown as typeof WebSocket;

describe("sending the invite to the PS5", () => {
  beforeEach(() => {
    FakeWs.last = null;
    FakeWs.made = [];
  });
  afterEach(() => vi.useRealTimers());

  it("joins the room, signals the link with the TV's nonce, and leaves on the app's yes", async () => {
    const done = sendToPs5(TARGET, { link: NET_LINK, name: "Alex" }, { WebSocketImpl: WS });
    const ws = FakeWs.last!;
    expect(ws.url).toBe("wss://hub.phantomptr.com/ws");
    ws.open();
    expect(ws.sent).toEqual([{ v: 1, t: "join", room: ROOM }]);
    ws.hub({ t: "joined", gid: "g".repeat(12) });
    expect(ws.sent[1]).toEqual({ v: 1, t: "signal", to: "host", data: { invite: { link: NET_LINK, name: "Alex", t: NONCE } } });
    // Something else from the app first: not the answer.
    ws.hub({ t: "signal", from: "host", data: { hello: 1 } });
    ws.hub("not json");
    ws.hub({ t: "signal", from: "host", data: { invite_ok: true } });
    await expect(done).resolves.toBe("ok");
    expect(ws.sent[2]).toEqual({ t: "leave" });
    expect(ws.sent).toHaveLength(3);
    expect(ws.closed).toBe(true);
  });

  it("leaves the name out when the link has none", async () => {
    const done = sendToPs5(TARGET, { link: LAN_LINK, name: null }, { WebSocketImpl: WS });
    const ws = FakeWs.last!;
    ws.open();
    ws.hub({ t: "joined", gid: "g" });
    expect(ws.sent[1]).toEqual({ v: 1, t: "signal", to: "host", data: { invite: { link: LAN_LINK, t: NONCE } } });
    ws.hub({ t: "signal", from: "host", data: { invite_ok: false, why: "busy" } });
    await expect(done).resolves.toBe("refused");
  });

  it("an expired code, and a hub that goes away", async () => {
    let done = sendToPs5(TARGET, { link: LAN_LINK, name: null }, { WebSocketImpl: WS });
    FakeWs.last!.open();
    FakeWs.last!.hub({ t: "error", why: "no_room" });
    await expect(done).resolves.toBe("expired");

    done = sendToPs5(TARGET, { link: LAN_LINK, name: null }, { WebSocketImpl: WS });
    FakeWs.last!.open();
    FakeWs.last!.hub({ t: "error", why: "rate_limited" });
    await expect(done).resolves.toBe("unreachable");

    done = sendToPs5(TARGET, { link: LAN_LINK, name: null }, { WebSocketImpl: WS });
    FakeWs.last!.onclose?.();
    await expect(done).resolves.toBe("unreachable");
  });

  it("gives up after the wait when the app says nothing", async () => {
    vi.useFakeTimers();
    const done = sendToPs5(TARGET, { link: LAN_LINK, name: null }, { WebSocketImpl: WS, answerMs: 10_000 });
    const ws = FakeWs.last!;
    ws.open();
    await vi.advanceTimersByTimeAsync(9_000);
    ws.hub({ t: "joined", gid: "g" });
    // The wait starts again for the app's answer.
    await vi.advanceTimersByTimeAsync(9_000);
    ws.hub({ t: "signal", from: "host", data: {} });
    await vi.advanceTimersByTimeAsync(1_001);
    await expect(done).resolves.toBe("unreachable");
    expect(ws.sent[ws.sent.length - 1]).toEqual({ t: "leave" });
  });

  it("never opens a socket to a hub the QR code may not name, or with no PS5", async () => {
    await expect(sendToPs5({ ...TARGET, hub: "evil.example" }, { link: LAN_LINK, name: null }, { WebSocketImpl: WS })).resolves.toBe("hint");
    await expect(sendToPs5(null, { link: LAN_LINK, name: null }, { WebSocketImpl: WS })).resolves.toBe("hint");
    expect(FakeWs.made).toEqual([]);
  });
});
