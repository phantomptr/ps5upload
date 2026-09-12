import { describe, it, expect } from "vitest";
import {
  powerStateFromDdp,
  wakeUi,
  isValidWakeCredential,
  WAKE_REQUIREMENTS,
  credentialFromRegistKeyHex,
  isValidSessionKey,
  wakeSetupStage,
} from "./wakeState";

describe("powerStateFromDdp", () => {
  it("maps the two codes the console actually sends", () => {
    expect(powerStateFromDdp(200)).toBe("awake");
    expect(powerStateFromDdp(620)).toBe("standby");
  });
  it("treats no reply and unknown codes as offline, not a guess", () => {
    expect(powerStateFromDdp(undefined)).toBe("offline");
    expect(powerStateFromDdp(0)).toBe("offline");
    expect(powerStateFromDdp(500)).toBe("offline");
  });
});

describe("wakeUi", () => {
  it("offers wake only when the console is asleep AND a credential exists", () => {
    expect(wakeUi("standby", true).showWakeButton).toBe(true);
    // Asleep but nothing to wake it with.
    expect(wakeUi("standby", false).showWakeButton).toBe(false);
    // Credential ready but the console is already awake.
    expect(wakeUi("awake", true).showWakeButton).toBe(false);
    expect(wakeUi("offline", true).showWakeButton).toBe(false);
  });

  it("offers setup exactly while there is no credential", () => {
    expect(wakeUi("awake", false).showSetup).toBe(true);
    expect(wakeUi("standby", false).showSetup).toBe(true);
    expect(wakeUi("awake", true).showSetup).toBe(false);
  });

  it("allows automatic setup only while the console is awake", () => {
    // Registration talks to the console, so it must be on.
    expect(wakeUi("awake", false).canAutoSetup).toBe(true);
    expect(wakeUi("standby", false).canAutoSetup).toBe(false);
    expect(wakeUi("offline", false).canAutoSetup).toBe(false);
  });
});

describe("isValidWakeCredential", () => {
  it("accepts a plausible decimal credential", () => {
    expect(isValidWakeCredential("1499970515")).toBe(true);
    expect(isValidWakeCredential("  2944964913 ")).toBe(true);
  });
  it("rejects the raw hex key, which is the most likely wrong paste", () => {
    expect(isValidWakeCredential("5967bbd3")).toBe(false);
    expect(isValidWakeCredential("af889931")).toBe(false);
  });
  it("rejects empty, non-numeric, zero, and out-of-range", () => {
    expect(isValidWakeCredential("")).toBe(false);
    expect(isValidWakeCredential("   ")).toBe(false);
    expect(isValidWakeCredential("not a number")).toBe(false);
    expect(isValidWakeCredential("0")).toBe(false);
    expect(isValidWakeCredential("18446744073709551616")).toBe(false); // u64 max + 1
  });
  // The range check is string arithmetic, not BigInt (ES2020 can't be
  // down-levelled for the old-WebView build target), so pin the boundary.
  it("accepts u64 max exactly and rejects just past it", () => {
    expect(isValidWakeCredential("18446744073709551615")).toBe(true);
    expect(isValidWakeCredential("18446744073709551620")).toBe(false);
    expect(isValidWakeCredential("99999999999999999999")).toBe(false);
    expect(isValidWakeCredential("184467440737095516150")).toBe(false); // longer
  });
  it("treats padded zeros as the number they spell", () => {
    expect(isValidWakeCredential("0000000000")).toBe(false);
    expect(isValidWakeCredential("0000001499970515")).toBe(true);
  });
});

describe("WAKE_REQUIREMENTS", () => {
  it("names the three settings that silently block wake", () => {
    const labels = WAKE_REQUIREMENTS.map((r) => r.labelFallback);
    expect(labels).toContain("Enable Remote Play");
    expect(labels).toContain("Stay Connected to the Internet");
    expect(labels).toContain("Enable Turning On PS5 from Network");
  });
  it("gives the two rest-mode settings the same menu path", () => {
    const restMode = WAKE_REQUIREMENTS.filter((r) =>
      r.pathFallback.includes("Rest Mode"),
    );
    expect(restMode).toHaveLength(2);
    expect(new Set(restMode.map((r) => r.pathKey)).size).toBe(1);
  });
});

describe("credentialFromRegistKeyHex", () => {
  it("derives the wake credential the way the console does", () => {
    // "5967bbd3" ASCII, NUL-padded to 16 bytes → 0x5967bbd3 → decimal.
    expect(credentialFromRegistKeyHex("35393637626264330000000000000000")).toBe(
      "1499970515",
    );
    expect(credentialFromRegistKeyHex("61663838393933310000000000000000")).toBe(
      "2944964913",
    );
  });
  it("rejects non-hex or empty input", () => {
    expect(credentialFromRegistKeyHex("")).toBe("");
    expect(credentialFromRegistKeyHex("not hex")).toBe("");
    expect(credentialFromRegistKeyHex("abc")).toBe(""); // odd length
  });
  // Hand-converted boundaries for the BigInt-free base conversion.
  it("converts the full 16-hex-digit range exactly", () => {
    // "ffffffffffffffff" (16 chars) NUL-padded → u64 max.
    expect(credentialFromRegistKeyHex("66".repeat(16))).toBe(
      "18446744073709551615",
    );
    // "1" → 1, the smallest usable credential.
    expect(credentialFromRegistKeyHex("31" + "00".repeat(15))).toBe("1");
    // "deadbeef" → 3735928559.
    expect(credentialFromRegistKeyHex("6465616462656566" + "00".repeat(8))).toBe(
      "3735928559",
    );
  });
  it("rejects a value wider than u64 and an all-zero key", () => {
    // 17 significant hex digits ("1" + 16 × "f") does not fit in u64.
    expect(
      credentialFromRegistKeyHex("31" + "66".repeat(16)),
    ).toBe("");
    expect(credentialFromRegistKeyHex("30".repeat(8) + "00".repeat(8))).toBe("");
  });
});

describe("isValidSessionKey", () => {
  it("requires exactly 32 hex chars (16 bytes)", () => {
    expect(isValidSessionKey("1395c8cc7eca16fe982eb22e527ba3da")).toBe(true);
    expect(isValidSessionKey(" 1395C8CC7ECA16FE982EB22E527BA3DA ")).toBe(true);
    expect(isValidSessionKey("1395c8cc")).toBe(false);
    expect(isValidSessionKey("")).toBe(false);
    expect(isValidSessionKey("zz95c8cc7eca16fe982eb22e527ba3da")).toBe(false);
  });
});

describe("wakeUi with session keys", () => {
  it("enables wake from session keys even without a separate credential", () => {
    expect(wakeUi("standby", false, true).showWakeButton).toBe(true);
    expect(wakeUi("standby", false, true).canSignIn).toBe(true);
    expect(wakeUi("standby", false, false).showWakeButton).toBe(false);
  });
  it("hides setup once session keys exist", () => {
    expect(wakeUi("awake", false, true).showSetup).toBe(false);
  });
});

describe("wakeSetupStage", () => {
  it("offers one-click setup when the console is awake and unpaired", () => {
    const s = wakeSetupStage("awake", false, false, false);
    expect(s).toBe("offer");
  });
  it("cannot pair a console it cannot reach", () => {
    // Pairing registers over the network, so the console must be on. Saying
    // so beats today's silently-hidden button.
    expect(wakeSetupStage("standby", false, false, false)).toBe("unreachable");
    expect(wakeSetupStage("offline", false, false, false)).toBe("unreachable");
  });
  it("reports the working state while pairing, whatever the power state", () => {
    expect(wakeSetupStage("awake", false, false, true)).toBe("working");
    expect(wakeSetupStage("offline", false, false, true)).toBe("working");
  });
  it("separates a wake-only pairing from a full sign-in pairing", () => {
    // Credential alone wakes to user-select; session keys wake into the user.
    expect(wakeSetupStage("awake", true, false, false)).toBe("wake-only");
    expect(wakeSetupStage("awake", true, true, false)).toBe("done");
    // Session keys imply the credential, so they alone are a complete setup.
    expect(wakeSetupStage("standby", false, true, false)).toBe("done");
  });
  it("keeps showing the finished state when the console sleeps", () => {
    expect(wakeSetupStage("standby", true, true, false)).toBe("done");
    expect(wakeSetupStage("offline", true, true, false)).toBe("done");
  });
});
