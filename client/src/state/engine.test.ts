import { describe, expect, it, vi } from "vitest";

const { mockIsTauri } = vi.hoisted(() => ({ mockIsTauri: vi.fn(() => true) }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => mockIsTauri() }));
const { mockIsMobile } = vi.hoisted(() => ({ mockIsMobile: vi.fn(() => false) }));
vi.mock("../lib/platform", () => ({ isMobile: () => mockIsMobile() }));

import {
  DEFAULT_ENGINE_URL,
  engineIsOnThisDevice,
  engineRunsElsewhere,
  engineUrlIsValid,
  getEngineUrl,
  isLoopbackUrl,
  normalizeEngineUrl,
  setLiveEngineUrl,
  useEngineStore,
} from "./engine";

describe("normalizeEngineUrl", () => {
  it("fills in what a bare address leaves out", () => {
    expect(normalizeEngineUrl("192.168.1.10")).toBe("http://192.168.1.10:19113");
    expect(normalizeEngineUrl(" 192.168.1.10:19113/ ")).toBe("http://192.168.1.10:19113");
    expect(normalizeEngineUrl("nas.local:8080")).toBe("http://nas.local:8080");
    expect(normalizeEngineUrl("nas.local")).toBe("http://nas.local:19113");
    expect(normalizeEngineUrl("nas.local:8080/ps5")).toBe("http://nas.local:8080/ps5");
    expect(normalizeEngineUrl("[fd00::1]")).toBe("http://[fd00::1]:19113");
    expect(normalizeEngineUrl("[fd00::1]:80")).toBe("http://[fd00::1]:80");
    expect(normalizeEngineUrl("fd00::1")).toBe("http://[fd00::1]:19113");
  });

  it("leaves a full URL as it is", () => {
    expect(normalizeEngineUrl("https://ps5.home/")).toBe("https://ps5.home");
    expect(normalizeEngineUrl("http://nas.local")).toBe("http://nas.local");
    expect(normalizeEngineUrl("")).toBe(DEFAULT_ENGINE_URL);
  });
});

describe("engineUrlIsValid", () => {
  it("takes addresses and refuses what cannot be fetched", () => {
    for (const u of ["192.168.1.10:19113", "nas.local", "https://ps5.home", ""]) {
      expect(engineUrlIsValid(u), u).toBe(true);
    }
    for (const u of ["ftp://nas.local", "file:///etc/hosts", "not a url", "http://"]) {
      expect(engineUrlIsValid(u), u).toBe(false);
    }
  });
});

describe("setEngineUrl", () => {
  it("does not drop the sidecar's fallback port when nothing changed", () => {
    useEngineStore.getState().setEngineUrl(DEFAULT_ENGINE_URL);
    setLiveEngineUrl("http://127.0.0.1:19114");
    // focus and blur of the Engine URL field
    useEngineStore.getState().setEngineUrl(`${DEFAULT_ENGINE_URL}/`);
    expect(getEngineUrl()).toBe("http://127.0.0.1:19114");
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19120");
    expect(getEngineUrl()).toBe("http://127.0.0.1:19120");
    useEngineStore.getState().setEngineUrl(DEFAULT_ENGINE_URL);
  });
});

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

describe("engineRunsElsewhere", () => {
  it("is true while this device's engine runs somewhere other than the setting says", () => {
    // F20: Settings showed the typed URL while the requests kept going to the running engine.
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19999");
    setLiveEngineUrl("http://127.0.0.1:19113");
    expect(engineRunsElsewhere(useEngineStore.getState())).toBe(true);
    expect(useEngineStore.getState().runningAt).toBe("http://127.0.0.1:19113");
    // This page's own requests go where the engine is, not where the field says.
    expect(getEngineUrl()).toBe("http://127.0.0.1:19113");
    // Restarted, it moved.
    setLiveEngineUrl("http://127.0.0.1:19999");
    expect(engineRunsElsewhere(useEngineStore.getState())).toBe(false);
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
    setLiveEngineUrl("http://127.0.0.1:19113");
  });

  it("is false for a chosen remote engine, whatever the local one does", () => {
    useEngineStore.getState().setEngineUrl("http://192.168.1.20:19113");
    setLiveEngineUrl("http://127.0.0.1:51234");
    expect(engineRunsElsewhere(useEngineStore.getState())).toBe(false);
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
    setLiveEngineUrl("http://127.0.0.1:19113");
  });
});

describe("a stored remote engine on a phone", () => {
  it("is not applied: the phone app uses the engine inside it", () => {
    mockIsTauri.mockReturnValue(true);
    mockIsMobile.mockReturnValue(true);
    try {
      useEngineStore.getState().setEngineUrl("http://192.168.1.50:19113");
      expect(useEngineStore.getState().engineUrl).toBe("http://127.0.0.1:19113");
      // The app's own engine on a fallback port is still fine.
      useEngineStore.getState().setEngineUrl("http://127.0.0.1:19123");
      expect(useEngineStore.getState().engineUrl).toBe("http://127.0.0.1:19123");
    } finally {
      mockIsMobile.mockReturnValue(false);
      useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
    }
  });

  it("stays the desktop's choice to make", () => {
    mockIsTauri.mockReturnValue(true);
    useEngineStore.getState().setEngineUrl("http://192.168.1.50:19113");
    expect(useEngineStore.getState().engineUrl).toBe("http://192.168.1.50:19113");
    useEngineStore.getState().setEngineUrl("http://127.0.0.1:19113");
  });
});
