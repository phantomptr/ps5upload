import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// "Stopped by you" (#471): a stop has to hold against everything that sends the helper by
// itself, and go away the moment the user asks for the helper again.

function memoryStorage() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, String(v)),
    removeItem: (k: string) => void m.delete(k),
  };
}

const storage = memoryStorage();
vi.stubGlobal("window", { localStorage: storage });

const payloadCheckMock = vi.fn();
const restoreMock = vi.fn();
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("../lib/appVersion", () => ({ getAppVersion: async () => "7.0.29" }));
vi.mock("../api/ps5", () => ({
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
  bundledPayloadPath: vi.fn(),
  sendPayload: vi.fn(),
}));
vi.mock("../lib/elfldrGuard", () => ({
  waitForLoader: async () => "healthy",
  guardElfldr: async () => {},
  STUCK_LOADER_MESSAGE: "",
  LISTENER_LOADER_HINT: "",
}));
vi.mock("../lib/restoreMainPayload", () => ({
  restoreMainPayload: (...a: unknown[]) => restoreMock(...a),
  // The browser build sends through the engine and throws on a refusal.
  engineSendsHelper: (...a: unknown[]) => restoreMock(...a),
}));
vi.mock("./logs", () => ({ log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() } }));

import {
  clearHelperStopped,
  HELPER_STOP_GRACE_MS,
  isHelperStopped,
  markHelperStopped,
  noteHelperAnswering,
  resetHelperStopped,
} from "./helperStopped";
import { autoRedeployDecision } from "../lib/autoRedeploy";
import { ensurePayloadCurrent, resetEnsurePayloadState } from "../lib/ensurePayloadCurrent";

const H = "192.0.2.99";

beforeEach(() => {
  resetHelperStopped();
  resetEnsurePayloadState();
  payloadCheckMock.mockReset();
  restoreMock.mockReset();
});
afterEach(() => vi.useRealTimers());

describe("the reconnect loop", () => {
  it("leaves a console whose helper the user stopped alone", () => {
    const down = { status: "down" as const, busy: false, delivered: 0 };
    expect(autoRedeployDecision(down)).toBe("redeploy");
    expect(autoRedeployDecision({ ...down, stopped: true })).toBe("hold");
    // A helper that is up again still re-arms the loop.
    expect(autoRedeployDecision({ ...down, status: "up", stopped: true })).toBe("rearm");
  });
});

describe("ensurePayloadCurrent", () => {
  it("does not send to a stopped console when nobody asked (recovery, wake resume)", async () => {
    markHelperStopped(H);
    payloadCheckMock.mockResolvedValue({ reachable: false });
    expect(await ensurePayloadCurrent(H, undefined, true)).toBe("no-push");
    expect(await ensurePayloadCurrent(H, undefined, true, "auto")).toBe("no-push");
    expect(restoreMock).not.toHaveBeenCalled();
    expect(payloadCheckMock).not.toHaveBeenCalled();
    expect(isHelperStopped(H)).toBe(true);
  });

  it("sends for an action the user started, and that clears the mark", async () => {
    markHelperStopped(H);
    payloadCheckMock
      .mockResolvedValueOnce({ reachable: false })
      .mockResolvedValue({ reachable: true, payloadVersion: "7.0.29" });
    expect(await ensurePayloadCurrent(H, undefined, false, "explicit")).toBe("pushed");
    expect(restoreMock).toHaveBeenCalledTimes(1);
    expect(isHelperStopped(H)).toBe(false);
  });
});

describe("the mark", () => {
  it("is per console and ignores the port and case", () => {
    markHelperStopped(`${H}:9021`);
    expect(isHelperStopped(H)).toBe(true);
    expect(isHelperStopped("192.0.2.100")).toBe(false);
    markHelperStopped("PS5.local");
    expect(isHelperStopped("ps5.local")).toBe(true);
    clearHelperStopped(H);
    expect(isHelperStopped(H)).toBe(false);
  });

  it("survives a restart (persisted)", async () => {
    markHelperStopped(H, 1234);
    vi.resetModules();
    const fresh = await import("./helperStopped");
    expect(fresh.isHelperStopped(H)).toBe(true);
    expect(fresh.useHelperStoppedStore.getState().stoppedAt[H]).toBe(1234);
  });

  it("goes when a helper answers well after the stop (started some other way)", () => {
    markHelperStopped(H, 1_000);
    // A poll that was already in flight at the stop does not count.
    expect(noteHelperAnswering(H, 1_000 + 5_000)).toBe(false);
    expect(isHelperStopped(H)).toBe(true);
    expect(noteHelperAnswering(H, 1_000 + HELPER_STOP_GRACE_MS)).toBe(true);
    expect(isHelperStopped(H)).toBe(false);
  });
});
