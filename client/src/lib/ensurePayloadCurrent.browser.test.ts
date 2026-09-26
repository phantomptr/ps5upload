import { describe, it, expect, vi, beforeEach } from "vitest";

// The self-hosted web UI. Everything Tauri-only must be routed to the engine
// instead of thrown, and the version check must not be the thing that kills
// the whole function before it starts.
const payloadCheckMock = vi.fn();
const bundledPathMock = vi.fn();
const sendPayloadMock = vi.fn();
const restoreMock = vi.fn();
const prearmMock = vi.fn();
const appVersionMock = vi.fn();

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("./appVersion", () => ({ getAppVersion: () => appVersionMock() }));
vi.mock("../api/ps5", () => ({
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
  bundledPayloadPath: (...a: unknown[]) => bundledPathMock(...a),
  sendPayload: (...a: unknown[]) => sendPayloadMock(...a),
}));
vi.mock("./restoreMainPayload", () => ({
  restoreMainPayload: (...a: unknown[]) => restoreMock(...a),
}));
vi.mock("./prearmDpi", () => ({
  prearmDpiDaemon: (...a: unknown[]) => {
    prearmMock(...a);
    return Promise.resolve({ outcome: "already-up" });
  },
}));
vi.mock("../state/logs", () => ({
  log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

import { ensurePayloadCurrent } from "./ensurePayloadCurrent";

beforeEach(() => {
  [payloadCheckMock, bundledPathMock, sendPayloadMock, restoreMock, prearmMock, appVersionMock]
    .forEach((m) => m.mockReset());
  appVersionMock.mockResolvedValue("5.17.7");
});

describe("ensurePayloadCurrent in a browser", () => {
  it("reads the version from the engine instead of throwing", async () => {
    // Tauri's getVersion() needs __TAURI_INTERNALS__ and throws in a browser.
    // That made this whole function return "no-push" on the first line, so
    // the web UI never checked or redeployed a helper at all.
    payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion: "5.17.7" });
    await expect(ensurePayloadCurrent("10.0.0.5")).resolves.toBe("current");
    expect(appVersionMock).toHaveBeenCalled();
  });

  it("arms the update installer once the console has answered", async () => {
    payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion: "5.17.7" });
    await ensurePayloadCurrent("10.0.0.5");
    expect(prearmMock).toHaveBeenCalledWith("10.0.0.5");
  });

  it("redeploys through the engine, never through desktop-only commands", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: true, payloadVersion: "5.16.0" })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.17.7" });
    const r = await ensurePayloadCurrent("10.0.0.5");
    expect(r).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledWith("10.0.0.5");
    expect(bundledPathMock).not.toHaveBeenCalled();
    expect(sendPayloadMock).not.toHaveBeenCalled();
  });
});
