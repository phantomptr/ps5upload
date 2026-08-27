import { describe, expect, it } from "vitest";
import { panelAnchorClass } from "./NotificationInbox";

/**
 * The Android bug this pins: the bell renders in two places that sit on
 * opposite sides of the window — the desktop sidebar footer (bottom LEFT)
 * and the mobile More footer (bottom RIGHT) — but the panel was hardcoded
 * `left-0`. In the second position that grew a 320px panel straight off the
 * right edge of the phone.
 */
describe("panelAnchorClass", () => {
  it("anchors left for the desktop sidebar footer", () => {
    // The bell is at the window's left edge there, so the panel must grow
    // rightward into the content area.
    expect(panelAnchorClass("left")).toBe("left-0");
  });

  it("anchors right for the mobile More footer", () => {
    // The bell is at the window's right edge there. Growing rightward is
    // what put the panel off-screen on Android.
    expect(panelAnchorClass("right")).toBe("right-0");
  });

  it("never returns both anchors, which would stretch the panel edge to edge", () => {
    for (const align of ["left", "right"] as const) {
      const cls = panelAnchorClass(align);
      expect(cls.includes("left-0") && cls.includes("right-0")).toBe(false);
    }
  });
});
