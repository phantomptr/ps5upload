import { describe, expect, it } from "vitest";

import { BAR_STRIP_PX, EscapeAttemptDetector, ESC_TWICE_MS, PUSH_GAP_MS } from "./kbmEscape";

function detector() {
  let t = 1000;
  const d = new EscapeAttemptDetector(() => t);
  return { d, tick: (ms: number) => (t += ms) };
}

describe("EscapeAttemptDetector", () => {
  it("Esc twice within a second is an attempt; slower is not", () => {
    const { d, tick } = detector();
    expect(d.key("Escape", true)).toBe(null);
    tick(ESC_TWICE_MS + 1);
    expect(d.key("Escape", true)).toBe(null);
    tick(400);
    expect(d.key("Escape", true, true)).toBe(null); // a repeat is not a press
    expect(d.key("Escape", true)).toBe("esc_twice");
    // Counting starts over.
    tick(100);
    expect(d.key("Escape", true)).toBe(null);
  });

  it("the ⌘ / Windows key is an attempt", () => {
    const { d } = detector();
    expect(d.key("MetaLeft", true)).toBe("system_key");
    expect(d.key("MetaLeft", false)).toBe(null);
    expect(d.key("KeyW", true)).toBe(null);
  });

  it("three pushes at the same edge within a few seconds is an attempt", () => {
    const { d, tick } = detector();
    d.start(500, 300, 1000, 600);
    const pushUp = () => {
      for (let i = 0; i < 10; i++) {
        tick(10);
        const r = d.move(0, -100);
        if (r) return r;
      }
      tick(PUSH_GAP_MS + 10);
      return null;
    };
    expect(pushUp()).toBe(null);
    expect(pushUp()).toBe(null);
    expect(pushUp()).toBe("edge");
  });

  it("one long turn is one push, and moving back and forth inside the picture is none", () => {
    const { d, tick } = detector();
    d.start(500, 300, 1000, 600);
    for (let i = 0; i < 200; i++) {
      tick(8);
      expect(d.move(60, 0)).toBe(null); // a long turn to the right
    }
    for (let i = 0; i < 100; i++) {
      tick(8);
      expect(d.move(i % 2 ? 300 : -300, 0)).toBe(null);
    }
  });

  it("pushes at different edges do not add up", () => {
    const { d, tick } = detector();
    d.start(500, 300, 1000, 600);
    const push = (dx: number, dy: number) => {
      let r = null;
      for (let i = 0; i < 12 && !r; i++) {
        tick(10);
        r = d.move(dx, dy);
      }
      tick(PUSH_GAP_MS + 10);
      return r;
    };
    expect(push(0, -100)).toBe(null);
    expect(push(100, 0)).toBe(null);
    expect(push(0, -100)).toBe(null);
    expect(push(-100, 0)).toBe(null);
  });

  it("a click with the virtual pointer in the bar's strip is an attempt", () => {
    const { d } = detector();
    expect(d.click()).toBe(null); // not started
    d.start(500, 300, 1000, 600);
    expect(d.click()).toBe(null);
    d.move(0, -(300 - BAR_STRIP_PX + 5));
    expect(d.click()).toBe("bar_click");
  });
});
