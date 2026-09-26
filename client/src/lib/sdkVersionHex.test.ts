import { describe, expect, it } from "vitest";
import { fwToSdkHex, sdkHexToFw, PS5_FIRMWARES } from "./sdkVersionHex";

/**
 * PS5 firmware versions are stored BCD-style: the decimal digits are used
 * directly as hex digits, so 9.60 is 0x09600000 — not 0x09060000. The
 * console confirms this: FW 9.60 reports system_sw_raw 0x09600004, and
 * the payload SDK's offset table uses cases like 0x09600000, 0x12700000
 * and 0x13600000.
 *
 * The SDK Version Changer previously asked the user to type this by hand
 * with no explanation, and shipped a default of 0x09060000 — a version
 * that does not exist.
 */
describe("fwToSdkHex", () => {
  it("encodes the digits as BCD, not as a decimal number", () => {
    expect(fwToSdkHex("9.60")).toBe("0x09600000");
    expect(fwToSdkHex("5.05")).toBe("0x05050000");
    expect(fwToSdkHex("12.70")).toBe("0x12700000");
    expect(fwToSdkHex("13.60")).toBe("0x13600000");
  });

  it("pads a single-digit major version", () => {
    expect(fwToSdkHex("1.00")).toBe("0x01000000");
  });

  it("accepts a single-digit minor and pads it", () => {
    expect(fwToSdkHex("9.6")).toBe("0x09600000");
  });

  it("tolerates surrounding whitespace", () => {
    expect(fwToSdkHex("  9.60 ")).toBe("0x09600000");
  });

  it("rejects anything that is not a firmware version", () => {
    expect(fwToSdkHex("")).toBeNull();
    expect(fwToSdkHex("banana")).toBeNull();
    expect(fwToSdkHex("9")).toBeNull();
    expect(fwToSdkHex("9.6.0")).toBeNull();
    expect(fwToSdkHex("-1.00")).toBeNull();
  });
});

describe("sdkHexToFw", () => {
  it("reads a stored value back as a version string", () => {
    expect(sdkHexToFw("0x09600000")).toBe("9.60");
    expect(sdkHexToFw("0x05050000")).toBe("5.05");
    expect(sdkHexToFw("0x12700000")).toBe("12.70");
  });

  it("reads the padded form param.json actually stores", () => {
    expect(sdkHexToFw("0x0960000000000000")).toBe("9.60");
  });

  it("ignores the low bits the console includes in system_sw_raw", () => {
    expect(sdkHexToFw("0x09600004")).toBe("9.60");
  });

  it("returns null for values that are not versions", () => {
    expect(sdkHexToFw("")).toBeNull();
    expect(sdkHexToFw("nope")).toBeNull();
  });

  it("round-trips every firmware we offer in the picker", () => {
    for (const fw of PS5_FIRMWARES) {
      const hex = fwToSdkHex(fw);
      expect(hex).not.toBeNull();
      expect(sdkHexToFw(hex as string)).toBe(fw);
    }
  });
});
