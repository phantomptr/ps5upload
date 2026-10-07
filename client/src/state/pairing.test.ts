import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  pairingStatus: vi.fn(),
  pairingConfirm: vi.fn(),
  pairingCancel: vi.fn(),
  pairingForget: vi.fn(),
}));
vi.mock("../api/ava1", () => api);

import { reportIfNotPaired } from "../lib/consoleSession";
import { usePairingStore } from "./pairing";

beforeEach(() => {
  api.pairingStatus.mockReset();
  api.pairingConfirm.mockReset();
  api.pairingCancel.mockReset();
  api.pairingForget.mockReset();
  usePairingStore.getState().close();
  usePairingStore.setState({ quietUntil: {} });
});

const CODE = { state: "code", consoleName: "PS5-Pro" } as const;

describe("pairing store", () => {
  it("asks for the code the console shows, then sends what the user typed", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    api.pairingConfirm.mockResolvedValue({ state: "accepted" });
    await usePairingStore.getState().openFor("10.0.0.2");
    let s = usePairingStore.getState();
    expect(s.open && s.host).toBe("10.0.0.2");
    expect(s.view).toEqual(CODE);
    await s.confirm("004821");
    expect(api.pairingConfirm).toHaveBeenCalledWith("10.0.0.2", "004821");
    s = usePairingStore.getState();
    expect(s.open).toBe(false);
    expect(s.paired).toBe("10.0.0.2");
  });

  it("explains a closed window and can try again", async () => {
    api.pairingStatus.mockResolvedValueOnce({ state: "closed" });
    await usePairingStore.getState().openFor("10.0.0.2");
    expect(usePairingStore.getState().view).toEqual({ state: "closed" });
    api.pairingStatus.mockResolvedValueOnce(CODE);
    await usePairingStore.getState().retry();
    expect(usePairingStore.getState().view).toEqual(CODE);
  });

  it("a confirm that finds the window shut goes back to the closed explanation", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    api.pairingConfirm.mockResolvedValue({ state: "closed" });
    await usePairingStore.getState().openFor("10.0.0.2");
    await usePairingStore.getState().confirm("123456");
    const s = usePairingStore.getState();
    expect(s.open).toBe(true);
    expect(s.view).toEqual({ state: "closed" });
  });

  it("a wrong code shows the retry state and stays open", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    const WRONG = { state: "wrong_code", consoleName: "PS5-Pro" } as const;
    api.pairingConfirm.mockResolvedValueOnce(WRONG);
    await usePairingStore.getState().openFor("10.0.0.2");
    await usePairingStore.getState().confirm("111111");
    let s = usePairingStore.getState();
    expect(s.open).toBe(true);
    expect(s.view).toEqual(WRONG);
    api.pairingConfirm.mockResolvedValueOnce({ state: "accepted" });
    await s.confirm("222222");
    s = usePairingStore.getState();
    expect(s.open).toBe(false);
    expect(s.paired).toBe("10.0.0.2");
  });

  it("dismissing the dialog closes the engine's pending handshake", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    await usePairingStore.getState().openFor("10.0.0.2");
    usePairingStore.getState().dismiss();
    expect(api.pairingCancel).toHaveBeenCalledWith("10.0.0.2");
    expect(usePairingStore.getState().open).toBe(false);
  });

  it("a different console at the pinned address: forget, then pair afresh", async () => {
    api.pairingStatus.mockResolvedValueOnce({ state: "wrong_console" });
    await usePairingStore.getState().openFor("10.0.0.2");
    expect(usePairingStore.getState().view).toEqual({ state: "wrong_console" });
    api.pairingForget.mockResolvedValue(undefined);
    api.pairingStatus.mockResolvedValueOnce(CODE);
    await usePairingStore.getState().forgetAndPair();
    expect(api.pairingForget).toHaveBeenCalledWith("10.0.0.2");
    expect(usePairingStore.getState().view).toEqual(CODE);
    expect(usePairingStore.getState().error).toBeNull();
  });

  it("a failed forget keeps the explanation and shows the error", async () => {
    api.pairingStatus.mockResolvedValueOnce({ state: "wrong_console" });
    await usePairingStore.getState().openFor("10.0.0.2");
    api.pairingForget.mockRejectedValue(new Error("engine said no"));
    await usePairingStore.getState().forgetAndPair();
    const s = usePairingStore.getState();
    expect(s.view).toEqual({ state: "wrong_console" });
    expect(s.error).toBe("engine said no");
  });

  it("a wrong-console failure opens the dialog like a not-paired one", async () => {
    api.pairingStatus.mockResolvedValue({ state: "wrong_console" });
    expect(reportIfNotPaired("ava1_wrong_console", "10.0.0.2")).toBe(true);
    await vi.waitFor(() => expect(usePairingStore.getState().open).toBe(true));
  });

  it("keeps the dialog open with the error when the console cannot be reached", async () => {
    api.pairingStatus.mockRejectedValue(new Error("timed out"));
    await usePairingStore.getState().openFor("10.0.0.2");
    const s = usePairingStore.getState();
    expect(s.open).toBe(true);
    expect(s.error).toBe("timed out");
    expect(s.view).toBeNull();
  });

  it("opens by itself when any call comes back not_paired, once", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    expect(reportIfNotPaired("ava1_not_paired", "10.0.0.2")).toBe(true);
    await vi.waitFor(() => expect(usePairingStore.getState().open).toBe(true));
    expect(api.pairingStatus).toHaveBeenCalledTimes(1);
    // A second not_paired while it is open does not start a second handshake.
    reportIfNotPaired("not_paired", "10.0.0.2");
    expect(api.pairingStatus).toHaveBeenCalledTimes(1);
  });

  it("stays quiet for a while after the user dismisses it", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    await usePairingStore.getState().openFor("10.0.0.2");
    usePairingStore.getState().dismiss();
    reportIfNotPaired("not_paired", "10.0.0.2");
    await Promise.resolve();
    expect(usePairingStore.getState().open).toBe(false);
    // The explicit Pair… button is never muted.
    await usePairingStore.getState().openFor("10.0.0.2");
    expect(usePairingStore.getState().open).toBe(true);
  });

  it("never opens for a failure that names no console, and opens the named one, not the active one", async () => {
    api.pairingStatus.mockResolvedValue(CODE);
    reportIfNotPaired("not_paired");
    await Promise.resolve();
    expect(usePairingStore.getState().open).toBe(false);
    expect(api.pairingStatus).not.toHaveBeenCalled();
    reportIfNotPaired("not_paired", "10.0.0.9");
    await vi.waitFor(() => expect(usePairingStore.getState().open).toBe(true));
    expect(usePairingStore.getState().host).toBe("10.0.0.9");
    expect(api.pairingStatus).toHaveBeenCalledWith("10.0.0.9");
  });

  it("treats a state it does not know as an error the user can retry", async () => {
    api.pairingStatus.mockResolvedValue({ state: "none" });
    await usePairingStore.getState().openFor("10.0.0.2");
    const s = usePairingStore.getState();
    expect(s.open).toBe(true);
    expect(s.view).toBeNull();
    expect(s.error).toContain("unexpected");
    api.pairingStatus.mockResolvedValue(CODE);
    await s.retry();
    expect(usePairingStore.getState().view).toEqual(CODE);
    expect(usePairingStore.getState().error).toBeNull();
  });
  it("after the helper is sent again, keeps asking while the window still reads closed", async () => {
    api.pairingStatus.mockResolvedValueOnce({ state: "closed" });
    await usePairingStore.getState().openFor("10.0.0.2");
    // The new helper is still starting: closed twice more, then it has paired by itself.
    api.pairingStatus
      .mockResolvedValueOnce({ state: "closed" })
      .mockResolvedValueOnce({ state: "closed" })
      .mockResolvedValueOnce({ state: "accepted" });
    const naps: number[] = [];
    await usePairingStore.getState().retryAfterResend(async (ms) => {
      naps.push(ms);
    });
    const s = usePairingStore.getState();
    expect(s.open).toBe(false);
    expect(s.paired).toBe("10.0.0.2");
    expect(naps).toHaveLength(2);
  });

  it("after the helper is sent again, stops asking once there is a code to type or it gave up", async () => {
    api.pairingStatus.mockResolvedValueOnce({ state: "closed" });
    await usePairingStore.getState().openFor("10.0.0.2");
    api.pairingStatus.mockResolvedValueOnce(CODE);
    await usePairingStore.getState().retryAfterResend(async () => {});
    expect(usePairingStore.getState().view).toEqual(CODE);
    expect(api.pairingStatus).toHaveBeenCalledTimes(2);

    // A window that stays closed is asked a bounded number of times, and stays explained.
    api.pairingStatus.mockReset();
    api.pairingStatus.mockResolvedValue({ state: "closed" });
    await usePairingStore.getState().retryAfterResend(async () => {});
    expect(api.pairingStatus.mock.calls.length).toBeLessThanOrEqual(10);
    expect(usePairingStore.getState().view).toEqual({ state: "closed" });
    expect(usePairingStore.getState().open).toBe(true);
  });
});
