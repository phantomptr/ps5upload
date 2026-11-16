import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { recallRoute, rememberRoute, staysOnConsoleSwitch } from "./consoleRoutes";

describe("consoleRoutes", () => {
  // The node test environment has no localStorage: a minimal in-memory one.
  beforeEach(() => {
    const store = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
        removeItem: (k: string) => void store.delete(k),
      },
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("goes back to where each console was left", () => {
    rememberRoute("192.168.1.20", "/files?path=/data");
    rememberRoute("192.168.1.21", "/games");
    expect(recallRoute("192.168.1.20")).toBe("/files?path=/data");
    expect(recallRoute("192.168.1.21")).toBe("/games");
  });

  it("knows nothing about a console not used here yet", () => {
    expect(recallRoute("192.168.1.99")).toBeNull();
    expect(recallRoute("")).toBeNull();
  });

  it("treats an address with a port as the same console", () => {
    rememberRoute("192.168.1.20:9113", "/saves");
    expect(recallRoute("192.168.1.20")).toBe("/saves");
  });

  it("keeps the most recently used consoles when the list is long", () => {
    for (let i = 0; i < 40; i++) rememberRoute(`10.0.0.${i}`, `/r${i}`, i);
    expect(recallRoute("10.0.0.0")).toBeNull();
    expect(recallRoute("10.0.0.39")).toBe("/r39");
  });

  it("stays on the Consoles page when another console is picked: it shows the picked one", () => {
    expect(staysOnConsoleSwitch("/connection")).toBe(true);
    expect(staysOnConsoleSwitch("/connection?console=p2&devices=1")).toBe(true);
    expect(staysOnConsoleSwitch("/connection?tab=devices")).toBe(false);
    expect(staysOnConsoleSwitch("/connection?tab=health")).toBe(false);
    expect(staysOnConsoleSwitch("/games")).toBe(false);
  });
});
