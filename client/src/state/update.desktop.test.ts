import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import type { UpdateCheck, UpdateDownload } from "../api/ps5";

// Same in-memory storage as update.channel.test.ts: the node environment has none.
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

vi.mock("../api/ps5", () => ({
  updateCheck: vi.fn(),
  updateDownload: vi.fn(),
  updateInstall: vi.fn(),
  updateInstallOnQuit: vi.fn(async () => true),
  updateLastInstall: vi.fn(async () => null),
  updateForgetFailure: vi.fn(async () => {}),
  updateOldDownloads: vi.fn(async () => ({ count: 0, bytes: 0 })),
  updateRemoveOldDownloads: vi.fn(async () => 0),
}));

vi.mock("./notifications", () => ({ pushNotification: vi.fn() }));

const result = (latest: string): UpdateCheck => ({
  available: true,
  current_version: "10.1.0",
  latest_version: latest,
  notes: "",
  pub_date: "",
  download_url: `https://github.com/x/PS5Upload-${latest}-linux-x64.deb`,
  download_filename: `PS5Upload-${latest}-linux-x64.deb`,
  sha256: "a".repeat(64),
});

const download = (extra: Partial<UpdateDownload> = {}): UpdateDownload => ({
  path: "/home/me/Downloads/PS5Upload-10.2.0-linux-x64.deb",
  bytes: 1,
  can_install: true,
  install_command: "sudo apt install '/home/me/Downloads/PS5Upload-10.2.0-linux-x64.deb'",
  ...extra,
});

describe("desktop updates", () => {
  beforeEach(() => {
    installStorage();
    vi.resetModules();
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("passes the manifest's checksum to the download", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateDownload).mockResolvedValue(download());
    const { useUpdateStore } = await import("./update");
    useUpdateStore.setState({ phase: { kind: "available", result: result("10.2.0") } });
    await useUpdateStore.getState().download();
    expect(api.updateDownload).toHaveBeenCalledWith(
      "https://github.com/x/PS5Upload-10.2.0-linux-x64.deb",
      "PS5Upload-10.2.0-linux-x64.deb",
      "a".repeat(64),
    );
  });

  it("a skipped version is no longer offered, and is remembered", async () => {
    const { useUpdateStore, updateOffered } = await import("./update");
    useUpdateStore.setState({ phase: { kind: "available", result: result("10.2.0") } });
    expect(updateOffered(useUpdateStore.getState())).toBe(true);
    useUpdateStore.getState().skipVersion();
    expect(updateOffered(useUpdateStore.getState())).toBe(false);
    expect(localStorage.getItem("ps5upload.update.skipped-version")).toBe("10.2.0");
    // A newer release is offered again.
    useUpdateStore.setState({ phase: { kind: "available", result: result("10.3.0") } });
    expect(updateOffered(useUpdateStore.getState())).toBe(true);
    useUpdateStore.getState().unskipVersion();
    expect(localStorage.getItem("ps5upload.update.skipped-version")).toBeNull();
  });

  it("install on quit is on by default and arms only an idle, passwordless, unskipped download", async () => {
    const { useUpdateStore, installOnQuitTarget } = await import("./update");
    const s = useUpdateStore.getState();
    expect(s.installOnQuitEnabled).toBe(true);
    const downloaded = {
      ...s,
      phase: { kind: "downloaded" as const, result: result("10.2.0"), download: download() },
    };
    expect(installOnQuitTarget(downloaded, true)).toBe(download().path);
    // A transfer is running.
    expect(installOnQuitTarget(downloaded, false)).toBeNull();
    // A .deb / .rpm asks for a password: never on quit.
    expect(
      installOnQuitTarget(
        { ...downloaded, phase: { ...downloaded.phase, download: download({ needs_password: true }) } },
        true,
      ),
    ).toBeNull();
    expect(installOnQuitTarget({ ...downloaded, skippedVersion: "10.2.0" }, true)).toBeNull();
    expect(installOnQuitTarget({ ...downloaded, installOnQuitEnabled: false }, true)).toBeNull();
    expect(installOnQuitTarget({ ...downloaded, installing: true }, true)).toBeNull();
  });

  it("a closed password prompt falls back to the command to copy", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateInstall).mockRejectedValue("pkexec-cancelled");
    const { useUpdateStore } = await import("./update");
    useUpdateStore.setState({
      phase: { kind: "downloaded", result: result("10.2.0"), download: download({ needs_password: true }) },
    });
    await useUpdateStore.getState().install();
    const s = useUpdateStore.getState();
    expect(s.installing).toBe(false);
    expect(s.installFallback).toBe("pkexec-cancelled");
    expect(s.phase.kind).toBe("downloaded");
    if (s.phase.kind === "downloaded") {
      expect(s.phase.download.can_install).toBe(false);
      expect(s.phase.download.install_command).toContain("sudo apt install");
    }
  });

  it("a failed install keeps the download, is not tried again by itself, and Try again retries it", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateInstall).mockRejectedValueOnce("could not move C:\\Apps\\PS5Upload.exe aside");
    const { useUpdateStore, installOnQuitTarget, failedInstall } = await import("./update");
    const zip = download({
      path: "C:\\u\\updates\\PS5Upload-10.2.0-win-x64.zip",
      install_command: undefined,
    });
    useUpdateStore.setState({ phase: { kind: "downloaded", result: result("10.2.0"), download: zip } });
    await useUpdateStore.getState().install();
    expect(api.updateInstall).toHaveBeenCalledWith(zip.path, "10.2.0");
    let s = useUpdateStore.getState();
    // The app stayed open on the old version: the download is still there to retry.
    expect(s.installing).toBe(false);
    expect(s.phase.kind).toBe("downloaded");
    expect(failedInstall(s, "10.2.0")?.message).toContain("aside");
    // Not on quit either, until the person tries again.
    expect(installOnQuitTarget(s, true)).toBeNull();
    // A newer release is a new attempt.
    expect(failedInstall(s, "10.3.0")).toBeNull();

    vi.mocked(api.updateInstall).mockResolvedValueOnce(undefined);
    await useUpdateStore.getState().retryInstall();
    expect(api.updateForgetFailure).toHaveBeenCalled();
    expect(api.updateInstall).toHaveBeenCalledTimes(2);
    s = useUpdateStore.getState();
    expect(s.lastInstall).toBeNull();
  });

  it("a failed install found at launch is said once and stops the automatic install", async () => {
    const api = await import("../api/ps5");
    const notes = await import("./notifications");
    vi.mocked(api.updateLastInstall).mockResolvedValue({
      version: "10.2.0",
      installed: false,
      message: "could not open the .dmg",
      new: true,
    });
    vi.mocked(api.updateOldDownloads).mockResolvedValue({ count: 3, bytes: 300 });
    localStorage.setItem("ps5upload.update.autoinstall", "1");
    const { useUpdateStore } = await import("./update");
    await useUpdateStore.getState().loadInstallStatus();
    expect(useUpdateStore.getState().oldDownloads).toBe(3);
    expect(notes.pushNotification).toHaveBeenCalledWith(
      "warning",
      expect.any(String),
      expect.objectContaining({ link: "/settings" }),
    );
    // The launch check finds 10.2.0 again: no download, no install, no quit.
    vi.mocked(api.updateCheck).mockResolvedValue(result("10.2.0"));
    await useUpdateStore.getState().checkNow();
    await new Promise((r) => setTimeout(r, 0));
    expect(api.updateDownload).not.toHaveBeenCalled();
    expect(api.updateInstall).not.toHaveBeenCalled();
    expect(useUpdateStore.getState().phase.kind).toBe("available");
  });

  it("removing old downloads asks the app and counts again", async () => {
    const api = await import("../api/ps5");
    vi.mocked(api.updateRemoveOldDownloads).mockResolvedValue(2);
    vi.mocked(api.updateOldDownloads).mockResolvedValue({ count: 0, bytes: 0 });
    const { useUpdateStore } = await import("./update");
    useUpdateStore.setState({ oldDownloads: 2 });
    await useUpdateStore.getState().removeOldDownloads();
    expect(api.updateRemoveOldDownloads).toHaveBeenCalled();
    expect(useUpdateStore.getState().oldDownloads).toBe(0);
  });
});
