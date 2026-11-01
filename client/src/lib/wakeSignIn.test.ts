import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../state/notifications", () => ({ pushNotification: vi.fn(() => "n") }));
vi.mock("../state/auditLog", () => ({ audit: vi.fn() }));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return {
    ...actual,
    wakeAndSignIn: vi.fn(),
    answerWakePasscode: vi.fn(),
    cancelWakePasscode: vi.fn(),
  };
});

import { answerWakePasscode, cancelWakePasscode, wakeAndSignIn, WakeLoginError } from "../api/ps5";
import { pushNotification } from "../state/notifications";
import {
  cancelWakeSignIn,
  resetWakeSignIn,
  submitWakePasscode,
  useWakeSignInStore,
  wakeAndSignInAsking,
} from "./wakeSignIn";

const HOST = "192.168.1.10";
const CREDS = { credential: "1", registKey: "aa", rpKey: "bb" };
const st = () => useWakeSignInStore.getState();

beforeEach(() => {
  vi.clearAllMocks();
  resetWakeSignIn();
});

describe("Wake & sign in with a login passcode (#608)", () => {
  it("asks for the passcode, then signs in", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode", id: "t1", retry: false, waitS: 180 });
    await wakeAndSignInAsking(HOST, CREDS);
    expect(vi.mocked(wakeAndSignIn).mock.calls[0][4]).toEqual({ askPasscode: true });
    expect(st().ask).toMatchObject({ host: HOST, id: "t1", retry: false, sending: false });
    expect(st().busy[HOST]).toBe(true);

    vi.mocked(answerWakePasscode).mockResolvedValue({ state: "signed_in" });
    await submitWakePasscode("1234");
    expect(answerWakePasscode).toHaveBeenCalledWith("t1", "1234");
    expect(st().ask).toBeNull();
    expect(st().busy[HOST]).toBe(false);
    expect(st().error[HOST]).toBeNull();
    expect(vi.mocked(pushNotification).mock.calls[0][0]).toBe("success");
  });

  it("asks again after a wrong passcode, with a fresh box", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode", id: "t1", retry: false, waitS: 180 });
    await wakeAndSignInAsking(HOST, CREDS);
    const first = st().ask!.seq;
    vi.mocked(answerWakePasscode).mockResolvedValue({ state: "passcode", id: "t1", retry: true, waitS: 180 });
    await submitWakePasscode("0000");
    expect(st().ask).toMatchObject({ retry: true, sending: false });
    expect(st().ask!.seq).not.toBe(first);
    expect(st().busy[HOST]).toBe(true);
  });

  it("cancel leaves the console at sign-in, said plainly and not as an error", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode", id: "t1", retry: false, waitS: 180 });
    await wakeAndSignInAsking(HOST, CREDS);
    vi.mocked(cancelWakePasscode).mockRejectedValue(new WakeLoginError("cancelled", "passcode_cancelled"));
    await cancelWakeSignIn();
    expect(cancelWakePasscode).toHaveBeenCalledWith("t1");
    expect(st().ask).toBeNull();
    expect(st().error[HOST]).toBeNull();
    expect(st().busy[HOST]).toBe(false);
    const [level, title] = vi.mocked(pushNotification).mock.calls[0];
    expect(level).toBe("info");
    expect(title).toContain("sign-in screen");
  });

  it("five wrong passcodes: enter it on the PS5", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode", id: "t1", retry: false, waitS: 180 });
    await wakeAndSignInAsking(HOST, CREDS);
    vi.mocked(answerWakePasscode).mockRejectedValue(new WakeLoginError("too many", "passcode_too_many"));
    await submitWakePasscode("0000");
    expect(st().ask).toBeNull();
    expect(st().error[HOST]).toBe("Too many wrong passcodes; enter it on the PS5.");
  });

  it("an expired prompt says to press Wake & sign in again", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode", id: "t1", retry: false, waitS: 180 });
    await wakeAndSignInAsking(HOST, CREDS);
    vi.mocked(answerWakePasscode).mockRejectedValue(new WakeLoginError("ended", "passcode_expired"));
    await submitWakePasscode("1234");
    expect(st().error[HOST]).toContain("Press Wake & sign in again");
  });

  it("a user without a passcode signs in with no box", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "signed_in" });
    await wakeAndSignInAsking(HOST, CREDS);
    expect(st().ask).toBeNull();
    expect(st().busy[HOST]).toBe(false);
  });

  it("other failures keep the console's own reason", async () => {
    vi.mocked(wakeAndSignIn).mockRejectedValue(new WakeLoginError("the console refused the control session"));
    await wakeAndSignInAsking(HOST, CREDS);
    expect(st().error[HOST]).toBe("the console refused the control session");
  });

  it("a second press while one runs does nothing", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode", id: "t1", retry: false, waitS: 180 });
    await wakeAndSignInAsking(HOST, CREDS);
    await wakeAndSignInAsking(HOST, CREDS);
    expect(wakeAndSignIn).toHaveBeenCalledTimes(1);
  });

  describe("two consoles asking at once", () => {
    const OTHER = "192.168.1.11";
    const asking = async () => {
      vi.mocked(wakeAndSignIn)
        .mockResolvedValueOnce({ state: "passcode", id: "t1", retry: false, waitS: 180 })
        .mockResolvedValueOnce({ state: "passcode", id: "t2", retry: false, waitS: 180 });
      await wakeAndSignInAsking(HOST, CREDS);
      await wakeAndSignInAsking(OTHER, CREDS);
    };

    it("shows one box at a time; the second console's comes after the first is answered", async () => {
      await asking();
      expect(st().ask).toMatchObject({ host: HOST, id: "t1" });
      expect(st().busy).toEqual({ [HOST]: true, [OTHER]: true });

      vi.mocked(answerWakePasscode).mockResolvedValue({ state: "signed_in" });
      await submitWakePasscode("1111");
      expect(answerWakePasscode).toHaveBeenLastCalledWith("t1", "1111");
      expect(st().busy[HOST]).toBe(false);
      expect(st().ask).toMatchObject({ host: OTHER, id: "t2", sending: false });

      await submitWakePasscode("2222");
      expect(answerWakePasscode).toHaveBeenLastCalledWith("t2", "2222");
      expect(st().ask).toBeNull();
      expect(st().busy).toEqual({ [HOST]: false, [OTHER]: false });
    });

    it("cancelling the first box shows the second; both end not busy", async () => {
      await asking();
      vi.mocked(cancelWakePasscode).mockRejectedValue(new WakeLoginError("cancelled", "passcode_cancelled"));
      await cancelWakeSignIn();
      expect(cancelWakePasscode).toHaveBeenLastCalledWith("t1");
      expect(st().busy[HOST]).toBe(false);
      expect(st().ask).toMatchObject({ host: OTHER, id: "t2" });
      await cancelWakeSignIn();
      expect(cancelWakePasscode).toHaveBeenLastCalledWith("t2");
      expect(st().ask).toBeNull();
      expect(st().busy).toEqual({ [HOST]: false, [OTHER]: false });
    });

    it("a wrong passcode keeps that console's box first; the other still waits", async () => {
      await asking();
      vi.mocked(answerWakePasscode).mockResolvedValue({ state: "passcode", id: "t1", retry: true, waitS: 180 });
      await submitWakePasscode("0000");
      expect(st().ask).toMatchObject({ host: HOST, retry: true });
      expect(st().asks.map((a) => a.host)).toEqual([HOST, OTHER]);
    });

    it("the first console failing leaves the second's box up", async () => {
      await asking();
      vi.mocked(answerWakePasscode).mockRejectedValue(new WakeLoginError("too many", "passcode_too_many"));
      await submitWakePasscode("0000");
      expect(st().busy[HOST]).toBe(false);
      expect(st().error[HOST]).toContain("Too many");
      expect(st().ask).toMatchObject({ host: OTHER, id: "t2" });
    });
  });
});
