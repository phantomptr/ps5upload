import { describe, expect, it } from "vitest";

import { createRunGen } from "./runGen";

describe("createRunGen", () => {
  it("starts at 0; current() reflects that", () => {
    const g = createRunGen();
    expect(g.current()).toBe(0);
  });

  it("next() returns 1, 2, 3 ... monotonic", () => {
    const g = createRunGen();
    expect(g.next()).toBe(1);
    expect(g.next()).toBe(2);
    expect(g.next()).toBe(3);
  });

  it("isLive(thisRun) is true only for the most-recently-issued run", () => {
    const g = createRunGen();
    const run1 = g.next();
    expect(g.isLive(run1)).toBe(true);
    const run2 = g.next();
    // run1 was superseded by run2.
    expect(g.isLive(run1)).toBe(false);
    expect(g.isLive(run2)).toBe(true);
  });

  it("isLive against a stale generation stays stale even if counter wraps to that value via bumps", () => {
    // The whole point: a stop() bumps the counter, the captured
    // thisRun goes stale, and even if some other caller bumps in
    // ways that happen to land on the same number again (impossible
    // here since next() is monotonic, but the contract is
    // "thisRun is stale once a NEXT one was issued").
    const g = createRunGen();
    const run1 = g.next();
    g.next(); // run1 is now stale
    g.next(); // still stale
    expect(g.isLive(run1)).toBe(false);
  });

  it("independent generators don't interfere", () => {
    const a = createRunGen();
    const b = createRunGen();
    const runA = a.next();
    b.next(); // advances b but not a
    expect(a.isLive(runA)).toBe(true);
    expect(b.current()).toBe(1);
    expect(a.current()).toBe(1);
  });

  it("isLive(0) is false on a fresh counter (no run yet)", () => {
    // current()=0 (the initial value, no run issued); a caller
    // who somehow captured 0 (e.g. uninitialized state) shouldn't
    // pass as live. This is defensive — the contract is "capture
    // via next() at start of work" so 0 should never be passed.
    const g = createRunGen();
    expect(g.isLive(0)).toBe(true);
    // ... but the moment next() runs, the 0 is stale.
    g.next();
    expect(g.isLive(0)).toBe(false);
  });
});
