import { describe, it, expect } from "vitest";
import {
  powerStateFromDdp,
  wakeUi,
  isValidWakeCredential,
  WAKE_REQUIREMENTS,
  credentialFromRegistKeyHex,
  isValidSessionKey,
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
