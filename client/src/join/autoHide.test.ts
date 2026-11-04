import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AutoHide, HIDE_AFTER_MS } from "./autoHide";

describe("AutoHide", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const make = () => {
    const seen: boolean[] = [];
    const a = new AutoHide((on) => seen.push(on));
    return { a, seen };
  };

  it("always shows while not enabled", () => {
    const { a } = make();
    a.set(false, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS * 5);
    expect(a.visible).toBe(true);
    a.hide();
    expect(a.visible).toBe(true);
  });

  it("hides HIDE_AFTER_MS after it is enabled, and a poke brings it back for as long", () => {
    const { a, seen } = make();
    a.set(true, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS - 1);
    expect(a.visible).toBe(true);
    vi.advanceTimersByTime(1);
    expect(a.visible).toBe(false);
    a.poke();
    expect(a.visible).toBe(true);
    vi.advanceTimersByTime(HIDE_AFTER_MS);
    expect(a.visible).toBe(false);
    expect(seen).toEqual([false, true, false]);
  });

  it("restarts the moment on every poke", () => {
    const { a } = make();
    a.set(true, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS - 100);
    a.poke();
    vi.advanceTimersByTime(HIDE_AFTER_MS - 100);
    expect(a.visible).toBe(true);
    vi.advanceTimersByTime(100);
    expect(a.visible).toBe(false);
  });

  it("never hides while pinned, and gives a full moment once let go", () => {
    const { a } = make();
    a.set(true, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS);
    expect(a.visible).toBe(false);
    a.set(true, true);
    expect(a.visible).toBe(true);
    vi.advanceTimersByTime(HIDE_AFTER_MS * 3);
    a.hide();
    expect(a.visible).toBe(true);
    a.set(true, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS - 1);
    expect(a.visible).toBe(true);
    vi.advanceTimersByTime(1);
    expect(a.visible).toBe(false);
  });

  it("hides at once on hide(), and re-setting the same state does not restart the moment", () => {
    const { a } = make();
    a.set(true, false);
    a.hide();
    expect(a.visible).toBe(false);
    a.set(true, false);
    expect(a.visible).toBe(false);
    a.poke();
    vi.advanceTimersByTime(HIDE_AFTER_MS / 2);
    a.set(true, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS / 2);
    expect(a.visible).toBe(false);
  });

  it("disabling shows them and stops the timer; dispose stops it too", () => {
    const { a, seen } = make();
    a.set(true, false);
    a.set(false, false);
    vi.advanceTimersByTime(HIDE_AFTER_MS * 2);
    expect(a.visible).toBe(true);
    expect(seen).toEqual([]);
    a.set(true, false);
    a.dispose();
    vi.advanceTimersByTime(HIDE_AFTER_MS * 2);
    expect(seen).toEqual([]);
  });
});

describe("AutoHide on touch (sticky)", () => {
  it("starts hidden, shows and hides only when asked, never by a timer", () => {
    vi.useFakeTimers();
    const seen: boolean[] = [];
    const a = new AutoHide((v) => seen.push(v), 1000);
    a.set(true, false, true);
    expect(a.visible).toBe(false);
    a.toggle();
    expect(a.visible).toBe(true);
    vi.advanceTimersByTime(5000);
    expect(a.visible).toBe(true);
    a.toggle();
    expect(a.visible).toBe(false);
    a.poke();
    a.hide();
    expect(a.visible).toBe(false);
    a.set(true, true, true);
    expect(a.visible).toBe(true);
    a.set(false, false, true);
    expect(a.visible).toBe(true);
    a.dispose();
    vi.useRealTimers();
  });
});

describe("AutoHide on touch: a pin let go", () => {
  it("keeps them showing (only asking hides them)", () => {
    const a = new AutoHide(() => {}, 1000);
    a.set(true, false, true);
    a.poke();
    a.set(true, true, true);
    a.set(true, false, true);
    expect(a.visible).toBe(true);
  });
});
