import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { activateFocusTrap, closeTopModal, openModalCount, registerBackDismiss } from "./focusTrap";
import { installAppBack, installBrowserBack } from "./systemBack";

// No jsdom in this repo: just enough of `document`, KeyboardEvent and a window's history.
class FakeKey extends Event {
  key: string;
  constructor(type: string, init: { key: string; bubbles?: boolean; cancelable?: boolean }) {
    super(type, init);
    this.key = init.key;
  }
}

/** A key target, as `window` is to the focus trap, with the Escape presses routed to it. */
function keyTarget() {
  const listeners = new Set<(e: KeyboardEvent) => void>();
  return {
    addEventListener: (_t: "keydown", fn: (e: KeyboardEvent) => void) => void listeners.add(fn),
    removeEventListener: (_t: "keydown", fn: (e: KeyboardEvent) => void) => void listeners.delete(fn),
    press: (e: Event) => {
      for (const l of [...listeners]) l(e as KeyboardEvent);
      return true;
    },
  };
}

function modal(target: ReturnType<typeof keyTarget>, onEscape: () => void) {
  return activateFocusTrap(() => null, target, { onEscape });
}

let target: ReturnType<typeof keyTarget>;

beforeEach(() => {
  target = keyTarget();
  vi.stubGlobal("KeyboardEvent", FakeKey);
  vi.stubGlobal("document", { activeElement: null, dispatchEvent: (e: Event) => target.press(e) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("closeTopModal", () => {
  it("is false with nothing open, so the shell goes on to move the history", () => {
    expect(openModalCount()).toBe(0);
    expect(closeTopModal()).toBe(false);
  });

  it("closes only the top one, as Escape does", () => {
    const closed: string[] = [];
    const a = modal(target, () => closed.push("under"));
    const b = modal(target, () => closed.push("top"));
    expect(closeTopModal()).toBe(true);
    expect(closed).toEqual(["top"]);
    b();
    expect(closeTopModal()).toBe(true);
    expect(closed).toEqual(["top", "under"]);
    a();
  });
});

describe("the Android app's Back", () => {
  it("answers true while a dialog takes the press, false once none is open", () => {
    const w = {} as Window;
    installAppBack(w);
    const closed = vi.fn();
    const undo = modal(target, closed);
    expect(w.__ps5uBack?.()).toBe(true);
    expect(closed).toHaveBeenCalledOnce();
    undo();
    expect(w.__ps5uBack?.()).toBe(false);
  });
});

describe("Back in a phone's browser", () => {
  function fakeWindow() {
    const pop = new Set<() => void>();
    const w = {
      location: { href: "http://nas/app" },
      history: {
        state: { idx: 1 },
        pushState: vi.fn(),
        back: vi.fn(),
      },
      addEventListener: (_t: string, fn: () => void) => void pop.add(fn),
      removeEventListener: (_t: string, fn: () => void) => void pop.delete(fn),
    };
    return { w: w as unknown as Window, h: w.history, loc: w.location, firePop: () => pop.forEach((f) => f()) };
  }

  beforeEach(() => void vi.useFakeTimers());

  it("adds a history entry while a dialog is open and uses it to close the dialog", () => {
    const { w, h, firePop } = fakeWindow();
    const off = installBrowserBack(w);
    let undo: (() => void) | null = null;
    const closed = vi.fn(() => {
      undo?.();
      undo = null;
    });
    undo = modal(target, closed);
    vi.runAllTimers();
    expect(h.pushState).toHaveBeenCalledOnce();
    firePop();
    expect(closed).toHaveBeenCalledOnce();
    vi.runAllTimers();
    // The entry was used up by Back: nothing more to take off.
    expect(h.back).not.toHaveBeenCalled();
    expect(h.pushState).toHaveBeenCalledOnce();
    off();
  });

  it("takes its entry off when the dialog closes by a button", () => {
    const { w, h } = fakeWindow();
    const off = installBrowserBack(w);
    const undo = modal(target, () => {});
    vi.runAllTimers();
    undo();
    vi.runAllTimers();
    expect(h.back).toHaveBeenCalledOnce();
    off();
  });

  it("leaves history alone when a confirm hands straight over to another dialog", () => {
    const { w, h } = fakeWindow();
    const off = installBrowserBack(w);
    const first = modal(target, () => {});
    vi.runAllTimers();
    first();
    const second = modal(target, () => {});
    vi.runAllTimers();
    expect(h.pushState).toHaveBeenCalledOnce();
    expect(h.back).not.toHaveBeenCalled();
    second();
    vi.runAllTimers();
    off();
  });

  it("does not go back after the dialog's link changed the page", () => {
    const { w, h, loc } = fakeWindow();
    const off = installBrowserBack(w);
    const undo = modal(target, () => {});
    vi.runAllTimers();
    loc.href = "http://nas/elsewhere";
    undo();
    vi.runAllTimers();
    expect(h.back).not.toHaveBeenCalled();
    off();
  });

  it("keeps Back with a dialog still open underneath", () => {
    const { w, h, firePop } = fakeWindow();
    const off = installBrowserBack(w);
    const under = modal(target, () => {});
    let top: (() => void) | null = modal(target, () => {
      top?.();
      top = null;
    });
    vi.runAllTimers();
    firePop();
    vi.runAllTimers();
    expect(h.pushState).toHaveBeenCalledTimes(2);
    expect(h.back).not.toHaveBeenCalled();
    under();
    vi.runAllTimers();
    off();
  });
});

describe("a surface that is not modal (a sheet, a menu)", () => {
  it("closes on Back, newest first, in order with the modals", () => {
    const closed: string[] = [];
    const a = registerBackDismiss(() => closed.push("sheet"));
    const b = modal(target, () => closed.push("modal"));
    const c = registerBackDismiss(() => closed.push("menu"));
    expect(openModalCount()).toBe(3);
    expect(closeTopModal()).toBe(true);
    c();
    expect(closeTopModal()).toBe(true);
    b();
    expect(closeTopModal()).toBe(true);
    a();
    expect(closed).toEqual(["menu", "modal", "sheet"]);
    expect(openModalCount()).toBe(0);
    expect(closeTopModal()).toBe(false);
  });
});
