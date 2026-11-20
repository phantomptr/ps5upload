import { beforeEach, describe, expect, it, vi } from "vitest";

// A queue start or a recovery replaces the helper for every device on the console, and nobody
// was asked: it must not take a newer helper, a pairing or someone's running work away.
const payloadCheckMock = vi.fn();
const restoreMock = vi.fn();
const announceMock = vi.fn();
const copiesMock = vi.fn();
const filesBusyMock = vi.fn();

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("./appVersion", () => ({ getAppVersion: async () => "5.37.1" }));
vi.mock("../api/ps5", async (importOriginal) => ({
  PayloadSendRefused: (await importOriginal<typeof import("../api/ps5")>()).PayloadSendRefused,
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
  bundledPayloadPath: vi.fn(),
  sendPayload: vi.fn(),
}));
vi.mock("../api/mesh", () => ({ meshAnnounce: (...a: unknown[]) => announceMock(...a) }));
vi.mock("./restoreMainPayload", () => ({ engineSendsHelper: (...a: unknown[]) => restoreMock(...a) }));
vi.mock("./elfldrGuard", () => ({
  waitForLoader: async () => "healthy",
  guardElfldr: async () => {},
  STUCK_LOADER_MESSAGE: "",
  LISTENER_LOADER_HINT: "",
}));
vi.mock("../state/logs", () => ({ log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() } }));
vi.mock("../state/helperStoredCopiesRuntime", () => ({
  updateHelperCopiesAfterSend: (...a: unknown[]) => copiesMock(...a),
}));
vi.mock("./ps5Transfers", () => ({ filesTransferBusy: (...a: unknown[]) => filesBusyMock(...a) }));
vi.mock("../state/mesh", () => {
  const byHost: Record<string, { state: unknown }> = {};
  return {
    __byHost: byHost,
    useMeshStore: { getState: () => ({ byHost }) },
    meshFor: (s: { byHost: typeof byHost }, h: string) => s.byHost[h] ?? { state: null },
    othersBusy: (state: { busy?: unknown[] } | null) => state?.busy ?? [],
  };
});

import { ensurePayloadCurrent, lastEnsureOutcome, resetEnsurePayloadState } from "./ensurePayloadCurrent";
import { sentAgoMs } from "./helperSendGate";
import { PayloadSendRefused } from "../api/ps5";
import * as mesh from "../state/mesh";

const H = "10.0.0.5";
const meshState = (mesh as unknown as { __byHost: Record<string, { state: unknown }> }).__byHost;
const answers = (payloadVersion: string) =>
  payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion });

beforeEach(() => {
  [payloadCheckMock, restoreMock, announceMock, copiesMock, filesBusyMock].forEach((m) => m.mockReset());
  announceMock.mockResolvedValue(true);
  copiesMock.mockResolvedValue([]);
  filesBusyMock.mockReturnValue(false);
  delete meshState[H];
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

describe("replacing a running helper unasked", () => {
  it.each([[false], [true]])("never puts this app's helper over a newer one (force=%s)", async (force) => {
    answers("6.0.0");
    expect(await ensurePayloadCurrent(H, undefined, force)).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it("leaves a helper that only wants pairing, even on a forced recovery", async () => {
    payloadCheckMock.mockResolvedValue({ reachable: false, session: "needs_pairing" });
    expect(await ensurePayloadCurrent(H, undefined, true)).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it.each([[false], [true]])(
    "leaves a helper that is running but slow to answer, as on a weak Wi-Fi link (force=%s)",
    async (force) => {
      payloadCheckMock.mockResolvedValue({ reachable: false, session: "down", reason: "status_slow" });
      expect(await ensurePayloadCurrent(H, undefined, force)).toBe("no-push");
      expect(restoreMock).not.toHaveBeenCalled();
    },
  );

  it("does not stop other devices' work", async () => {
    answers("5.0.0");
    meshState[H] = { state: { busy: [{ device: {}, items: [{}] }] } };
    expect(await ensurePayloadCurrent(H)).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it("does not stop a Files transfer on that console", async () => {
    answers("5.0.0");
    filesBusyMock.mockReturnValue(true);
    expect(await ensurePayloadCurrent(H)).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it("says so before replacing an older helper on a quiet console", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: true, payloadVersion: "5.0.0" })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    meshState[H] = { state: { busy: [] } };
    const order: string[] = [];
    announceMock.mockImplementation(async () => (order.push("announce"), true));
    restoreMock.mockImplementation(async () => void order.push("send"));
    // The automatic update asked the console itself first (consoleChecked).
    expect(
      await onFakeClock(() => ensurePayloadCurrent(H, undefined, false, "auto", { consoleChecked: true })),
    ).toBe("pushed");
    expect(order).toEqual(["announce", "send"]);
    expect(announceMock.mock.calls[0].slice(0, 2)).toEqual([H, "restart"]);
  });

  it("does not replace, unattended, an older helper that cannot say whether the PS5 is busy", async () => {
    answers("5.0.0");
    expect(await ensurePayloadCurrent(H)).toBe("no-push");
    expect(lastEnsureOutcome(H)?.why).toBe("console-unknown");
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it("does not replace a helper that reports work a restart would stop, asked or not", async () => {
    const busy = { uploads: 0, install: true, extract: false, convert: false, dump: false, input: false, pads: 0, otherDevices: 0, idle: false };
    payloadCheckMock.mockResolvedValue({ reachable: true, payloadVersion: "5.0.0", busy });
    for (const mode of ["auto", "explicit"] as const) {
      expect(await ensurePayloadCurrent(H, undefined, false, mode, { consoleChecked: true })).toBe("no-push");
      expect(lastEnsureOutcome(H)?.why).toBe("console-busy");
    }
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it.each([["helper_alive_unreadable"], ["helper_too_new"]])(
    "never replaces a helper that answered in a way this app cannot use (%s), even on a forced recovery",
    async (reason) => {
      payloadCheckMock.mockResolvedValue({ reachable: false, session: "down", reason, helperAlive: true });
      for (const mode of ["auto", "explicit"] as const) {
        expect(await ensurePayloadCurrent(H, undefined, true, mode)).toBe("no-push");
        expect(lastEnsureOutcome(H)?.why).toBe("answering");
      }
      expect(restoreMock).not.toHaveBeenCalled();
    },
  );

  it("replaces a too-old helper only when someone started the upload, never unattended", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false, session: "down", reason: "helper_not_ava1", helperTooOld: true, helperAlive: true })
      .mockResolvedValueOnce({ reachable: false, session: "down", reason: "helper_not_ava1", helperTooOld: true, helperAlive: true })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    expect(await ensurePayloadCurrent(H, undefined, true, "auto")).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
    expect(await onFakeClock(() => ensurePayloadCurrent(H, undefined, false, "explicit"))).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledWith(H, expect.objectContaining({ mode: "explicit" }));
  });

  it("a forced recovery leaves a current helper that answers (transfers share its port)", async () => {
    answers("5.37.1");
    expect(await ensurePayloadCurrent(H, undefined, true)).toBe("current");
    expect(restoreMock).not.toHaveBeenCalled();
  });

  it("holds, unattended, a helper whose version does not read as one", async () => {
    answers("dev-build");
    expect(await ensurePayloadCurrent(H, undefined, true)).toBe("no-push");
    expect(lastEnsureOutcome(H)?.why).toBe("version-unknown");
  });

  it("a send that broke after the loader took it counts as a send", async () => {
    payloadCheckMock.mockResolvedValue({ reachable: false });
    restoreMock.mockRejectedValue(new Error("send timed out: 10.0.0.5:9021 took nothing for 20 s"));
    expect(await ensurePayloadCurrent(H)).toBe("sent-failed");
    expect(sentAgoMs(H)).not.toBeNull();
    // A loader never reached is no send.
    resetEnsurePayloadState();
    restoreMock.mockRejectedValue(new Error("connect 10.0.0.5:9021: Connection refused"));
    expect(await ensurePayloadCurrent(H)).toBe("no-push");
    expect(sentAgoMs(H)).toBeNull();
  });

  it("an engine refusal is held with its code and when it may go again", async () => {
    payloadCheckMock.mockResolvedValue({ reachable: false });
    restoreMock.mockRejectedValue(
      new PayloadSendRefused("send_budget_spent", { message: "x", retryAfterMs: 60_000 }),
    );
    expect(await ensurePayloadCurrent(H)).toBe("no-push");
    const o = lastEnsureOutcome(H);
    expect(o?.why).toBe("send_budget_spent");
    expect(o?.retryAt).toBeGreaterThan(Date.now());
  });

  it("a console with no helper gets one, whatever a stale picture of the others says", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    meshState[H] = { state: { busy: [{ device: {}, items: [{}] }] } };
    expect(await onFakeClock(() => ensurePayloadCurrent(H, undefined, true))).toBe("pushed");
    expect(announceMock).not.toHaveBeenCalled();
  });
});

describe("the copies the PS5 starts at boot", () => {
  it("are brought up to date after the queue or the reconnect loop sends", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    expect(await onFakeClock(() => ensurePayloadCurrent(H, undefined, true))).toBe("pushed");
    expect(copiesMock).toHaveBeenCalledTimes(1);
    expect(copiesMock.mock.calls[0][0]).toBe(H);
  });

  it("are left alone when nothing was sent", async () => {
    answers("5.37.1");
    expect(await ensurePayloadCurrent(H)).toBe("current");
    expect(copiesMock).not.toHaveBeenCalled();
  });
});
