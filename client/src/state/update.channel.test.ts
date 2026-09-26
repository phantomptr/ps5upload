import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// These tests run in the node environment, which has no DOM storage, so the
// store's `typeof localStorage === "undefined"` guards would short-circuit and
// every case would trivially read "stable". Install a minimal in-memory
// storage so the persistence path is actually exercised.
function installStorage() {
  const make = () => {
    const map = new Map<string, string>();
    return {
      getItem: (k: string) => (map.has(k) ? (map.get(k) as string) : null),
      setItem: (k: string, v: string) => void map.set(k, String(v)),
      removeItem: (k: string) => void map.delete(k),
      clear: () => map.clear(),
      key: (i: number) => Array.from(map.keys())[i] ?? null,
      get length() {
        return map.size;
      },
    } as unknown as Storage;
  };
  const local = make();
  const session = make();
  // Both spellings matter: the store guards on bare `localStorage`, while
  // lib/safeStorage goes through `window.localStorage`. Stub only one and the
  // reads work while the writes silently vanish. The listener no-ops are
  // needed because the store subscribes to window events on import.
  vi.stubGlobal("localStorage", local);
  vi.stubGlobal("sessionStorage", session);
  vi.stubGlobal("window", {
    localStorage: local,
    sessionStorage: session,
    addEventListener: () => {},
    removeEventListener: () => {},
  });
}

vi.mock("../api/ps5", () => ({
  updateCheck: vi.fn(async () => ({
    available: false,
    current_version: "1.0.0",
    latest_version: "1.0.0",
    notes: "",
    pub_date: "",
    download_url: "",
    download_filename: "",
  })),
  updateDownload: vi.fn(),
}));

const CHANNEL_KEY = "ps5upload.update.channel";

describe("update channel preference", () => {
  beforeEach(() => {
    installStorage();
    vi.resetModules();
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("defaults to stable when nothing is stored", async () => {
    // The pre-release channel must be opt-in: defaulting the other way would
    // offer builds nobody has run on hardware to every user.
    const { useUpdateStore } = await import("./update");
    expect(useUpdateStore.getState().channel).toBe("stable");
  });

  it("treats an unrecognised stored value as stable", async () => {
    // A corrupt value, or one written by a future build, must fail safe.
    localStorage.setItem(CHANNEL_KEY, "banana");
    const { useUpdateStore } = await import("./update");
    expect(useUpdateStore.getState().channel).toBe("stable");
  });

  it("reads a stored opt-in back", async () => {
    localStorage.setItem(CHANNEL_KEY, "prerelease");
    const { useUpdateStore } = await import("./update");
    expect(useUpdateStore.getState().channel).toBe("prerelease");
  });

  it("persists a change so it survives a restart", async () => {
    const { useUpdateStore } = await import("./update");
    useUpdateStore.getState().setChannel("prerelease");
    expect(localStorage.getItem(CHANNEL_KEY)).toBe("prerelease");
    useUpdateStore.getState().setChannel("stable");
    expect(localStorage.getItem(CHANNEL_KEY)).toBe("stable");
  });

  it("asks the updater for the channel the user chose", async () => {
    const api = await import("../api/ps5");
    const { useUpdateStore } = await import("./update");
    useUpdateStore.getState().setChannel("prerelease");
    await useUpdateStore.getState().checkNow();
    expect(api.updateCheck).toHaveBeenCalledWith("prerelease");
  });
});
