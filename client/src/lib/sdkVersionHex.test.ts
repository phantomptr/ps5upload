import { describe, expect, it } from "vitest";
import { sdkHexToFw } from "./sdkVersionHex";

/**
 * PS5 firmware versions are stored BCD-style: the decimal digits are used
 * directly as hex digits, so 9.60 is 0x09600000 — not 0x09060000. The
 * console confirms this: FW 9.60 reports system_sw_raw 0x09600004, and
 * the payload SDK's offset table uses cases like 0x09600000, 0x12700000
 * and 0x13600000.
 */
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
});
