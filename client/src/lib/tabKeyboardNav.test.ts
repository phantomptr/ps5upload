/**
 * Off-by-one and cyclic-boundary tests for makeTabKeyHandler. The
 * handler is plain logic — no React — but it does touch
 * `document.getElementById` for the focus side-effect. Vitest runs
 * in a node env here (no DOM); we stub document.getElementById via
 * vi.stubGlobal, mirroring the pattern in fsLastPath.test.ts.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { makeTabKeyHandler } from "./tabKeyboardNav";

type Key = "a" | "b" | "c";
const TABS: Key[] = ["a", "b", "c"];

function fakeEvent(key: string): {
  key: string;
  preventDefault: () => void;
  _prevented: boolean;
} {
  const ev = { key, _prevented: false, preventDefault: () => {} };
  ev.preventDefault = () => {
    ev._prevented = true;
  };
  return ev;
}

describe("makeTabKeyHandler", () => {
  // Typed mock — the bare `ReturnType<typeof vi.fn>` is too generic
  // to satisfy `(next: Key) => void`, which makeTabKeyHandler asks for.
  let setActive: ReturnType<typeof vi.fn<(next: Key) => void>>;
  let focused: string[];

  beforeEach(() => {
    setActive = vi.fn<(next: Key) => void>();
    focused = [];
    vi.stubGlobal("document", {
      getElementById: (id: string) => ({
        focus: () => focused.push(id),
      }),
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  const handler = () =>
    makeTabKeyHandler<Key>(TABS, setActive, (id) => `tab-${id}`);

  it("ArrowRight from a → b", () => {
    const ev = fakeEvent("ArrowRight");
    handler()(ev as any, "a");
    expect(setActive).toHaveBeenCalledWith("b");
    expect(focused).toEqual(["tab-b"]);
    expect(ev._prevented).toBe(true);
  });

  it("ArrowRight from c wraps to a", () => {
    const ev = fakeEvent("ArrowRight");
    handler()(ev as any, "c");
    expect(setActive).toHaveBeenCalledWith("a");
    expect(focused).toEqual(["tab-a"]);
  });

  it("ArrowLeft from a wraps to c", () => {
    const ev = fakeEvent("ArrowLeft");
    handler()(ev as any, "a");
    expect(setActive).toHaveBeenCalledWith("c");
    expect(focused).toEqual(["tab-c"]);
  });

  it("ArrowLeft from b → a", () => {
    const ev = fakeEvent("ArrowLeft");
    handler()(ev as any, "b");
    expect(setActive).toHaveBeenCalledWith("a");
  });

  it("Home from c → a", () => {
    const ev = fakeEvent("Home");
    handler()(ev as any, "c");
    expect(setActive).toHaveBeenCalledWith("a");
    expect(focused).toEqual(["tab-a"]);
  });

  it("End from a → c", () => {
    const ev = fakeEvent("End");
    handler()(ev as any, "a");
    expect(setActive).toHaveBeenCalledWith("c");
    expect(focused).toEqual(["tab-c"]);
  });

  it("non-nav keys are ignored — no setActive, no preventDefault", () => {
    const ev = fakeEvent("Enter");
    handler()(ev as any, "a");
    expect(setActive).not.toHaveBeenCalled();
    expect(focused).toEqual([]);
    expect(ev._prevented).toBe(false);
  });

  it("Tab key passes through (browser handles focus traversal)", () => {
    const ev = fakeEvent("Tab");
    handler()(ev as any, "b");
    expect(setActive).not.toHaveBeenCalled();
    expect(ev._prevented).toBe(false);
  });
});
