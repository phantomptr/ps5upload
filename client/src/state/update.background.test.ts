import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import type { UpdateCheck, UpdateDownload } from "../api/ps5";

// The desktop app: background download, the once-a-day notice and the tray hint.

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
  vi.stubGlobal("localStorage", local);
  vi.stubGlobal("sessionStorage", session);
  vi.stubGlobal("window", {
    localStorage: local,
    sessionStorage: session,
    addEventListener: () => {},
    removeEventListener: () => {},
  });
}

const listeners = new Map<string, () => void>();
const busy = { queue: false };

vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: () => void) => {
    listeners.set(name, cb);
    return () => {};
  }),
}));
vi.mock("./uploadQueue", () => ({
  useUploadQueueStore: {
    getState: () => ({ running: busy.queue }),
    subscribe: () => () => {},
  },
}));
vi.mock("./transfer", () => ({
  useTransferStore: {
    getState: () => ({ phasesByHost: {} }),
    subscribe: () => () => {},
  },
}));
vi.mock("../api/ps5", () => ({
  updateCheck: vi.fn(),
  updateDownload: vi.fn(),
  updateInstall: vi.fn(),
  updateInstallOnQuit: vi.fn(async () => true),
  updateLastInstall: vi.fn(async () => null),
  updateForgetFailure: vi.fn(async () => {}),
  updateOldDownloads: vi.fn(async () => ({ count: 0, bytes: 0 })),
  updateRemoveOldDownloads: vi.fn(async () => 0),
  updateCanSelfInstall: vi.fn(async () => true),
}));
vi.mock("./notifications", () => ({
  pushNotification: vi.fn(),
  useNotificationsStore: { getState: () => ({ osNotifyEnabled: true }) },
}));
vi.mock("../lib/osNotify", () => ({
  appIsForeground: () => false,
  sendOsNotification: vi.fn(async () => {}),
}));

const result = (latest: string): UpdateCheck => ({
  available: true,
  current_version: "10.7.1",
  latest_version: latest,
  notes: "",
  pub_date: "",
  download_url: `https://github.com/x/PS5Upload-${latest}-win-x64.zip`,
  download_filename: `PS5Upload-${latest}-win-x64.zip`,
  sha256: "a".repeat(64),
});

const download = (
  latest: string,
  extra: Partial<UpdateDownload> = {},
): UpdateDownload => ({
  path: `C:\\u\\updates\\PS5Upload-${latest}-win-x64.zip`,
  bytes: 1,
  can_install: true,
  needs_password: false,
  ...extra,
});

/** Let the background download's awaits run (vitest's lazy imports take a while). */
const settle = () => new Promise((r) => setTimeout(r, 300));
const eventually = (check: () => void | Promise<void>) =>
  vi.waitFor(check, { timeout: 5000, interval: 20 });

const titles = async () => {
  const notes = await import("./notifications");
  return vi.mocked(notes.pushNotification).mock.calls.map((c) => c[1]);
};

describe("once-a-day update notice", () => {
  it("never twice for a version, at most once a day for any", async () => {
    const { shouldNotifyUpdate } = await import("./update");
    const day = 24 * 60 * 60 * 1000;
    const now = 10 * day;
    expect(shouldNotifyUpdate("10.7.2", null, null, now)).toBe(true);
    expect(shouldNotifyUpdate("10.7.2", "10.7.2", now - 3 * day, now)).toBe(
      false,
    );
    expect(
      shouldNotifyUpdate("10.7.3", "10.7.2", now - 2 * 60 * 60 * 1000, now),
    ).toBe(false);
    expect(shouldNotifyUpdate("10.7.3", "10.7.2", now - day, now)).toBe(true);
    // Written by a build without the timestamp: only the version counts.
    expect(shouldNotifyUpdate("10.7.3", "10.7.2", null, now)).toBe(true);
    // A clock set back does not hold the notice off for good.
    expect(shouldNotifyUpdate("10.7.3", "10.7.2", now + day, now)).toBe(true);
  });
});

describe("desktop background download", () => {
  beforeEach(async () => {
    installStorage();
    vi.resetModules();
    vi.clearAllMocks();
    listeners.clear();
    busy.queue = false;
    // Loaded up front: the idle check imports them lazily, and two lazy imports of one mocked
    // module at once never settle under vitest.
    await import("./uploadQueue");
    await import("./transfer");
    // The mocks outlive vi.resetModules: put back what a test changed.
    const api = await import("../api/ps5");
    vi.mocked(api.updateCanSelfInstall).mockResolvedValue(true);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("is wanted only with auto-check and install on quit on and nothing against it", async () => {
    const { backgroundDownloadWanted } = await import("./update");
    const base = {
      phase: { kind: "available" as const, result: result("10.7.3") },
      skippedVersion: null,
      lastInstall: null,
      autoCheckEnabled: true,
      installOnQuitEnabled: true,
      autoInstallEnabled: false,
    };
    expect(backgroundDownloadWanted(base)).toBe(true);
    expect(backgroundDownloadWanted({ ...base, autoCheckEnabled: false })).toBe(
      false,
    );
    expect(
      backgroundDownloadWanted({ ...base, installOnQuitEnabled: false }),
    ).toBe(false);
    // The automatic install downloads it itself.
    expect(
      backgroundDownloadWanted({ ...base, autoInstallEnabled: true }),
    ).toBe(false);
    expect(
      backgroundDownloadWanted({ ...base, skippedVersion: "10.7.3" }),
    ).toBe(false);
    expect(
      backgroundDownloadWanted({
        ...base,
        lastInstall: {
          version: "10.7.3",
          installed: false,
          message: "",
          new: false,
        },
      }),
    ).toBe(false);
    expect(
      backgroundDownloadWanted({
        ...base,
        phase: {
          kind: "available",
          result: { ...result("10.7.3"), download_url: "" },
        },
      }),
    ).toBe(false);
    expect(backgroundDownloadWanted({ ...base, phase: { kind: "idle" } })).toBe(
      false,
    );
  });

  it("downloads an update this copy installs quietly and says it is ready", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.3"));
    vi.mocked(api.updateDownload).mockResolvedValue(download("10.7.3"));
    const { useUpdateStore } = await import("./update");
    await useUpdateStore.getState().checkNow();
    await eventually(async () =>
      expect(await titles()).toEqual(["v10.7.3 is ready"]),
    );
    expect(api.updateCanSelfInstall).toHaveBeenCalledWith(
      "PS5Upload-10.7.3-win-x64.zip",
    );
    expect(api.updateDownload).toHaveBeenCalledTimes(1);
    expect(useUpdateStore.getState().phase.kind).toBe("downloaded");
    // Armed for the quit.
    await eventually(() =>
      expect(api.updateInstallOnQuit).toHaveBeenCalledWith(
        download("10.7.3").path,
        "10.7.3",
      ),
    );
  });

  it("leaves an update that needs a password (or by hand) to the person", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.3"));
    vi.mocked(api.updateCanSelfInstall).mockResolvedValue(false);
    const { useUpdateStore } = await import("./update");
    await useUpdateStore.getState().checkNow();
    await settle();
    expect(api.updateDownload).not.toHaveBeenCalled();
    expect(useUpdateStore.getState().phase.kind).toBe("available");
    expect(await titles()).toEqual(["Update available — v10.7.3"]);
  });

  it("not with install on quit off", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.3"));
    localStorage.setItem("ps5upload.update.install-on-quit", "0");
    const { useUpdateStore } = await import("./update");
    await useUpdateStore.getState().checkNow();
    await settle();
    expect(api.updateCanSelfInstall).not.toHaveBeenCalled();
    expect(api.updateDownload).not.toHaveBeenCalled();
  });

  it("waits while something transfers", async () => {
    busy.queue = true;
    const api = await import("../api/ps5");
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.3"));
    vi.mocked(api.updateDownload).mockResolvedValue(download("10.7.3"));
    const { useUpdateStore } = await import("./update");
    await useUpdateStore.getState().checkNow();
    await eventually(() => expect(api.updateCanSelfInstall).toHaveBeenCalled());
    await settle();
    // Asked, then waiting for the queue (it looks again every minute): nothing downloaded,
    // nothing said yet.
    expect(api.updateDownload).not.toHaveBeenCalled();
    expect(useUpdateStore.getState().phase.kind).toBe("available");
    expect(await titles()).toEqual([]);
  });

  it("a failed background download goes back to plain 'available'", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.3"));
    vi.mocked(api.updateDownload).mockRejectedValue(new Error("network down"));
    const { useUpdateStore } = await import("./update");
    await useUpdateStore.getState().checkNow();
    await eventually(async () =>
      expect(await titles()).toEqual(["Update available — v10.7.3"]),
    );
    expect(useUpdateStore.getState().phase.kind).toBe("available");
  });

  it("a newer release the same day replaces the download without a second notice", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.2"));
    vi.mocked(api.updateDownload).mockResolvedValue(download("10.7.2"));
    const { useUpdateStore } = await import("./update");
    const downloadedVersion = () => {
      const p = useUpdateStore.getState().phase;
      return p.kind === "downloaded" ? p.result.latest_version : null;
    };
    await useUpdateStore.getState().checkNow();
    await eventually(async () =>
      expect(await titles()).toEqual(["v10.7.2 is ready"]),
    );
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.3"));
    vi.mocked(api.updateDownload).mockResolvedValue(download("10.7.3"));
    await useUpdateStore.getState().checkNow();
    await eventually(() => expect(downloadedVersion()).toBe("10.7.3"));
    await settle();
    expect(await titles()).toEqual(["v10.7.2 is ready"]);
    // A day later, the next one is said again.
    localStorage.setItem(
      "ps5upload.update.notified-at",
      String(Date.now() - 25 * 60 * 60 * 1000),
    );
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.7.4"));
    vi.mocked(api.updateDownload).mockResolvedValue(download("10.7.4"));
    await useUpdateStore.getState().checkNow();
    await eventually(async () =>
      expect(await titles()).toEqual(["v10.7.2 is ready", "v10.7.4 is ready"]),
    );
  });
});

describe("closed to the tray", () => {
  beforeEach(() => {
    installStorage();
    vi.resetModules();
    vi.clearAllMocks();
    listeners.clear();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("hints once per version that an armed update waits for Quit", async () => {
    const { trayHintVersion, useUpdateStore } = await import("./update");
    const armed = {
      phase: {
        kind: "downloaded" as const,
        result: result("10.7.3"),
        download: download("10.7.3"),
      },
      installOnQuitArmed: true,
    };
    expect(trayHintVersion(armed, null)).toBe("10.7.3");
    expect(trayHintVersion(armed, "10.7.3")).toBeNull();
    expect(
      trayHintVersion({ ...armed, installOnQuitArmed: false }, null),
    ).toBeNull();
    expect(
      trayHintVersion(
        { phase: { kind: "idle" }, installOnQuitArmed: true },
        null,
      ),
    ).toBeNull();

    vi.useFakeTimers();
    await Promise.resolve();
    const closed = listeners.get("tray-closed");
    expect(closed).toBeDefined();
    useUpdateStore.setState(armed);
    closed!();
    await vi.advanceTimersByTimeAsync(600);
    closed!();
    await vi.advanceTimersByTimeAsync(600);
    expect(await titles()).toEqual(["v10.7.3 installs when you quit"]);
    // Nothing armed: nothing said.
    useUpdateStore.setState({
      ...armed,
      installOnQuitArmed: false,
      phase: { ...armed.phase, result: result("10.7.4") },
    });
    closed!();
    await vi.advanceTimersByTimeAsync(600);
    expect(await titles()).toHaveLength(1);
  });
});
