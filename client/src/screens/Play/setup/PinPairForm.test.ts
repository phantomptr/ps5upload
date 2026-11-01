import { afterEach, describe, expect, it, vi } from "vitest";

import { pairForWake } from "../../../api/ps5";
import { canPairWithPin, pinDigits } from "./PinPairForm";

describe("pairing with the PIN shown on the PS5", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("takes the PIN's eight digits however they were typed", () => {
    expect(pinDigits("1234 5678")).toBe("12345678");
    expect(pinDigits("1234-56789")).toBe("12345678");
    expect(pinDigits("12ab")).toBe("12");
  });

  it("needs all eight digits and a readable account id", () => {
    expect(canPairWithPin("12345678", "0x7a356e99a9e2205c")).toBe(true);
    // The base64 chiaki-ng shows works too.
    expect(canPairWithPin("12345678", "XCDiqZluNXo=")).toBe(true);
    expect(canPairWithPin("1234567", "0x7a356e99a9e2205c")).toBe(false);
    expect(canPairWithPin("12345678", "")).toBe(false);
    expect(canPairWithPin("12345678", "not an id")).toBe(false);
  });

  it("asks the engine to register with the typed PIN and account, no helper needed", async () => {
    const fetchMock = vi.fn(async () =>
      new Response(JSON.stringify({ credential: "1", regist_key: "", rp_key: "", nickname: "PS5", mac: "", account_id_b64: "XCDiqZluNXo=", account_via: "manual_pin" }), {
        status: 200,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    await pairForWake("192.168.1.100", undefined, { pin: "12345678", accountIdB64: "XCDiqZluNXo=" });
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toMatch(/\/api\/ps5\/power\/pair$/);
    expect(JSON.parse(String(init.body))).toEqual({ host: "192.168.1.100", pin: "12345678", account_id: "XCDiqZluNXo=" });
  });

  it("automatic pairing sends no PIN, and its failure carries the engine's reason", async () => {
    const reason = "the PS5 could not make a pairing PIN: sceRemoteplayGeneratePinCode failed: 0x80C60011";
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ error: reason }), { status: 502 }));
    vi.stubGlobal("fetch", fetchMock);
    await expect(pairForWake("192.168.1.100", "192.168.1.100:9113")).rejects.toThrow(reason);
    const [, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({ host: "192.168.1.100", addr: "192.168.1.100:9113" });
  });
});
