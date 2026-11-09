import { beforeEach, describe, expect, it, vi } from "vitest";

const store = vi.hoisted(() => new Map<string, string>());
vi.mock("./safeStorage", () => ({
  safeGetItem: (k: string) => store.get(k) ?? null,
  safeSetItem: (k: string, v: string) => void store.set(k, v),
}));

import { isSavableRoute, landingRoute, readLastRoute, saveLastRoute } from "./lastRoute";

describe("last route", () => {
  beforeEach(() => store.clear());

  it("reopens the last screen, including its tab", () => {
    saveLastRoute("/games", "?tab=files");
    expect(readLastRoute()).toBe("/games?tab=files");
  });

  it("never saves or reopens the landing, the wizard or release notes", () => {
    for (const p of ["/", "/first-run", "/whats-new"]) {
      expect(isSavableRoute(p)).toBe(false);
      saveLastRoute(p, "");
      expect(readLastRoute()).toBeNull();
    }
  });

  it("ignores a stored value that isn't an in-app path", () => {
    store.set("ps5upload.last_route", "//evil.example/x");
    expect(readLastRoute()).toBeNull();
    store.set("ps5upload.last_route", "https://evil.example");
    expect(readLastRoute()).toBeNull();
  });

  it("opens Connection for a fresh install, and never by itself for a returning user", () => {
    expect(landingRoute(false, "/games")).toBe("/connection");
    expect(landingRoute(true, null)).toBe("/home");
    expect(landingRoute(true, "/games?tab=files")).toBe("/games?tab=files");
    // Left on Connection last time (a console looked down): the next start lands on Home,
    // not on Connection while the first check still runs.
    expect(landingRoute(true, "/connection")).toBe("/home");
    expect(landingRoute(true, "/connection?tab=devices")).toBe("/home");
    saveLastRoute("/connection", "?tab=health");
    expect(landingRoute(true)).toBe("/home");
  });
});
