import { describe, expect, it } from "vitest";

import { pickScrollRoot } from "./useScrollLock";

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
