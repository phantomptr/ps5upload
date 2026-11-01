import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  LOCK_WAIT_MS,
  _grabSettled,
  _resetCaptureForTest,
  captureDelta,
  captureState,
  isCaptured,
  releaseCapture,
  requestCapture,
  subscribeCapture,
  isEngaged,
} from "./pointerCapture";

// vitest runs in node: a document with Pointer Lock, enough for the module.
type Listener = () => void;
interface FakeDoc {
  pointerLockElement: unknown;
  listeners: Map<string, Listener[]>;
  addEventListener: (t: string, fn: Listener) => void;
  exitPointerLock: () => void;
}

function fire(doc: FakeDoc, t: string) {
  for (const fn of doc.listeners.get(t) ?? []) fn();
}

let doc: FakeDoc;
let win: { listeners: Map<string, Listener[]>; addEventListener: (t: string, fn: Listener) => void };

/** A stage whose requestPointerLock does what `grants` says. */
function stage(grants: "lock" | "never" | "throw" | "late" = "lock") {
  const attrs = new Set<string>();
  const el = {
    style: { cursor: "" },
    setAttribute: (a: string) => attrs.add(a),
    removeAttribute: (a: string) => attrs.delete(a),
    attrs,
    requestPointerLock: vi.fn(() => {
      if (grants === "throw") throw new Error("no");
      if (grants === "lock")
        queueMicrotask(() => {
          doc.pointerLockElement = el;
          fire(doc, "pointerlockchange");
        });
      return Promise.resolve();
    }),
  };
  return el as unknown as HTMLElement & { attrs: Set<string>; requestPointerLock: ReturnType<typeof vi.fn> };
}

function grantLock(el: unknown) {
  doc.pointerLockElement = el;
  fire(doc, "pointerlockchange");
}

beforeEach(() => {
  vi.useFakeTimers();
  doc = {
    pointerLockElement: null,
    listeners: new Map(),
    addEventListener(t, fn) {
      this.listeners.set(t, [...(this.listeners.get(t) ?? []), fn]);
    },
    exitPointerLock: vi.fn(() => {
      doc.pointerLockElement = null;
      fire(doc, "pointerlockchange");
    }),
  };
  win = {
    listeners: new Map(),
    addEventListener(t, fn) {
      this.listeners.set(t, [...(this.listeners.get(t) ?? []), fn]);
    },
  };
  vi.stubGlobal("document", doc);
  vi.stubGlobal("window", win);
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function setup(desktop: boolean) {
  const calls: boolean[] = [];
  _resetCaptureForTest({
    desktop,
    grab: async (on) => {
      calls.push(on);
    },
  });
  return calls;
}

describe("pointer capture", () => {
  it("uses the browser's lock when it is granted, and never the grab", async () => {
    const calls = setup(true);
    const el = stage("lock");
    requestCapture(el);
    await Promise.resolve();
    expect(captureState().kind).toBe("lock");
    expect(isCaptured(el)).toBe(true);
    vi.advanceTimersByTime(LOCK_WAIT_MS * 2);
    await _grabSettled();
    expect(calls).toEqual([]);
    // The browser ending its lock (Esc) is the capture ending.
    doc.exitPointerLock();
    expect(isCaptured()).toBe(false);
  });

  it("grabs in the desktop app when no lock arrives (WKWebView), and gives the cursor back on release", async () => {
    const calls = setup(true);
    const el = stage("never");
    requestCapture(el);
    expect(isCaptured(el)).toBe(false);
    vi.advanceTimersByTime(LOCK_WAIT_MS - 1);
    expect(isCaptured(el)).toBe(false);
    vi.advanceTimersByTime(1);
    expect(captureState()).toMatchObject({ kind: "grab", failed: false });
    expect(isCaptured(el)).toBe(true);
    expect(el.style.cursor).toBe("none");
    releaseCapture();
    await _grabSettled();
    expect(calls).toEqual([true, false]);
    expect(isCaptured()).toBe(false);
    expect(el.style.cursor).toBe("");
    expect(el.attrs.size).toBe(0);
  });

  it("grabs straight away when the lock call throws", async () => {
    const calls = setup(true);
    requestCapture(stage("throw"));
    expect(captureState().kind).toBe("grab");
    await _grabSettled();
    expect(calls).toEqual([true]);
    releaseCapture();
    await _grabSettled();
    expect(calls).toEqual([true, false]);
  });

  it("in a browser, no lock is a failed capture (the hover fallback), never a grab", async () => {
    const calls = setup(false);
    const el = stage("never");
    requestCapture(el);
    vi.advanceTimersByTime(LOCK_WAIT_MS);
    await _grabSettled();
    expect(captureState()).toMatchObject({ kind: "none", failed: true });
    expect(calls).toEqual([]);
    // A later lock clears it.
    requestCapture(el);
    grantLock(el);
    expect(captureState()).toMatchObject({ kind: "lock", failed: false });
  });

  it("a lock that arrives after the grab replaces it (the grab is let go)", async () => {
    const calls = setup(true);
    const el = stage("never");
    requestCapture(el);
    vi.advanceTimersByTime(LOCK_WAIT_MS);
    expect(captureState().kind).toBe("grab");
    grantLock(el);
    await _grabSettled();
    expect(captureState().kind).toBe("lock");
    expect(calls).toEqual([true, false]);
    expect(el.style.cursor).toBe("");
  });

  it("a release before the wait ends never grabs", async () => {
    const calls = setup(true);
    requestCapture(stage("never"));
    releaseCapture();
    vi.advanceTimersByTime(LOCK_WAIT_MS * 2);
    await _grabSettled();
    expect(calls).toEqual([]);
    expect(isCaptured()).toBe(false);
  });

  it("a grab that fails puts the cursor back and counts as not captured", async () => {
    const calls: boolean[] = [];
    _resetCaptureForTest({
      desktop: true,
      grab: async (on) => {
        calls.push(on);
        if (on) throw new Error("denied");
      },
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const el = stage("never");
    requestCapture(el);
    vi.advanceTimersByTime(LOCK_WAIT_MS);
    await _grabSettled();
    await _grabSettled();
    expect(calls).toEqual([true, false]);
    expect(captureState()).toMatchObject({ kind: "none", failed: true });
    expect(el.style.cursor).toBe("");
    warn.mockRestore();
  });

  it("a reload (pagehide) gives the cursor back", async () => {
    const calls = setup(true);
    requestCapture(stage("never"));
    vi.advanceTimersByTime(LOCK_WAIT_MS);
    for (const fn of win.listeners.get("pagehide") ?? []) fn();
    await _grabSettled();
    expect(calls).toEqual([true, false]);
  });

  it("tells subscribers about every change, and release is safe when nothing is captured", () => {
    setup(true);
    const seen: string[] = [];
    const off = subscribeCapture(() => seen.push(captureState().kind));
    releaseCapture();
    const el = stage("never");
    requestCapture(el);
    vi.advanceTimersByTime(LOCK_WAIT_MS);
    releaseCapture();
    off();
    // The click engages first (the keys are the PS5's at once), then the grab comes.
    expect(seen).toEqual(["none", "grab", "none"]);
  });

  it("engaged: from the click until the release, also when nothing can capture", () => {
    setup(false);
    const el = stage("never");
    expect(isEngaged()).toBe(false);
    requestCapture(el);
    expect(isEngaged(el)).toBe(true);
    vi.advanceTimersByTime(LOCK_WAIT_MS);
    // A browser that refuses the lock: not captured, still engaged (the keys go to the PS5).
    expect(captureState()).toMatchObject({ kind: "none", failed: true, engaged: el });
    releaseCapture();
    expect(isEngaged()).toBe(false);
  });
});

describe("captureDelta", () => {
  it("prefers movementX/Y, else the change of screen position (a grab that leaves the cursor free)", () => {
    setup(true);
    expect(captureDelta({ movementX: 3, movementY: -2, screenX: 100, screenY: 100 })).toEqual([3, -2]);
    expect(captureDelta({ movementX: 0, movementY: 0, screenX: 104, screenY: 99 })).toEqual([4, -1]);
    // Under the macOS grab the cursor stands still: the deltas are movementX/Y.
    expect(captureDelta({ movementX: 5, movementY: 1, screenX: 104, screenY: 99 })).toEqual([5, 1]);
    expect(captureDelta({ movementX: 0, movementY: 0, screenX: 104, screenY: 99 })).toEqual([0, 0]);
  });
});
