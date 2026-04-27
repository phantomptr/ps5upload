import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  MOUNT_DEFAULT_SUBPATH,
  MOUNT_PRESETS,
  fallbackMountVolumes,
  loadMountDest,
  payloadSupportsMountPoint,
  resolveMountPath,
  saveMountDest,
} from "./mountDest";

const KEY = "ps5upload.mount.lastDest";

function installLocalStorageStub() {
  const store = new globalThis.Map<string, string>();
  const stub = {
    getItem: (k: string) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k: string, v: string) => {
      store.set(k, v);
    },
    removeItem: (k: string) => {
      store.delete(k);
    },
    clear: () => store.clear(),
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    get length() {
      return store.size;
    },
  };
  vi.stubGlobal("window", { localStorage: stub });
  return stub;
}

describe("mountDest persistence", () => {
  beforeEach(() => {
    installLocalStorageStub();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns null when no entry exists", () => {
    expect(loadMountDest("192.168.1.50")).toBeNull();
  });

  it("returns null for empty / whitespace host", () => {
    saveMountDest("192.168.1.50", { volume: "/mnt/ext1", subpath: "etaHEN/games" });
    expect(loadMountDest("")).toBeNull();
    expect(loadMountDest("   ")).toBeNull();
  });

  it("round-trips a saved destination", () => {
    saveMountDest("192.168.1.50", { volume: "/mnt/ext1", subpath: "etaHEN/games" });
    expect(loadMountDest("192.168.1.50")).toEqual({
      volume: "/mnt/ext1",
      subpath: "etaHEN/games",
    });
  });

  it("keeps separate destinations per host", () => {
    saveMountDest("192.168.1.50", { volume: "/mnt/ext1", subpath: "etaHEN/games" });
    saveMountDest("192.168.137.2", { volume: "/data", subpath: "ps5upload" });
    expect(loadMountDest("192.168.1.50")?.volume).toBe("/mnt/ext1");
    expect(loadMountDest("192.168.137.2")?.volume).toBe("/data");
  });

  it("ignores empty-volume saves so a mid-edit blank doesn't wipe state", () => {
    saveMountDest("192.168.1.50", { volume: "/mnt/ext1", subpath: "etaHEN/games" });
    saveMountDest("192.168.1.50", { volume: "", subpath: "anything" });
    expect(loadMountDest("192.168.1.50")?.volume).toBe("/mnt/ext1");
  });

  it("survives malformed JSON", () => {
    const stub = installLocalStorageStub();
    stub.setItem(KEY, "not-json");
    expect(loadMountDest("192.168.1.50")).toBeNull();
  });

  it("filters wrong-shape entries from a partially-corrupted map", () => {
    const stub = installLocalStorageStub();
    stub.setItem(
      KEY,
      JSON.stringify({
        good: { volume: "/data", subpath: "ps5upload" },
        bad1: "string-instead-of-object",
        bad2: { volume: 42, subpath: "x" },
        bad3: { volume: "/data" }, // missing subpath
      }),
    );
    expect(loadMountDest("good")).toEqual({ volume: "/data", subpath: "ps5upload" });
    expect(loadMountDest("bad1")).toBeNull();
    expect(loadMountDest("bad2")).toBeNull();
    expect(loadMountDest("bad3")).toBeNull();
  });
});

describe("payloadSupportsMountPoint", () => {
  it("rejects null", () => {
    expect(payloadSupportsMountPoint(null)).toBe(false);
  });
  it("rejects pre-2.2.25", () => {
    expect(payloadSupportsMountPoint("2.2.24")).toBe(false);
    expect(payloadSupportsMountPoint("2.2.16")).toBe(false);
    expect(payloadSupportsMountPoint("2.1.0")).toBe(false);
  });
  it("accepts 2.2.25 exactly and newer", () => {
    expect(payloadSupportsMountPoint("2.2.25")).toBe(true);
    expect(payloadSupportsMountPoint("2.2.26")).toBe(true);
    expect(payloadSupportsMountPoint("2.3.0")).toBe(true);
  });
  it("rejects unparseable version strings", () => {
    expect(payloadSupportsMountPoint("garbage")).toBe(false);
  });
});

describe("resolveMountPath", () => {
  it("composes /<volume>/<subpath>/<name>", () => {
    expect(resolveMountPath("/mnt/ext1", "etaHEN/games", "dead-space")).toBe(
      "/mnt/ext1/etaHEN/games/dead-space",
    );
  });
  it("handles empty subpath as direct-under-volume", () => {
    expect(resolveMountPath("/mnt/ext1", "", "dead-space")).toBe(
      "/mnt/ext1/dead-space",
    );
  });
  it("trims accidental leading / trailing slashes", () => {
    expect(
      resolveMountPath("/mnt/ext1/", "/etaHEN/games/", "/dead-space/"),
    ).toBe("/mnt/ext1/etaHEN/games/dead-space");
  });
});

describe("MOUNT_PRESETS + defaults", () => {
  it("includes the four upload-style presets", () => {
    const labels = MOUNT_PRESETS.map((p) => p.label);
    expect(labels).toEqual(["etaHEN/games", "homebrew", "exfat", "ps5upload"]);
  });
  it("default subpath is the legacy-equivalent", () => {
    expect(MOUNT_DEFAULT_SUBPATH).toBe("ps5upload");
  });
  it("fallback volumes are non-empty even when the live probe hasn't run", () => {
    expect(fallbackMountVolumes().length).toBeGreaterThan(0);
  });
});
