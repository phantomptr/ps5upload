import { describe, it, expect } from "vitest";
import { accountIdToChiakiNumeric } from "./remoteplay";

describe("accountIdToChiakiNumeric", () => {
  it("returns empty string for empty input", () => {
    expect(accountIdToChiakiNumeric("")).toBe("");
  });

  it("returns empty string for invalid base64", () => {
    expect(accountIdToChiakiNumeric("!!!")).toBe("");
  });

  it("decodes a known account_id to the correct numeric ID", () => {
    // 8 bytes: 0x00 0x00 0x00 0x01 0x00 0x00 0x00 0x01
    // = 4294967297 (2^32 + 1)
    // base64 of these 8 bytes: AAAAAQAAAAE=
    const result = accountIdToChiakiNumeric("AAAAAQAAAAE=");
    expect(result).toBe("4294967297");
  });

  it("handles all-zeros account_id", () => {
    expect(accountIdToChiakiNumeric("AAAAAAAAAAA=")).toBe("0");
  });

  it("handles max 64-bit value", () => {
    // 0xFFFFFFFFFFFFFFFF = 18446744073709551615
    // base64: ///////////8=
    // Actually 8 bytes of 0xFF = ///////////8= but let's verify
    // 0xFF * 8 bytes = CP////////8= is wrong. Let me just test it:
    const bytes = new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    const b64 = btoa(bin);
    const result = accountIdToChiakiNumeric(b64);
    expect(result).toBe("18446744073709551615");
  });

  it("strips whitespace from input", () => {
    expect(accountIdToChiakiNumeric("  ABCDABCD \n ")).toBe(
      accountIdToChiakiNumeric("ABCDABCD"),
    );
  });
});
