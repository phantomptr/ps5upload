import { describe, expect, it, vi } from "vitest";

import { lanPage, lanTarget, probeHub } from "./lanHint";
import { parseInternetFragment } from "./rtcTransport";

const KEY = "0f".repeat(16);
const FP = Array.from({ length: 32 }, () => "AB").join(":");
const BASE = `#room=${"r".repeat(22)}&k=${KEY}&fp=${FP}&hub=hub.phantomptr.com`;
const LAN = "&lan=http%3A%2F%2F192.168.1.10%3A19113%2Fjoin";

describe("the Wi-Fi hint in an internet link", () => {
  it("reads the host's page on their home network", () => {
    expect(lanPage(BASE + LAN, "join")).toBe("http://192.168.1.10:19113/join");
    // An engine under a path, on https by name.
    expect(lanPage(`${BASE}&lan=${encodeURIComponent("https://nas.local/ps5/join")}`, "join")).toBe("https://nas.local/ps5/join");
    expect(lanPage(`${BASE}&lan=${encodeURIComponent("http://192.168.1.10:19113/pad")}&w=1`, "pad")).toBe("http://192.168.1.10:19113/pad");
  });

  it("an older host's link names none; anything but its own page is no hint", () => {
    expect(lanPage(BASE, "join")).toBeNull();
    for (const bad of [
      "javascript:alert(1)",
      "data:text/html,x",
      "http://192.168.1.10:19113/elsewhere",
      "http://192.168.1.10:19113/pad",
      "http://user:pw@192.168.1.10/join",
      "http://192.168.1.10/join?x=1",
      "not a url",
    ]) {
      expect(lanPage(`${BASE}&lan=${encodeURIComponent(bad)}`, "join"), bad).toBeNull();
    }
  });

  it("the old fragment reads the same with the hint in it (an older page ignores it)", () => {
    expect(parseInternetFragment(BASE + LAN)).toEqual(parseInternetFragment(BASE));
    expect(parseInternetFragment(BASE + LAN)?.key).toBe(KEY);
  });

  it("goes to the join page with the invite's key, the phone link's page with no key", () => {
    expect(lanTarget("http://192.168.1.10:19113/join", { key: KEY })).toBe(`http://192.168.1.10:19113/join#i=${KEY}`);
    expect(lanTarget("http://192.168.1.10:19113/pad", { watch: true })).toBe("http://192.168.1.10:19113/pad#w=1");
    expect(lanTarget("http://192.168.1.10:19113/pad", {})).toBe("http://192.168.1.10:19113/pad");
  });
});

/** A WebSocket stand-in that opens, closes, or never answers. */
function fakeWs(behaviour: "open" | "close" | "silent" | "throw") {
  const made: Array<{ url: string; closed: boolean }> = [];
  class W {
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    private rec: { url: string; closed: boolean };
    constructor(url: string) {
      if (behaviour === "throw") throw new Error("blocked");
      this.rec = { url, closed: false };
      made.push(this.rec);
      setTimeout(() => {
        if (behaviour === "open") this.onopen?.();
        if (behaviour === "close") this.onclose?.();
      }, 10);
    }
    close() {
      this.rec.closed = true;
    }
  }
  return { WS: W as unknown as typeof WebSocket, made };
}

describe("is the invite service there?", () => {
  it("yes when its socket opens (closed again at once)", async () => {
    const { WS, made } = fakeWs("open");
    expect(await probeHub("wss://hub.phantomptr.com/ws", 1000, WS)).toBe(true);
    expect(made).toEqual([{ url: "wss://hub.phantomptr.com/ws", closed: true }]);
  });

  it("no when it closes, can't be made, or takes longer than the wait", async () => {
    expect(await probeHub("wss://h/ws", 1000, fakeWs("close").WS)).toBe(false);
    expect(await probeHub("wss://h/ws", 1000, fakeWs("throw").WS)).toBe(false);
    expect(await probeHub("wss://h/ws", 1000, undefined)).toBe(false);
    vi.useFakeTimers();
    try {
      const silent = fakeWs("silent");
      const p = probeHub("wss://h/ws", 4000, silent.WS);
      await vi.advanceTimersByTimeAsync(4000);
      expect(await p).toBe(false);
      expect(silent.made[0].closed).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});
