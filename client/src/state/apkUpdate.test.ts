import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import type { UpdateCheck } from "../api/ps5";

const updateDownloadApk = vi.fn();
vi.mock("../api/ps5", () => ({ updateDownloadApk: (...a: unknown[]) => updateDownloadApk(...a) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../lib/openExternalUrl", () => ({ openExternalUrl: vi.fn(async () => true) }));

import {
  apkUpdaterAvailable,
  downloadAndInstallApk,
  installRefusalMessage,
  parseInstallStatus,
  progressPercent,
  releasePageFor,
  useApkUpdateStore,
  type UpdaterBridge,
} from "./apkUpdate";

const RESULT: UpdateCheck = {
  available: true,
  current_version: "10.1.0",
  latest_version: "10.2.0",
  notes: "",
  pub_date: "",
  download_url: "https://github.com/phantomptr/ps5upload/releases/download/v10.2.0/PS5Upload-android.apk",
  download_filename: "PS5Upload-android.apk",
  download_sha256: "ab".repeat(32),
};

function fakeBridge(over: Partial<UpdaterBridge> & { states?: string[] } = {}) {
  const states = over.states ?? ["writing", "confirm"];
  let i = 0;
  const bridge: UpdaterBridge = {
    canInstall: over.canInstall ?? (() => true),
    openInstallSettings: over.openInstallSettings ?? vi.fn(),
    install: over.install ?? vi.fn(() => ""),
    status:
      over.status ??
      (() => JSON.stringify({ state: states[Math.min(i++, states.length - 1)], message: "" })),
  };
  vi.stubGlobal("window", { PS5UploadUpdater: bridge });
  return bridge;
}

describe("apk update helpers", () => {
  it("reads the installer status, treating junk as a failure", () => {
    expect(parseInstallStatus('{"state":"confirm","message":""}')).toEqual({
      state: "confirm",
      message: "",
    });
    expect(parseInstallStatus('{"state":"failed","message":"INSTALL_FAILED_X"}').message).toBe(
      "INSTALL_FAILED_X",
    );
    expect(parseInstallStatus('{"state":"weird"}').state).toBe("failed");
    expect(parseInstallStatus("not json").state).toBe("failed");
  });

  it("gives a percent only when the size is known", () => {
    expect(progressPercent(0, 0)).toBeNull();
    expect(progressPercent(50, 200)).toBe(25);
    expect(progressPercent(300, 200)).toBe(100);
    expect(progressPercent(199, 200)).toBe(99);
  });

  it("falls back to the release page of the offered version", () => {
    expect(releasePageFor("10.2.0")).toBe(
      "https://github.com/phantomptr/ps5upload/releases/tag/v10.2.0",
    );
    expect(releasePageFor("v10.2.0")).toBe(
      "https://github.com/phantomptr/ps5upload/releases/tag/v10.2.0",
    );
    expect(releasePageFor("")).toBe("https://github.com/phantomptr/ps5upload/releases/latest");
  });

  it("names a signing-key mismatch in plain words", () => {
    expect(installRefusalMessage("signed-differently")).toMatch(/different key/);
    expect(installRefusalMessage("boom")).toBe("boom");
  });
});

describe("downloadAndInstallApk", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    updateDownloadApk.mockReset();
    useApkUpdateStore.setState({ step: { kind: "idle" } });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("is unavailable without the Android bridge", async () => {
    vi.stubGlobal("window", {});
    expect(apkUpdaterAvailable()).toBe(false);
    expect(await downloadAndInstallApk(RESULT)).toMatch(/cannot install/);
    expect(updateDownloadApk).not.toHaveBeenCalled();
  });

  it("downloads with the release hash, then waits for Android's dialog", async () => {
    const bridge = fakeBridge();
    updateDownloadApk.mockResolvedValue({ path: "/data/c/updates/x.apk", bytes: 1, verified: true });
    const p = downloadAndInstallApk(RESULT);
    await vi.advanceTimersByTimeAsync(2000);
    expect(await p).toBeNull();
    expect(updateDownloadApk).toHaveBeenCalledWith(
      RESULT.download_url,
      RESULT.download_filename,
      RESULT.download_sha256,
    );
    expect(bridge.install).toHaveBeenCalledWith("/data/c/updates/x.apk");
    expect(useApkUpdateStore.getState().step.kind).toBe("installing");
  });

  it("opens Install unknown apps when it is off", async () => {
    const bridge = fakeBridge({ install: vi.fn(() => "needs-permission") });
    updateDownloadApk.mockResolvedValue({ path: "/p.apk", bytes: 1, verified: false });
    expect(await downloadAndInstallApk(RESULT)).toBeNull();
    expect(bridge.openInstallSettings).toHaveBeenCalled();
    expect(useApkUpdateStore.getState().step.kind).toBe("needs-permission");
  });

  it("reports a download or checksum failure", async () => {
    fakeBridge();
    updateDownloadApk.mockRejectedValue("the download does not match the release checksum");
    expect(await downloadAndInstallApk(RESULT)).toMatch(/checksum/);
    expect(useApkUpdateStore.getState().step.kind).toBe("failed");
  });

  it("reports a refused APK (different signing key)", async () => {
    fakeBridge({ install: vi.fn(() => "signed-differently") });
    updateDownloadApk.mockResolvedValue({ path: "/p.apk", bytes: 1, verified: true });
    expect(await downloadAndInstallApk(RESULT)).toMatch(/different key/);
  });

  it("reports an install that fails before the dialog", async () => {
    fakeBridge({ states: ["writing", "failed"] });
    updateDownloadApk.mockResolvedValue({ path: "/p.apk", bytes: 1, verified: true });
    const p = downloadAndInstallApk(RESULT);
    await vi.advanceTimersByTimeAsync(2000);
    expect(await p).toBe("install failed");
  });

  it("goes back to idle when the dialog is cancelled", async () => {
    fakeBridge({ states: ["confirm", "confirm", "cancelled"] });
    updateDownloadApk.mockResolvedValue({ path: "/p.apk", bytes: 1, verified: true });
    expect(await downloadAndInstallApk(RESULT)).toBeNull();
    expect(useApkUpdateStore.getState().step.kind).toBe("installing");
    await vi.advanceTimersByTimeAsync(3000);
    expect(useApkUpdateStore.getState().step.kind).toBe("idle");
  });
});
