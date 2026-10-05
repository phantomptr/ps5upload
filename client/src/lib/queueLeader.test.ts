import { describe, expect, it, vi } from "vitest";
import {
  CLAIM_SETTLE_MS,
  LEADER_KEY,
  LEASE_TTL_MS,
  createQueueLeader,
  makeTabId,
} from "./queueLeader";

function fakeStorage() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
    raw: m,
  };
}

/** Two tabs of one browser: the same storage, their own ids, one shared clock. */
function twoTabs() {
  const storage = fakeStorage();
  let t = 1_000_000;
  const clock = { now: () => t, advance: (ms: number) => void (t += ms) };
  const sleep = async (ms: number) => clock.advance(ms);
  const mk = (id: string) => createQueueLeader({ storage, now: clock.now, id, sleep });
  return { storage, clock, a: mk("tab-a"), b: mk("tab-b") };
}

describe("queue leader election (one runner per browser profile)", () => {
  it("the first tab runs the queue and a second tab does not", async () => {
    const { a, b } = twoTabs();
    expect(await a.claim()).toBe(true);
    expect(await b.claim()).toBe(false);
    expect(a.isLeader()).toBe(true);
    expect(b.isLeader()).toBe(false);
  });

  it("the leader's heartbeat keeps the lease: a second tab stays out well past the TTL", async () => {
    const { a, b, clock } = twoTabs();
    await a.claim();
    for (let i = 0; i < 10; i++) {
      clock.advance(LEASE_TTL_MS / 2);
      expect(await a.claim()).toBe(true); // heartbeat
      expect(await b.claim()).toBe(false);
    }
  });

  it("takes over once the leader stops heartbeating (closed tab, frozen tab)", async () => {
    const { a, b, clock } = twoTabs();
    await a.claim();
    clock.advance(LEASE_TTL_MS - 500);
    expect(await b.claim()).toBe(false);
    clock.advance(600);
    expect(await b.claim()).toBe(true);
    // the old leader finds out at its next heartbeat and steps down
    expect(await a.claim()).toBe(false);
    expect(a.isLeader()).toBe(false);
  });

  it("two tabs claiming in the same instant end with exactly one leader", async () => {
    const { a, b } = twoTabs();
    const [ra, rb] = await Promise.all([a.claim(), b.claim()]);
    expect([ra, rb].filter(Boolean)).toHaveLength(1);
    expect(a.isLeader()).not.toBe(b.isLeader());
  });

  it("a released lease is free at once", async () => {
    const { a, b } = twoTabs();
    await a.claim();
    a.release();
    expect(await b.claim()).toBe(true);
  });

  it("reports transitions through the start callback", async () => {
    const { a, b, clock } = twoTabs();
    const seen: boolean[] = [];
    b.start((v) => seen.push(v));
    await a.claim();
    await b.claim();
    clock.advance(LEASE_TTL_MS + 1);
    await b.claim();
    expect(seen).toEqual([true]);
    b.release();
  });

  it("without usable storage the tab is the leader (nothing to coordinate with)", async () => {
    const none = createQueueLeader({ storage: null, id: "x" });
    expect(await none.claim()).toBe(true);
    const broken = createQueueLeader({
      id: "y",
      storage: {
        getItem: () => {
          throw new Error("blocked");
        },
        setItem: () => {
          throw new Error("blocked");
        },
        removeItem: () => {},
      },
    });
    expect(await broken.claim()).toBe(true);
  });

  it("a corrupt lease counts as no lease", async () => {
    const { a, storage } = twoTabs();
    storage.setItem(LEADER_KEY, "{not json");
    expect(await a.claim()).toBe(true);
  });

  it("the settle wait is short and the tab id needs no crypto.randomUUID", () => {
    expect(CLAIM_SETTLE_MS).toBeLessThan(1000);
    const spy = vi.fn();
    vi.stubGlobal("crypto", { randomUUID: spy });
    const id = makeTabId();
    vi.unstubAllGlobals();
    expect(spy).not.toHaveBeenCalled();
    expect(id).toMatch(/^[a-z0-9]+-[a-z0-9]+$/);
    expect(makeTabId()).not.toBe(id);
  });
});
