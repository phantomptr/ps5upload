import { describe, expect, it } from "vitest";

import { backStep, endText, focusWhere, pathLabel, pickWarning, showsGuide } from "./JoinView";

describe("guest's Back and Esc, for a TV's remote (#926)", () => {
  const base = { checking: false, guide: false, where: "game" as const, full: false };

  it("closes what is open first: the controller check, then the guide", () => {
    expect(backStep({ ...base, checking: true, guide: true, full: true })).toBe("close-check");
    expect(backStep({ ...base, guide: true, full: true })).toBe("close-guide");
  });

  it("from the game it brings the buttons, from the buttons it leaves full screen, then lets go", () => {
    expect(backStep({ ...base, full: true })).toBe("show-controls");
    expect(backStep({ ...base, where: "controls", full: true })).toBe("leave-full");
    expect(backStep({ ...base, where: "controls" })).toBeNull();
    // On another part of the app: that screen's Back.
    expect(backStep({ ...base, where: "page" })).toBeNull();
    expect(backStep({ ...base, where: "page", full: true })).toBe("leave-full");
  });

  it("knows where the focus is", () => {
    const body = { closest: () => null };
    const gameSpot = { closest: (s: string) => (s === "[data-join-game]" ? {} : null) };
    const button = { closest: () => null };
    const elsewhere = { closest: () => null };
    const view = { contains: (n: never) => n === (button as never) || n === (gameSpot as never) };
    expect(focusWhere(null, body, view)).toBe("game");
    expect(focusWhere(body, body, view)).toBe("game");
    expect(focusWhere(gameSpot, body, view)).toBe("game");
    expect(focusWhere(button, body, view)).toBe("controls");
    expect(focusWhere(elsewhere, body, view)).toBe("page");
    expect(focusWhere(button, body, null)).toBe("page");
  });
});

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

describe("guest page: never the host's own PS5 user (2026-11-26)", () => {
  const picking = { state: "picking_user" as const, moveFirst: true, hostUser: "Pat", tookHost: false };

  it("says to move to Quick Play first and names the host's user while the TV may ask", () => {
    expect(pickWarning(tr, picking, "Ann")).toBe(
      "Move to Quick Play before pressing ✕. Don't pick Pat: that's Ann's user, and it ends their game.",
    );
    // The name not known (yet), or an engine from before it: the host's user all the same.
    expect(pickWarning(tr, { ...picking, hostUser: null }, "Ann")).toContain("Don't pick Ann's PS5 user");
    expect(pickWarning(tr, { ...picking, moveFirst: false, hostUser: null }, "Ann")).toContain("Don't pick Ann's PS5 user");
    // In the menus right after their pick (✕ still after a move).
    expect(pickWarning(tr, { ...picking, state: "playing" }, "Ann")).toContain("Don't pick Pat");
  });

  it("says it more firmly once the host's game ended while they picked", () => {
    expect(pickWarning(tr, { ...picking, tookHost: true }, "Ann")).toBe(
      "Ann's game stopped because their PS5 user was picked on your controller. This time, move to Quick Play first, then press ✕.",
    );
  });

  it("says nothing while they play, or with no status yet", () => {
    expect(pickWarning(tr, { ...picking, state: "playing", moveFirst: false }, "Ann")).toBeNull();
    expect(pickWarning(tr, null, "Ann")).toBeNull();
  });
});
