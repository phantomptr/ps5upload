import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Force Android on (the module gates on isAndroid) and run against minimal
// hand-rolled `document`/`navigator` stubs — the repo deliberately avoids
// pulling jsdom for one helper (same call as fsLastPath.test.ts).
vi.mock("./platform", () => ({ isAndroid: () => true }));

type Listener = () => void;

interface FakeSentinel {
  released: boolean;
  release: () => Promise<void>;
  addEventListener: (t: "release", cb: Listener) => void;
  _fireRelease: () => void;
}

function makeApi() {
  let live = 0; // sentinels currently held
  let acquired = 0; // total acquisitions
  let last: FakeSentinel | null = null;
  const wakeLock = {
    async request(_type: "screen") {
      acquired++;
      live++;
      let onRelease: Listener | null = null;
      const s: FakeSentinel = {
        released: false,
        async release() {
          if (!s.released) {
            s.released = true;
            live--;
          }
        },
        addEventListener(_t, cb) {
          onRelease = cb;
        },
        _fireRelease() {
          if (!s.released) {
            s.released = true;
            live--;
            onRelease?.();
          }
        },
      };
      last = s;
      return s;
    },
  };
  return {
    wakeLock,
    stats: () => ({ live, acquired }),
    last: () => last,
  };
}

interface DocStub {
  visibilityState: string;
  addEventListener: (type: string, cb: Listener) => void;
  _fire: (type: string) => void;
}

let api: ReturnType<typeof makeApi>;
let doc: DocStub;

function installStubs() {
  const listeners = new Map<string, Listener[]>();
  doc = {
    visibilityState: "visible",
    addEventListener(type, cb) {
      const arr = listeners.get(type) ?? [];
      arr.push(cb);
      listeners.set(type, arr);
    },
    _fire(type) {
      (listeners.get(type) ?? []).forEach((cb) => cb());
    },
  };
  Object.defineProperty(globalThis, "document", {
    value: doc,
    configurable: true,
  });
  Object.defineProperty(globalThis, "navigator", {
    value: { userAgent: "android", wakeLock: api.wakeLock },
    configurable: true,
  });
}

// Re-import the module per test so its singleton state starts clean.
async function freshModule() {
  vi.resetModules();
  return await import("./androidScreenWake");
}

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  api = makeApi();
  installStubs();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("setScreenWakeReason", () => {
  it("acquires on first reason and releases on last", async () => {
    const { setScreenWakeReason } = await freshModule();
    setScreenWakeReason("transfer", true);
    await flush();
    expect(api.stats().acquired).toBe(1);
    expect(api.stats().live).toBe(1);

    setScreenWakeReason("transfer", false);
    await flush();
    expect(api.stats().live).toBe(0);
  });

  it("keeps the lock while any reason is still held", async () => {
    const { setScreenWakeReason } = await freshModule();
    setScreenWakeReason("transfer", true);
    setScreenWakeReason("manual", true);
    await flush();
    // One OS lock for two reasons.
    expect(api.stats().acquired).toBe(1);

    setScreenWakeReason("transfer", false);
    await flush();
    expect(api.stats().live).toBe(1); // manual still holds it

    setScreenWakeReason("manual", false);
    await flush();
    expect(api.stats().live).toBe(0);
  });

  it("re-acquires after the OS auto-releases on page hide", async () => {
    const { setScreenWakeReason } = await freshModule();
    setScreenWakeReason("transfer", true);
    await flush();
    expect(api.stats().acquired).toBe(1);

    // Spec-mandated auto-release when the page is hidden.
    api.last()!._fireRelease();
    expect(api.stats().live).toBe(0);

    // Returning to the foreground re-acquires because a reason is held.
    doc._fire("visibilitychange");
    await flush();
    expect(api.stats().acquired).toBe(2);
    expect(api.stats().live).toBe(1);
  });
});
