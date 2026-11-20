import { describe, expect, it } from "vitest";

import { hintToShow, type LinkHistory } from "./linkHistory";

const history = (hint: LinkHistory["hint"]): LinkHistory => ({ events: [], link: null, hint, up: true });
const usb = { kind: "usb_wifi", drops: 4, since_ms: 1_000, last_ms: 5_000, drive_at_ms: 900 } as const;

describe("hintToShow", () => {
  it("shows the engine's hint", () => {
    expect(hintToShow(history(usb), {})).toEqual(usb);
  });

  it("shows nothing when the engine has no hint or no record", () => {
    expect(hintToShow(history(null), {})).toBeNull();
    expect(hintToShow(null, {})).toBeNull();
  });

  it("a dismissal holds for its episode only", () => {
    expect(hintToShow(history(usb), { dismissedSince: 1_000 })).toBeNull();
    // A new episode (a later first drop) shows again.
    expect(hintToShow(history({ ...usb, since_ms: 9_000 }), { dismissedSince: 1_000 })).not.toBeNull();
  });

  it("says nothing about a console in rest mode, or about a kind it does not know", () => {
    expect(hintToShow(history(usb), { resting: true })).toBeNull();
    expect(hintToShow(history({ ...usb, kind: "later_kind" as never }), {})).toBeNull();
  });
});
