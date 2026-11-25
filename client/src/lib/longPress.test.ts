import { describe, expect, it, vi } from "vitest";

import { createLongPress, LONG_PRESS_MS } from "./longPress";

function setup() {
  const fired: string[] = [];
  let pending: { fn: () => void; at: number } | null = null;
  let now = 0;
  const clock = {
    set: (fn: () => void, ms: number) => (pending = { fn, at: now + ms }),
    clear: () => void (pending = null),
    now: () => now,
  };
  const lp = createLongPress<string>((t) => fired.push(t), { clock });
  const wait = (ms: number) => {
    now += ms;
    if (pending && pending.at <= now) {
      const { fn } = pending;
      pending = null;
      fn();
    }
  };
  return { lp, fired, wait };
}

describe("long press", () => {
  it("fires after the finger holds still, once", () => {
    const { lp, fired, wait } = setup();
    lp.down("a", 10, 10, "touch");
    wait(LONG_PRESS_MS - 1);
    expect(fired).toEqual([]);
    wait(1);
    expect(fired).toEqual(["a"]);
    // Android also reports it as a context menu: the same press, not a second one, and no menu.
    expect(lp.contextMenu()).toBe(true);
    expect(fired).toEqual(["a"]);
  });

  it("swallows the click of the finger lifting, and only that one", () => {
    const { lp, wait } = setup();
    lp.down("a", 0, 0, "touch");
    wait(LONG_PRESS_MS);
    lp.end();
    expect(lp.takeClick()).toBe(true);
    expect(lp.takeClick()).toBe(false);
  });

  it("lets a later tap through when the lift made no click (Android after its own long press)", () => {
    const { lp, wait } = setup();
    lp.down("a", 0, 0, "touch");
    wait(LONG_PRESS_MS);
    lp.end();
    wait(2000);
    // A tap on the bar of actions: no pointer went down on a row first.
    expect(lp.takeClick()).toBe(false);
  });

  it("is a tap when the finger lifts early", () => {
    const { lp, fired, wait } = setup();
    lp.down("a", 0, 0, "touch");
    wait(100);
    lp.end();
    wait(LONG_PRESS_MS);
    expect(fired).toEqual([]);
    expect(lp.takeClick()).toBe(false);
  });

  it("is a scroll when the finger moves away", () => {
    const { lp, fired, wait } = setup();
    lp.down("a", 0, 0, "touch");
    lp.move(3, 4); // 5 px: still holding
    lp.move(0, 30);
    wait(LONG_PRESS_MS);
    expect(fired).toEqual([]);
  });

  it("takes no mouse or pen: a right-click keeps its menu", () => {
    const { lp, fired, wait } = setup();
    lp.down("a", 0, 0, "mouse");
    wait(LONG_PRESS_MS);
    expect(fired).toEqual([]);
    expect(lp.contextMenu()).toBe(false);
  });

  it("ends early on the browser's own long press", () => {
    const { lp, fired, wait } = setup();
    lp.down("b", 0, 0, "touch");
    wait(300);
    expect(lp.contextMenu()).toBe(true);
    expect(fired).toEqual(["b"]);
    wait(LONG_PRESS_MS);
    expect(fired).toEqual(["b"]);
  });

  it("runs on real timers", () => {
    vi.useFakeTimers();
    try {
      const fired: string[] = [];
      const lp = createLongPress<string>((t) => fired.push(t));
      lp.down("c", 0, 0, "touch");
      vi.advanceTimersByTime(LONG_PRESS_MS);
      expect(fired).toEqual(["c"]);
      lp.down("d", 0, 0, "touch");
      lp.dispose();
      vi.advanceTimersByTime(LONG_PRESS_MS);
      expect(fired).toEqual(["c"]);
    } finally {
      vi.useRealTimers();
    }
  });
});
