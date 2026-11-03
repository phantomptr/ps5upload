import { describe, expect, it, vi } from "vitest";

const { mockIsTauri } = vi.hoisted(() => ({ mockIsTauri: vi.fn(() => true) }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => mockIsTauri() }));

import { engineIsOnThisDevice, getEngineUrl, isLoopbackUrl, setLiveEngineUrl, useEngineStore } from "./engine";

describe("isLoopbackUrl", () => {
  it("recognises this device's loopback in every spelling", () => {
    for (const u of [
      "http://127.0.0.1:19113",
      "http://127.0.0.2:19113",
      "http://localhost:19113",
      "http://LOCALHOST:19113",
      "http://[::1]:19113",
    ]) {
      expect(isLoopbackUrl(u), u).toBe(true);
    }
  });

  it("treats LAN hosts and junk as not this device", () => {
    for (const u of ["http://192.168.1.10:19113", "http://homelab.local:19113", "not a url", ""]) {
      expect(isLoopbackUrl(u), u).toBe(false);
    }
  });
});

describe("engineIsOnThisDevice", () => {
  it("is true for the desktop app's own sidecar and false for a remote engine", () => {
    mockIsTauri.mockReturnValue(true);
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
    expect(engineIsOnThisDevice()).toBe(true);
    useEngineStore.getState().setEngineUrl("http://192.168.1.10:19113");
    expect(engineIsOnThisDevice()).toBe(false);
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
  });

  it("is false in the browser build, whose engine is the server it came from", () => {
    mockIsTauri.mockReturnValue(false);
    expect(engineIsOnThisDevice()).toBe(false);
  });
});

describe("getEngineUrl", () => {
  it("follows this device's engine to its fallback port, but never over a chosen self-hosted engine", () => {
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
    setLiveEngineUrl("http://127.0.0.1:19114");
    expect(getEngineUrl()).toBe("http://127.0.0.1:19114");
    // A self-hosted engine chosen: the app's own engine starting up (and reporting its port)
    // doesn't take it back.
    useEngineStore.getState().setEngineUrl("http://192.168.1.20:19113");
    setLiveEngineUrl("http://127.0.0.1:19113");
    expect(getEngineUrl()).toBe("http://192.168.1.20:19113");
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
  });
});
