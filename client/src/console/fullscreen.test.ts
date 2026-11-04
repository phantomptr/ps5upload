import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  _resetFullscreenForTests,
  fullscreenApi,
  fullscreenPref,
  fullscreenSupported,
  installFullscreen,
  isPs5Browser,
  setFullscreenPref,
  toggleFullscreen,
  watchFullscreen,
  type FullscreenDoc,
} from "./fullscreen";
import { onOtherDevice } from "./files/transfer";

const PS5_UA =
  "Mozilla/5.0 (PlayStation 5 13.60) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const DESKTOP_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0) AppleWebKit/605.1.15 Version/17.0 Safari/605.1.15";

type Opts = { enabled?: boolean; refuse?: boolean; webkitSilent?: boolean };

/** A document stand-in: the request APIs, the change/error events, and a call log. */
function doc(kind: "standard" | "webkit" | "both" | "none", o: Opts = {}) {
  const calls: string[] = [];
  const listeners: Record<string, (() => void)[]> = {};
  const fire = (t: string) => (listeners[t] ?? []).slice().forEach((fn) => fn());
  const d: FullscreenDoc = {
    documentElement: {} as FullscreenDoc["documentElement"],
    addEventListener: (t: string, fn: () => void) => void (listeners[t] ??= []).push(fn),
    removeEventListener: (t: string, fn: () => void) => {
      listeners[t] = (listeners[t] ?? []).filter((f) => f !== fn);
    },
  };
  const el = d.documentElement as unknown as Record<string, unknown>;
  if (kind === "standard" || kind === "both") {
    el.requestFullscreen = () => {
      calls.push("standard");
      if (o.refuse) {
        fire("fullscreenerror");
        return Promise.reject(new Error("refused"));
      }
      d.fullscreenElement = el as unknown as Element;
      fire("fullscreenchange");
      return Promise.resolve();
    };
    d.fullscreenEnabled = o.enabled ?? true;
    d.exitFullscreen = () => {
      calls.push("exit");
      d.fullscreenElement = null;
      fire("fullscreenchange");
      return Promise.resolve();
    };
  }
  if (kind === "webkit" || kind === "both") {
    el.webkitRequestFullscreen = () => {
      calls.push("webkit");
      if (o.webkitSilent) return; // neither change nor error: the browser ignored it
      if (o.refuse) {
        fire("webkitfullscreenerror");
        return;
      }
      d.webkitFullscreenElement = el as unknown as Element;
      fire("webkitfullscreenchange");
    };
    d.webkitFullscreenEnabled = o.enabled ?? true;
    d.webkitExitFullscreen = () => {
      calls.push("webkit-exit");
      d.webkitFullscreenElement = null;
      fire("webkitfullscreenchange");
    };
  }
  return { d, calls, listeners };
}

function storage(init: Record<string, string> = {}, broken = false) {
  const m = new Map(Object.entries(init));
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => {
      if (broken) throw new Error("denied");
      return m.has(k) ? m.get(k) : null;
    },
    setItem: (k: string, v: string) => {
      if (broken) throw new Error("denied");
      m.set(k, v);
    },
  });
  return m;
}

function win() {
  const listeners: Record<string, ((e: any) => void)[]> = {};
  return {
    listeners,
    addEventListener: (t: string, fn: (e: any) => void) => void (listeners[t] ??= []).push(fn),
    removeEventListener: (t: string, fn: (e: any) => void) => {
      listeners[t] = (listeners[t] ?? []).filter((f) => f !== fn);
    },
    fire: (t: string, e: any = {}) => (listeners[t] ?? []).slice().forEach((fn) => fn(e)),
  };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => _resetFullscreenForTests());
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("full screen", () => {
  it("prefers the standard API, falls back to the webkit-prefixed one", () => {
    expect(fullscreenApi(doc("both").d)).toBe("standard");
    expect(fullscreenApi(doc("webkit").d)).toBe("webkit");
    expect(fullscreenApi(doc("none").d)).toBeNull();
  });

  it("is unsupported when the API is missing or the browser disables it", () => {
    expect(fullscreenSupported(doc("webkit").d)).toBe(true);
    expect(fullscreenSupported(doc("none").d)).toBe(false);
    expect(fullscreenSupported(doc("standard", { enabled: false }).d)).toBe(false);
  });

  it("the setting defaults to on, and survives broken storage", () => {
    storage();
    expect(fullscreenPref()).toBe(true);
    storage({ "ps5upload.console.fullscreen": "0" });
    expect(fullscreenPref()).toBe(false);
    storage({}, true);
    expect(fullscreenPref()).toBe(true);
    expect(() => setFullscreenPref(false, doc("none").d)).not.toThrow();
  });

  it("only the PS5 browser goes full screen by itself", () => {
    expect(isPs5Browser(PS5_UA)).toBe(true);
    expect(isPs5Browser(DESKTOP_UA)).toBe(false);
  });

  it("any PlayStation user agent is the console, however the rest is spelled (one test for the whole page)", () => {
    expect(isPs5Browser("Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko)")).toBe(true);
    expect(isPs5Browser("Mozilla/5.0 (PlayStation 4 11.00) AppleWebKit/601.2 (KHTML, like Gecko)")).toBe(true);
    // Full screen, Back and Upload read the same export: the three can no longer disagree.
    expect(onOtherDevice("192.168.1.20", "Mozilla/5.0 (PlayStation 4 11.00) AppleWebKit/601.2")).toBe(false);
    expect(onOtherDevice("192.168.1.20", DESKTOP_UA)).toBe(true);
  });

  it("PS5: enters on the first activation with the prefixed API, only once", async () => {
    storage();
    const { d, calls } = doc("webkit");
    const w = win();
    installFullscreen(w, d, PS5_UA);
    w.fire("click");
    await tick();
    expect(calls).toEqual(["webkit"]);
    w.fire("click");
    w.fire("keydown", { key: "Enter" });
    await tick();
    expect(calls).toEqual(["webkit"]);
  });

  it("a LAN viewer (phone, desktop) is never put in full screen", async () => {
    storage();
    const { d, calls } = doc("standard");
    const w = win();
    installFullscreen(w, d, DESKTOP_UA);
    w.fire("click");
    await tick();
    expect(calls).toEqual([]);
  });

  it("Escape is not an activation: it spends no try", async () => {
    storage();
    const { d, calls } = doc("standard");
    const w = win();
    installFullscreen(w, d, PS5_UA);
    w.fire("keydown", { key: "Escape" });
    await tick();
    expect(calls).toEqual([]);
    w.fire("keydown", { key: "Enter" });
    await tick();
    expect(calls).toEqual(["standard"]);
  });

  it("does nothing on activation when the setting is off", async () => {
    storage({ "ps5upload.console.fullscreen": "0" });
    const { d, calls } = doc("standard");
    const w = win();
    installFullscreen(w, d, PS5_UA);
    w.fire("click");
    await tick();
    expect(calls).toEqual([]);
  });

  it("a refusal makes full screen unsupported for the session (the switch hides)", async () => {
    storage();
    const { d, calls } = doc("standard", { refuse: true });
    const w = win();
    const seen: number[] = [];
    watchFullscreen(() => seen.push(1), d);
    installFullscreen(w, d, PS5_UA);
    w.fire("click");
    await tick();
    expect(calls).toEqual(["standard"]);
    expect(fullscreenSupported(d)).toBe(false);
    expect(seen.length).toBeGreaterThan(0);
    w.fire("click");
    await tick();
    expect(calls).toEqual(["standard"]); // never asked again
  });

  it("webkit: a request the browser silently ignores is not counted as success", async () => {
    vi.useFakeTimers();
    storage();
    const { d, calls } = doc("webkit", { webkitSilent: true });
    const w = win();
    installFullscreen(w, d, PS5_UA);
    w.fire("click");
    await vi.advanceTimersByTimeAsync(2000);
    expect(calls).toEqual(["webkit"]);
    w.fire("click");
    await vi.advanceTimersByTimeAsync(2000);
    expect(calls).toEqual(["webkit", "webkit"]); // tried again: it did not take
  });

  it("webkit: an error event is a refusal", async () => {
    storage();
    const { d } = doc("webkit", { refuse: true });
    const w = win();
    installFullscreen(w, d, PS5_UA);
    w.fire("click");
    await tick();
    expect(fullscreenSupported(d)).toBe(false);
  });

  it("the switch and the touchpad toggle enter and leave; watchers hear each change", async () => {
    const m = storage();
    const { d, calls } = doc("standard");
    const states: boolean[] = [];
    const stop = watchFullscreen((s) => states.push(s.on), d);
    setFullscreenPref(false, d);
    expect(m.get("ps5upload.console.fullscreen")).toBe("0");
    setFullscreenPref(true, d);
    await tick();
    expect(calls).toEqual(["standard"]);
    toggleFullscreen(d);
    await tick();
    expect(calls).toEqual(["standard", "exit"]);
    expect(states).toEqual([true, false]);
    stop();
  });
});
