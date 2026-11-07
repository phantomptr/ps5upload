import { beforeEach, describe, expect, it, vi } from "vitest";

const data = new Map<string, string>();
vi.stubGlobal("window", {
  localStorage: {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    removeItem: (k: string) => void data.delete(k),
  },
});

import {
  markMinionHandled,
  minionDefaultApplies,
  minionHandled,
  minionNeedsReapply,
  minionOffForNew,
  minionRememberedOff,
  rememberMinion,
  setMinionOffForNew,
} from "./minionMemory";

describe("Minion kept off from this computer", () => {
  beforeEach(() => data.clear());

  it("remembers off per console, by host, and forgets it on", () => {
    rememberMinion("192.168.1.9:9113", true);
    expect(minionRememberedOff("192.168.1.9")).toBe(true);
    expect(minionRememberedOff("192.168.1.10")).toBe(false);
    rememberMinion("192.168.1.9", false);
    expect(minionRememberedOff("192.168.1.9")).toBe(false);
  });

  it("turns Minion off again only when the console has no settings file and is on", () => {
    const on = { supported: true, enabled: true, saved: false };
    expect(minionNeedsReapply(true, on)).toBe(true);
    // Someone chose "on" on the console (or another computer): its file says so; leave it.
    expect(minionNeedsReapply(true, { ...on, saved: true })).toBe(false);
    expect(minionNeedsReapply(true, { ...on, enabled: false })).toBe(false);
    expect(minionNeedsReapply(false, on)).toBe(false);
    // An older helper says nothing about its file: never guessed.
    expect(minionNeedsReapply(true, { supported: true, enabled: true })).toBe(false);
  });

  it("keeps Minion off on new consoles only, once each", () => {
    expect(minionOffForNew()).toBe(false);
    // Turning it on leaves the consoles already known as they are.
    setMinionOffForNew(true, ["192.168.1.9:9113"]);
    expect(minionOffForNew()).toBe(true);
    expect(minionHandled("192.168.1.9")).toBe(true);
    expect(minionHandled("192.168.1.20")).toBe(false);
    const on = { supported: true, enabled: true };
    expect(minionDefaultApplies(true, false, on)).toBe(true);
    // Once handled (applied, or chosen by hand), never again.
    markMinionHandled("192.168.1.20");
    expect(minionDefaultApplies(true, minionHandled("192.168.1.20"), on)).toBe(false);
    expect(minionDefaultApplies(false, false, on)).toBe(false);
    expect(minionDefaultApplies(true, false, { ...on, enabled: false })).toBe(false);
    // An older helper without the switch is not guessed at.
    expect(minionDefaultApplies(true, false, { supported: true })).toBe(false);
    expect(minionDefaultApplies(true, false, { supported: false })).toBe(false);
  });
});
