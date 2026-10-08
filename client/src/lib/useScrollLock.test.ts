import { describe, expect, it } from "vitest";

import { activeScrollLocks, lockScrollRoot, pickScrollRoot } from "./useScrollLock";

// A screen kept alive behind another one is `display: none`: it has no boxes.
const el = (name: string, shown: boolean) => ({
  name,
  getClientRects: () => ({ length: shown ? 1 : 0 }),
});

describe("pickScrollRoot", () => {
  it("takes the scroll root that is on show, wherever it is in the page", () => {
    // A hidden screen can still carry the marker for a moment: React re-renders hidden
    // trees late. The one the user sees is the one to lock.
    expect(pickScrollRoot([el("hidden", false), el("shown", true)])?.name).toBe("shown");
    expect(pickScrollRoot([el("shown", true), el("hidden", false)])?.name).toBe("shown");
  });

  it("falls back to the first when none has a box, and to nothing when there is none", () => {
    expect(pickScrollRoot([el("a", false), el("b", false)])?.name).toBe("a");
    expect(pickScrollRoot([])).toBeNull();
  });
});

describe("lockScrollRoot", () => {
  it("puts back the root it locked, even after another console's screen is on show", () => {
    // Console A's screen locks its root (a dialog opens), the user switches to console B,
    // then the dialog closes. A must scroll again and B must be left alone (the bug: B was
    // "restored" and A stayed at overflow:hidden).
    const a = { style: { overflow: "" } };
    const b = { style: { overflow: "auto" } };
    const releaseA = lockScrollRoot(a);
    expect(a.style.overflow).toBe("hidden");
    releaseA();
    expect(a.style.overflow).toBe("");
    expect(b.style.overflow).toBe("auto");
    expect(activeScrollLocks()).toBe(0);
  });

  it("counts stacked overlays per root and releases each once", () => {
    const r = { style: { overflow: "" } };
    const one = lockScrollRoot(r);
    const two = lockScrollRoot(r);
    one();
    one(); // a second release of the same lock does nothing
    expect(r.style.overflow).toBe("hidden");
    two();
    expect(r.style.overflow).toBe("");
  });
});
