import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../state/auditLog", () => ({ audit: vi.fn() }));
vi.mock("../state/logs", () => ({ log: { info: vi.fn(), warn: vi.fn() } }));
vi.mock("../api/ps5", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/ps5")>();
  return { ...actual, wakeAndSignIn: vi.fn(), powerWake: vi.fn(async () => ({ ok: true })) };
});

import { wakeAndSignIn } from "../api/ps5";
import { log } from "../state/logs";
import { useRosterStore } from "../state/roster";
import {
  resetWakeForUpload,
  shouldWakeForUpload,
  wakeForUpload,
  WAKE_FOR_UPLOAD_RETRY_MS,
} from "./wakeForUpload";
import { useWakeSignInStore } from "./wakeSignIn";

describe("shouldWakeForUpload", () => {
  const base = { enabled: true, canWake: true, lastSentAt: undefined, now: 1_000_000 };

  it("wakes a console with wake set up", () => {
    expect(shouldWakeForUpload(base)).toBe(true);
  });

  it("never wakes when the setting is off", () => {
    expect(shouldWakeForUpload({ ...base, enabled: false })).toBe(false);
  });

  it("never wakes a console without a wake credential", () => {
    expect(shouldWakeForUpload({ ...base, canWake: false })).toBe(false);
  });

  it("does not resend while the last wake is still recent", () => {
    expect(shouldWakeForUpload({ ...base, lastSentAt: base.now - 10_000 })).toBe(false);
  });

  it("sends again once the retry window has passed", () => {
    expect(
      shouldWakeForUpload({ ...base, lastSentAt: base.now - WAKE_FOR_UPLOAD_RETRY_MS }),
    ).toBe(true);
  });
});

describe("wakeForUpload never prompts", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resetWakeForUpload();
    useRosterStore.setState({
      profiles: [
        { id: "p1", name: "PS5", host: "10.0.0.5", wake_regist_key: "aa", wake_rp_key: "bb" },
      ] as unknown as ReturnType<typeof useRosterStore.getState>["profiles"],
    });
  });

  it("signs in without asking, and a passcode screen counts as woken", async () => {
    vi.mocked(wakeAndSignIn).mockResolvedValue({ state: "passcode_needed" });
    expect(wakeForUpload("10.0.0.5:9113", true, 1)).toBe(true);
    const call = vi.mocked(wakeAndSignIn).mock.calls[0];
    // No askPasscode: the engine ends the sign-in at the passcode screen instead of holding it.
    expect(call[4]).toBeUndefined();
    await Promise.resolve();
    await Promise.resolve();
    expect(vi.mocked(log.info).mock.calls.some(([, m]) => String(m).includes("sign-in screen"))).toBe(true);
    expect(log.warn).not.toHaveBeenCalled();
    expect(useWakeSignInStore.getState().ask).toBeNull();
  });
});
