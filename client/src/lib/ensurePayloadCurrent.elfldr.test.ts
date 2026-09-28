import { beforeEach, describe, expect, it, vi } from "vitest";

// Sending the helper into a stuck elfldr only adds another connection it will never answer.
const payloadCheckMock = vi.fn();
const restoreMock = vi.fn();
const waitMock = vi.fn();
const guardMock = vi.fn();
const errorLog = vi.fn();

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("./appVersion", () => ({ getAppVersion: async () => "5.37.1" }));
vi.mock("../api/ps5", () => ({
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
  bundledPayloadPath: vi.fn(),
  sendPayload: vi.fn(),
}));
vi.mock("./restoreMainPayload", () => ({ restoreMainPayload: (...a: unknown[]) => restoreMock(...a) }));
vi.mock("./elfldrGuard", () => ({
  waitForLoader: (...a: unknown[]) => waitMock(...a),
  guardElfldr: (...a: unknown[]) => guardMock(...a),
  STUCK_LOADER_MESSAGE: "elfldr is stuck",
}));
vi.mock("../state/logs", () => ({ log: { info: vi.fn(), warn: vi.fn(), error: (...a: unknown[]) => errorLog(...a) } }));

import { ensurePayloadCurrent, resetEnsurePayloadState } from "./ensurePayloadCurrent";

beforeEach(() => {
  [payloadCheckMock, restoreMock, waitMock, guardMock, errorLog].forEach((m) => m.mockReset());
  guardMock.mockResolvedValue(undefined);
  resetEnsurePayloadState();
});

describe("the helper and the console's elfldr", () => {
  it("does not send into a loader that stays stuck, and says why", async () => {
    payloadCheckMock.mockResolvedValue({ reachable: false });
    waitMock.mockResolvedValue("stuck");
    expect(await ensurePayloadCurrent("10.0.0.5", undefined, true)).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
    expect(errorLog).toHaveBeenCalledWith("payload", expect.stringContaining("elfldr is stuck"));
  });

  it("sends once the loader answers, then has the patched elfldr put in place at once", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    waitMock.mockResolvedValue("healthy");
    expect(await ensurePayloadCurrent("10.0.0.5", undefined, true)).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledTimes(1);
    expect(guardMock).toHaveBeenCalledWith("10.0.0.5", true);
  });

  it("checks the elfldr on a helper that is already current, without forcing it", async () => {
    payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    expect(await ensurePayloadCurrent("10.0.0.5")).toBe("current");
    expect(guardMock).toHaveBeenCalledWith("10.0.0.5", false);
    expect(waitMock).not.toHaveBeenCalled();
  });

  it("sends as it always has when the check can't tell (an older engine)", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    waitMock.mockResolvedValue("unknown");
    expect(await ensurePayloadCurrent("10.0.0.5", undefined, true)).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledTimes(1);
  });
});
