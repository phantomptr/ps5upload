import { describe, it, expect, vi, beforeEach } from "vitest";

const invokeMock = vi.fn();
const payloadCheckMock = vi.fn();
const sendPayloadMock = vi.fn();

vi.mock("./invokeLogged", () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
vi.mock("../api/ps5", () => ({
  payloadCheck: (...a: unknown[]) => payloadCheckMock(...a),
}));
vi.mock("./restoreMainPayload", () => ({
  restoreMainPayload: (...a: unknown[]) => sendPayloadMock(...a),
}));
vi.mock("../state/logs", () => ({
  log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
}));

import {
  prearmDpiDaemon,
  resetPrearmMemoForTests,
  invalidatePrearm,
  dpiWasArmed,
} from "./prearmDpi";

beforeEach(() => {
  resetPrearmMemoForTests();
  invokeMock.mockReset();
  payloadCheckMock.mockReset();
  sendPayloadMock.mockReset();
});

describe("prearmDpiDaemon", () => {
  it("costs one probe when the daemon is already listening", async () => {
    invokeMock.mockResolvedValue({ ok: true, listening: true, sent: false });
    const r = await prearmDpiDaemon("10.0.0.5:9114");
    expect(r.outcome).toBe("already-up");
    // Nothing was pushed at the loader, and the helper was not disturbed.
    expect(payloadCheckMock).not.toHaveBeenCalled();
    expect(sendPayloadMock).not.toHaveBeenCalled();
  });

  it("arms the daemon and confirms the helper survived", async () => {
    invokeMock.mockResolvedValue({ ok: true, listening: true, sent: true });
    payloadCheckMock.mockResolvedValue({ reachable: true });
    const r = await prearmDpiDaemon("10.0.0.5");
    expect(r.outcome).toBe("armed");
    expect(sendPayloadMock).not.toHaveBeenCalled();
  });

  it("restores the helper when a single-payload loader replaced it", async () => {
    // Measured non-destructive on FW 5.10 and 9.60 — but the loaders in the
    // wild are not those two. Leaving a console with the daemon and no helper
    // is worse than never arming, so this path must put the helper back.
    invokeMock.mockResolvedValue({ ok: true, listening: true, sent: true });
    payloadCheckMock.mockResolvedValue({ reachable: false });
    const r = await prearmDpiDaemon("10.0.0.5");
    expect(r.outcome).toBe("reverted");
    // Via the shared, environment-aware restore — NOT payload_send, which
    // does not exist in the browser build the bug was reported from.
    expect(sendPayloadMock).toHaveBeenCalledWith("10.0.0.5");
  });

  it("treats a dead loader as a non-event, keeping the reason", async () => {
    // The exact 2026-09-08 shape. Nothing is shown to the user here: the
    // install path reports it, with guidance, if it ever actually matters.
    invokeMock.mockResolvedValue({
      ok: false,
      sent: false,
      reason: "loader_unreachable",
      error: "send dpi.elf: connect 10.0.0.5:9021: Connection refused",
    });
    const r = await prearmDpiDaemon("10.0.0.5");
    expect(r.outcome).toBe("unavailable");
    expect(r.reason).toBe("loader_unreachable");
    expect(sendPayloadMock).not.toHaveBeenCalled();
  });

  it("never throws when the bridge itself fails", async () => {
    invokeMock.mockRejectedValue(new Error("bridge gone"));
    await expect(prearmDpiDaemon("10.0.0.5")).resolves.toMatchObject({
      outcome: "unavailable",
    });
  });

  it("attempts once per host, and shares one in-flight attempt", async () => {
    // Two queues connecting at once must not race two ELF pushes at the same
    // loader.
    let resolveEnsure: (v: unknown) => void = () => {};
    invokeMock.mockReturnValue(
      new Promise((res) => {
        resolveEnsure = res;
      }),
    );
    payloadCheckMock.mockResolvedValue({ reachable: true });

    const a = prearmDpiDaemon("10.0.0.5:9114");
    const b = prearmDpiDaemon("10.0.0.5:9113");
    resolveEnsure({ ok: true, listening: true, sent: true });
    expect((await a).outcome).toBe("armed");
    expect((await b).outcome).toBe("skipped");
    expect(invokeMock).toHaveBeenCalledTimes(1);

    // And a later connect to the same console doesn't try again.
    expect((await prearmDpiDaemon("10.0.0.5")).outcome).toBe("skipped");
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("keys the memo on the host, not the address", async () => {
    invokeMock.mockResolvedValue({ ok: true, listening: true, sent: false });
    await prearmDpiDaemon("10.0.0.5:9114");
    await prearmDpiDaemon("10.0.0.6:9114");
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock).toHaveBeenNthCalledWith(1, "dpi_ensure", { ip: "10.0.0.5" });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "dpi_ensure", { ip: "10.0.0.6" });
  });
});

describe("invalidatePrearm + dpiWasArmed (mid-session / wake re-arm)", () => {
  it("re-attempts after invalidation so a dead DPI can be brought back", async () => {
    invokeMock.mockResolvedValue({ ok: true, listening: false, sent: true });
    payloadCheckMock.mockResolvedValue({ reachable: true });

    expect((await prearmDpiDaemon("10.0.0.5")).outcome).toBe("armed");
    // Second call is memoized — the once-per-session guard.
    expect((await prearmDpiDaemon("10.0.0.5")).outcome).toBe("skipped");
    expect(invokeMock).toHaveBeenCalledTimes(1);

    // A death signal (wake edge / observed :9040 drop) clears the memo, and
    // the next call genuinely re-arms instead of returning "skipped".
    invalidatePrearm("10.0.0.5:9114");
    expect((await prearmDpiDaemon("10.0.0.5")).outcome).toBe("armed");
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("marks a host armed when DPI is up, so the poller knows to watch it", async () => {
    expect(dpiWasArmed("10.0.0.5")).toBe(false);
    invokeMock.mockResolvedValue({ ok: true, listening: true, sent: false });
    await prearmDpiDaemon("10.0.0.5:9114");
    expect(dpiWasArmed("10.0.0.5")).toBe(true);
    // Keyed on host, not the probed address.
    expect(dpiWasArmed("10.0.0.5:9040")).toBe(true);
  });

  it("does not mark a host with a dead loader as armed (nothing to watch)", async () => {
    invokeMock.mockResolvedValue({ ok: false, reason: "no_bringup" });
    const r = await prearmDpiDaemon("10.0.0.9");
    expect(r.outcome).toBe("unavailable");
    expect(dpiWasArmed("10.0.0.9")).toBe(false);
  });
});
