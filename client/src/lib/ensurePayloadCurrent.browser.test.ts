import { describe, it, expect, vi, beforeEach } from "vitest";

// The self-hosted web UI. Everything Tauri-only must be routed to the engine
// instead of thrown, and the version check must not be the thing that kills
// the whole function before it starts.
const payloadCheckMock = vi.fn();
const bundledPathMock = vi.fn();
const sendPayloadMock = vi.fn();
const restoreMock = vi.fn();
const appVersionMock = vi.fn();

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("./appVersion", () => ({ getAppVersion: () => appVersionMock() }));
vi.mock("../api/ps5", () => ({
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
  bundledPayloadPath: (...a: unknown[]) => bundledPathMock(...a),
  sendPayload: (...a: unknown[]) => sendPayloadMock(...a),
}));
// The loader answers and the elfldr needs nothing (see ensurePayloadCurrent.elfldr.test.ts).
vi.mock("./elfldrGuard", () => ({
  waitForLoader: async () => "healthy",
  guardElfldr: async () => {},
  STUCK_LOADER_MESSAGE: "",
  LISTENER_LOADER_HINT: "",
}));
vi.mock("./restoreMainPayload", () => ({
  engineSendsHelper: (...a: unknown[]) => restoreMock(...a),
}));
vi.mock("../state/logs", () => ({
  log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

import { ensurePayloadCurrent } from "./ensurePayloadCurrent";
import { resetHelperSendGate, sentAgoMs } from "./helperSendGate";

beforeEach(() => {
  [payloadCheckMock, bundledPathMock, sendPayloadMock, restoreMock, appVersionMock]
    .forEach((m) => m.mockReset());
  appVersionMock.mockResolvedValue("5.17.7");
  resetHelperSendGate();
});

/** Runs `run` on a fake clock, so the helper's boot wait (1.5 s, then 1 s polls) passes at once. */
async function onFakeClock<T>(run: () => Promise<T>): Promise<T> {
  vi.useFakeTimers();
  try {
    const p = run();
    await vi.advanceTimersByTimeAsync(1_600);
    return await p;
  } finally {
    vi.useRealTimers();
  }
}

describe("ensurePayloadCurrent in a browser", () => {
  it("reads the version from the engine instead of throwing", async () => {
    // Tauri's getVersion() needs __TAURI_INTERNALS__ and throws in a browser.
    // That made this whole function return "no-push" on the first line, so
    // the web UI never checked or redeployed a helper at all.
    payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion: "5.17.7" });
    await expect(ensurePayloadCurrent("10.0.0.5")).resolves.toBe("current");
    expect(appVersionMock).toHaveBeenCalled();
  });

  it("redeploys through the engine, never through desktop-only commands", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: true, payloadVersion: "5.16.0" })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.17.7" });
    const r = await onFakeClock(() => ensurePayloadCurrent("10.0.0.5"));
    expect(r).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledWith("10.0.0.5");
    expect(bundledPathMock).not.toHaveBeenCalled();
    expect(sendPayloadMock).not.toHaveBeenCalled();
  });

  it("a send the engine refused is no send: no-push at once, and nothing stamped", async () => {
    // Before: the restore swallowed {ok:false}, the gate stamped a send that never
    // happened, the 30 s boot poll ran for nothing, and every other caller was held
    // to "no-push" for the cooldown.
    payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion: "5.16.0" });
    restoreMock.mockRejectedValue(new Error("the engine has no helper to send"));
    await expect(ensurePayloadCurrent("10.0.0.5")).resolves.toBe("no-push");
    expect(restoreMock).toHaveBeenCalledTimes(1);
    expect(sentAgoMs("10.0.0.5")).toBeNull();
  });
});
