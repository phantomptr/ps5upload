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

import {
  ensurePayloadCurrent,
  lastEnsureOutcome,
  resetEnsurePayloadState,
  RESEND_COOLDOWN_MS,
} from "./ensurePayloadCurrent";
import { sendHelperOnce } from "./helperSendGate";

beforeEach(() => {
  vi.useRealTimers();
  [payloadCheckMock, restoreMock, appVersionMock].forEach((m) => m.mockReset());
  appVersionMock.mockResolvedValue("5.33.3");
  resetEnsurePayloadState();
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

describe("sending the helper", () => {
  it("shares one check between callers that arrive together", async () => {
    // First probe: nothing running. After the send: the new helper answers.
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.33.3" });
    const [a, b] = await onFakeClock(() =>
      Promise.all([
        ensurePayloadCurrent("10.0.0.5", undefined, true),
        ensurePayloadCurrent("10.0.0.5", undefined, true),
      ]),
    );
    expect(a).toBe("pushed");
    expect(b).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledTimes(1);
  });

  it("waits about two minutes for the helper it sent before saying it did not show", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    payloadCheckMock.mockResolvedValue({ reachable: false });
    const first = ensurePayloadCurrent("10.0.0.5", undefined, true);
    let done = false;
    void first.then(() => (done = true));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(70_000);
    expect(await first).toBe("stale-ok");
    expect(restoreMock).toHaveBeenCalledTimes(1);
  });

  it("does not send again while the last one may still be starting", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    payloadCheckMock.mockResolvedValue({ reachable: false });
    // A helper was sent moments ago (the Send button, another path).
    await sendHelperOnce("10.0.0.5", async () => {});
    expect(await ensurePayloadCurrent("10.0.0.5", undefined, true)).toBe("no-push");
    expect(lastEnsureOutcome("10.0.0.5")?.why).toBe("recent");
    expect(restoreMock).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(RESEND_COOLDOWN_MS);
    const again = ensurePayloadCurrent("10.0.0.5", undefined, true);
    await vi.advanceTimersByTimeAsync(130_000);
    await again;
    expect(restoreMock).toHaveBeenCalledTimes(1);
  });

  it("an explicit request joining an unattended check that held back runs after it, as itself", async () => {
    // The unattended check holds (the console just started); the person's queue start then
    // waits out the start itself instead of taking the "no-push".
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.33.3" });
    await sendHelperOnce("10.0.0.5", async () => {});
    const auto = ensurePayloadCurrent("10.0.0.5", undefined, false, "auto");
    const asked = ensurePayloadCurrent("10.0.0.5", undefined, false, "explicit");
    expect(await auto).toBe("no-push");
    // The explicit one ran too (and met the same cooldown: nothing sent twice).
    expect(await asked).toBe("no-push");
    expect(payloadCheckMock).toHaveBeenCalledTimes(2);
  });

  it("keeps consoles independent", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.33.3" });
    await onFakeClock(() =>
      Promise.all([
        ensurePayloadCurrent("10.0.0.5", undefined, true),
        ensurePayloadCurrent("10.0.0.6", undefined, true),
      ]),
    );
    expect(restoreMock).toHaveBeenCalledTimes(2);
  });
});
