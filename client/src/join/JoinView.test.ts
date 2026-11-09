import { describe, expect, it } from "vitest";

import { endText, pathLabel, showsGuide } from "./JoinView";

// The fallback with the variables filled in (what an untranslated key shows).
const tr = ((_key: string, vars?: Record<string, string | number> | string, fallback?: string) => {
  const text = typeof vars === "string" ? vars : (fallback ?? _key);
  const v = typeof vars === "object" ? vars : {};
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in v ? String(v[k]) : m));
}) as Parameters<typeof endText>[0];

describe("guest page: which way the game comes", () => {
  it("names the path beside the delay", () => {
    expect(pathLabel(tr, "lan")).toBe("Home network");
    expect(pathLabel(tr, "tailscale")).toBe("Tailscale");
    expect(pathLabel(tr, "internet")).toBe("Internet");
  });
});

describe("guest page: the host's stream's controller (#687, #720)", () => {
  it("Player 1 plays as the host's user: no 'press PS, pick Quick Play' guide", () => {
    expect(showsGuide({ player: 1, guided: false, movedOff: false, reconnecting: false })).toBe(false);
    expect(showsGuide({ player: 2, guided: false, movedOff: false, reconnecting: false })).toBe(true);
    expect(showsGuide({ player: 2, guided: true, movedOff: false, reconnecting: false })).toBe(false);
    // Moved off it onto a controller of their own: they pick a user now, so the guide is back.
    expect(showsGuide({ player: 2, guided: true, movedOff: true, reconnecting: false })).toBe(true);
    expect(showsGuide({ player: 2, guided: false, movedOff: false, reconnecting: true })).toBe(false);
  });

  it("explains a seat the host needed back", () => {
    for (const stage of [
      { kind: "ended", why: "seat_taken" },
      { kind: "refused", why: "seat_taken" },
    ] as const) {
      const text = endText(tr, stage, "Ann");
      expect(text).toContain("Ann needed their controller back");
      expect(text).toContain("let you in again");
    }
  });
});
