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
vi.mock("../api/ps5", () => ({
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

import { ensurePayloadCurrent, resetEnsurePayloadState } from "./ensurePayloadCurrent";
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
    expect(await ensurePayloadCurrent(H)).toBe("pushed");
    expect(order).toEqual(["announce", "send"]);
    expect(announceMock.mock.calls[0].slice(0, 2)).toEqual([H, "restart"]);
  });

  it("a console with no helper gets one, whatever a stale picture of the others says", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    meshState[H] = { state: { busy: [{ device: {}, items: [{}] }] } };
    expect(await ensurePayloadCurrent(H, undefined, true)).toBe("pushed");
    expect(announceMock).not.toHaveBeenCalled();
  });
});

describe("the copies the PS5 starts at boot", () => {
  it("are brought up to date after the queue or the reconnect loop sends", async () => {
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "5.37.1" });
    expect(await ensurePayloadCurrent(H, undefined, true)).toBe("pushed");
    expect(copiesMock).toHaveBeenCalledTimes(1);
    expect(copiesMock.mock.calls[0][0]).toBe(H);
  });

  it("are left alone when nothing was sent", async () => {
    answers("5.37.1");
    expect(await ensurePayloadCurrent(H)).toBe("current");
    expect(copiesMock).not.toHaveBeenCalled();
  });
});
