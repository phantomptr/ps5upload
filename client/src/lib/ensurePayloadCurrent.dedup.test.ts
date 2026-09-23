import { describe, it, expect, vi, beforeEach } from "vitest";

// Two sends that land together start two helpers, which then fight over the
// takeover (the Phat's startup log: ENTER_MAIN pairs 82 ms and 180 ms apart).
const payloadCheckMock = vi.fn();
const restoreMock = vi.fn();
const appVersionMock = vi.fn();

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("./appVersion", () => ({ getAppVersion: () => appVersionMock() }));
vi.mock("../api/ps5", () => ({
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
  bundledPayloadPath: vi.fn(),
  sendPayload: vi.fn(),
}));
vi.mock("./restoreMainPayload", () => ({
  restoreMainPayload: (...a: unknown[]) => restoreMock(...a),
}));
vi.mock("./prearmDpi", () => ({
  prearmDpiDaemon: () => Promise.resolve({ outcome: "already-up" }),
}));
vi.mock("../state/logs", () => ({
  log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

import {
  ensurePayloadCurrent,
  resetEnsurePayloadState,
  RESEND_COOLDOWN_MS,
} from "./ensurePayloadCurrent";

beforeEach(() => {
  vi.useRealTimers();
  [payloadCheckMock, restoreMock, appVersionMock].forEach((m) => m.mockReset());
  appVersionMock.mockResolvedValue("5.33.3");
  resetEnsurePayloadState();
});

describe("sending the helper", () => {
  it("shares one check between callers that arrive together", async () => {
    // First probe: nothing running. After the send: the new helper answers.
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.33.3" });
    const [a, b] = await Promise.all([
      ensurePayloadCurrent("10.0.0.5", undefined, true),
      ensurePayloadCurrent("10.0.0.5", undefined, true),
    ]);
    expect(a).toBe("pushed");
    expect(b).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledTimes(1);
  });

  it("does not send again while the last one may still be starting", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    payloadCheckMock.mockResolvedValue({ reachable: false });
    const first = ensurePayloadCurrent("10.0.0.5", undefined, true);
    await vi.advanceTimersByTimeAsync(40_000);
    expect(await first).toBe("stale-ok");
    expect(restoreMock).toHaveBeenCalledTimes(1);

    expect(await ensurePayloadCurrent("10.0.0.5", undefined, true)).toBe("no-push");
    expect(restoreMock).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(RESEND_COOLDOWN_MS);
    const again = ensurePayloadCurrent("10.0.0.5", undefined, true);
    await vi.advanceTimersByTimeAsync(40_000);
    await again;
    expect(restoreMock).toHaveBeenCalledTimes(2);
  });

  it("keeps consoles independent", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.33.3" });
    await Promise.all([
      ensurePayloadCurrent("10.0.0.5", undefined, true),
      ensurePayloadCurrent("10.0.0.6", undefined, true),
    ]);
    expect(restoreMock).toHaveBeenCalledTimes(2);
  });
});
