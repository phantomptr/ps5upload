import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { RESTORE_SETTLE_MS, relayoutElement, watchRestore } from "./repaintOnRestore";

function fakeWin() {
  const target = new EventTarget();
  const w = {
    document: { visibilityState: "visible" as DocumentVisibilityState },
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms) as unknown as number,
    clearTimeout: (id: number) => clearTimeout(id),
    fire: (t: string) => target.dispatchEvent(new Event(t)),
  };
  return w;
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("watchRestore (#652)", () => {
  it("lays out once after a burst of resizes settles", () => {
    const w = fakeWin();
    const fn = vi.fn();
    watchRestore(w as never, fn);
    w.fire("resize");
    vi.advanceTimersByTime(RESTORE_SETTLE_MS - 10);
    w.fire("resize");
    vi.advanceTimersByTime(RESTORE_SETTLE_MS - 10);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(20);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("lays out when the window is shown again, not when it is hidden", () => {
    const w = fakeWin();
    const fn = vi.fn();
    watchRestore(w as never, fn);
    w.document.visibilityState = "hidden";
    w.fire("visibilitychange");
    vi.advanceTimersByTime(RESTORE_SETTLE_MS * 2);
    expect(fn).not.toHaveBeenCalled();
    w.document.visibilityState = "visible";
    w.fire("visibilitychange");
    vi.advanceTimersByTime(RESTORE_SETTLE_MS);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it("stops with a pending layout dropped", () => {
    const w = fakeWin();
    const fn = vi.fn();
    const stop = watchRestore(w as never, fn);
    w.fire("focus");
    stop();
    vi.advanceTimersByTime(RESTORE_SETTLE_MS * 2);
    w.fire("resize");
    vi.advanceTimersByTime(RESTORE_SETTLE_MS * 2);
    expect(fn).not.toHaveBeenCalled();
  });

  it("puts the element's own display back", () => {
    let reads = 0;
    const el = {
      style: { display: "block" },
      get offsetWidth() {
        reads++;
        expect(this.style.display).toBe("none");
        return 0;
      },
    };
    relayoutElement(el as unknown as HTMLElement);
    expect(reads).toBe(1);
    expect(el.style.display).toBe("block");
  });
});
