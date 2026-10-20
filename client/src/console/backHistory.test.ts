import { describe, expect, it, vi } from "vitest";
import { BACK_ECHO_MS, createBackHistory, isPs5Browser, type HistoryEnv } from "./backHistory";

/** A browser history in memory: entries, popstate, a clock and a deferred queue. */
function fakeEnv(startsOnGuard = false) {
  const entries: string[] = startsOnGuard ? ["page", "guard"] : ["page"];
  let pop: (() => void) | null = null;
  let clock = 0;
  const queue: (() => void)[] = [];
  const env: HistoryEnv = {
    push: vi.fn(() => {
      entries.push("guard");
    }),
    back: vi.fn(() => {
      // history.back() is async: the popstate comes later.
      queue.push(() => {
        entries.pop();
        pop?.();
      });
    }),
    onPop: (fn) => {
      pop = fn;
      return () => {
        pop = null;
      };
    },
    now: () => clock,
    defer: (fn) => {
      queue.push(fn);
    },
    startsOnGuard: () => startsOnGuard,
  };
  return {
    env,
    entries,
    /** Run everything queued (React commits, async pops) until nothing is left. */
    flush() {
      while (queue.length) queue.shift()!();
    },
    /** The browser's own Back (the PS5's ○): leaves the page when only "page" is left. */
    browserBack(): "left" | "stayed" {
      if (entries.length === 1) return "left";
      entries.pop();
      pop?.();
      return "stayed";
    },
    tick(ms: number) {
      clock += ms;
    },
    /** A popstate that moves nothing in history (WebKit fires one as a page loads). */
    spuriousPop() {
      pop?.();
    },
  };
}

/** The page: a stack of levels (Files folder, modal...), the back action closes the top one. */
function page(f: ReturnType<typeof fakeEnv>) {
  const open: (() => void)[] = [];
  const runBack = vi.fn(() => {
    open.pop()?.();
  });
  const h = createBackHistory(f.env, runBack);
  return {
    h,
    runBack,
    open() {
      open.push(h.addLevel());
      f.flush();
    },
    /** Closed with an on-screen button, not Back. */
    closeByButton() {
      open.pop()?.();
      f.flush();
    },
    depth: () => open.length,
  };
}

describe("back history", () => {
  it("one guard while the page has somewhere to go back to, however deep", () => {
    const f = fakeEnv();
    const p = page(f);
    expect(f.entries).toEqual(["page"]);
    p.open(); // a folder
    p.open(); // a sheet over it
    p.open(); // a dialog from the sheet
    expect(f.entries).toEqual(["page", "guard"]);
    expect(f.env.push).toHaveBeenCalledTimes(1);
  });

  it("○_steps_back_inside_the_page_and_leaves_only_from_the_first_screen", () => {
    const f = fakeEnv();
    const p = page(f);
    p.open();
    p.open();
    f.tick(1000);
    expect(f.browserBack()).toBe("stayed");
    f.flush();
    expect(p.runBack).toHaveBeenCalledTimes(1);
    expect(p.depth()).toBe(1);
    expect(f.entries).toEqual(["page", "guard"]); // put back: still somewhere to go
    f.tick(1000);
    expect(f.browserBack()).toBe("stayed");
    f.flush();
    expect(p.depth()).toBe(0);
    expect(f.entries).toEqual(["page"]); // the first screen: no guard
    expect(f.browserBack()).toBe("left");
    expect(p.runBack).toHaveBeenCalledTimes(2);
  });

  it("a level closed by a button removes the guard, and that pop runs nothing", () => {
    const f = fakeEnv();
    const p = page(f);
    p.open();
    p.closeByButton();
    expect(f.entries).toEqual(["page"]);
    expect(f.env.back).toHaveBeenCalledTimes(1);
    expect(p.runBack).not.toHaveBeenCalled();
    // Closing one of two keeps the guard.
    p.open();
    p.open();
    p.closeByButton();
    expect(f.entries).toEqual(["page", "guard"]);
  });

  it("Escape/Backspace take the same path: history.back() when the guard is up", () => {
    const f = fakeEnv();
    const p = page(f);
    p.open();
    f.tick(1000);
    p.h.requestBack();
    expect(f.env.back).toHaveBeenCalledTimes(1);
    expect(p.runBack).not.toHaveBeenCalled(); // not yet: the popstate runs it
    f.flush();
    expect(p.runBack).toHaveBeenCalledTimes(1);
    expect(f.entries).toEqual(["page"]);
    // No guard (first screen): the back action runs at once, nothing is popped.
    p.h.requestBack();
    expect(p.runBack).toHaveBeenCalledTimes(2);
    expect(f.env.back).toHaveBeenCalledTimes(1);
    expect(f.entries).toEqual(["page"]);
  });

  it("one press reported twice (key and browser Back) steps back once", () => {
    const f = fakeEnv();
    const p = page(f);
    p.open();
    p.open();
    f.tick(1000);
    f.browserBack();
    f.tick(BACK_ECHO_MS - 1);
    f.env.push(); // the browser had more history: a second pop arrives at once
    f.browserBack();
    f.flush();
    expect(p.runBack).toHaveBeenCalledTimes(1);
    expect(p.depth()).toBe(1);
    expect(f.entries[f.entries.length - 1]).toBe("guard");
  });

  it("a reload with the guard on top drops it", () => {
    const f = fakeEnv(true);
    const p = page(f);
    f.flush();
    expect(f.entries).toEqual(["page"]);
    expect(p.runBack).not.toHaveBeenCalled();
  });

  it("isPs5Browser matches the PS5's user agent only", () => {
    expect(isPs5Browser("Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko)")).toBe(true);
    expect(isPs5Browser("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15")).toBe(false);
  });
});

describe("back history on the PS5 (close at the first screen)", () => {
  function ps5(graceMs = 0) {
    const f = fakeEnv();
    f.env.atGuard = () => f.entries[f.entries.length - 1] === "guard";
    const onRoot = vi.fn();
    const runBack = vi.fn();
    const h = createBackHistory(f.env, runBack, { onRoot, graceMs });
    f.flush();
    return { f, h, onRoot, runBack };
  }

  it("○ on the first screen closes the browser instead of leaving the page", () => {
    const { f, onRoot, runBack } = ps5();
    expect(f.entries).toEqual(["page", "guard"]); // a guard even with nothing open
    f.tick(1000);
    expect(f.browserBack()).toBe("stayed");
    f.flush();
    expect(onRoot).toHaveBeenCalledTimes(1);
    expect(runBack).not.toHaveBeenCalled();
    // Still open (close refused): the guard stays down, the next ○ is the browser's own.
    expect(f.entries).toEqual(["page"]);
    expect(f.browserBack()).toBe("left");
  });

  it("a level opened after a refused close puts the first-screen guard back", () => {
    const { f, h, onRoot } = ps5();
    f.tick(1000);
    f.browserBack();
    f.flush();
    expect(onRoot).toHaveBeenCalledTimes(1);
    const close = h.addLevel();
    f.flush();
    expect(f.entries).toEqual(["page", "guard"]);
    close();
    f.flush();
    expect(f.entries).toEqual(["page", "guard"]); // kept for the first screen
  });

  it("closing a level by ○ steps back, and never closes the browser", () => {
    const { f, h, onRoot, runBack } = ps5();
    const close = h.addLevel();
    runBack.mockImplementation(() => close());
    f.flush();
    f.tick(1000);
    f.browserBack();
    f.flush();
    expect(runBack).toHaveBeenCalledTimes(1);
    expect(onRoot).not.toHaveBeenCalled();
    expect(f.entries).toEqual(["page", "guard"]);
  });

  it("a popstate that pops nothing (WebKit at load) is ignored", () => {
    const { f, onRoot, runBack } = ps5();
    f.tick(1000);
    f.spuriousPop();
    f.flush();
    expect(f.entries).toEqual(["page", "guard"]);
    expect(onRoot).not.toHaveBeenCalled();
    expect(runBack).not.toHaveBeenCalled();
  });

  it("before any key or click, a pop inside the load grace only puts the guard back", () => {
    const { f, h, onRoot } = ps5(1500);
    f.tick(500);
    f.browserBack();
    f.flush();
    expect(onRoot).not.toHaveBeenCalled();
    expect(f.entries).toEqual(["page", "guard"]);
    // After a key the same pop is real.
    h.noteInput();
    f.browserBack();
    f.flush();
    expect(onRoot).toHaveBeenCalledTimes(1);
  });

  it("after the grace a pop is real even with no key pressed (○ is not a key on the PS5)", () => {
    const { f, onRoot } = ps5(1500);
    f.tick(1600);
    f.browserBack();
    f.flush();
    expect(onRoot).toHaveBeenCalledTimes(1);
  });
});


it("coalesces two Back requests before the asynchronous history pop", () => {
  const f = fakeEnv();
  const p = page(f);
  p.open();
  p.open();
  f.tick(2000);
  p.h.requestBack();
  p.h.requestBack();
  expect(f.env.back).toHaveBeenCalledTimes(1);
  f.flush();
  expect(p.depth()).toBe(1);
});
