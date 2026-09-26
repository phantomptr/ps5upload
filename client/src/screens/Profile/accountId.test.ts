import { describe, expect, it } from "vitest";
import { parseAccountId, formatAccountId, accountIdToB64 } from "./index";

describe("parseAccountId", () => {
  it("accepts hex with or without the 0x prefix, any case", () => {
    expect(parseAccountId("0x1a2b")).toBe("0x1a2b");
    expect(parseAccountId("1A2B")).toBe("0x1a2b");
    expect(parseAccountId("  0XffFF  ")).toBe("0xffff");
  });

  it("accepts a full 64-bit id without losing a digit", () => {
    // Returned as a string, never a number or a BigInt: numbers lose
    // precision above 2^53, and BigInt cannot be down-levelled for the
    // build target and would break the bundle on old WebViews.
    expect(parseAccountId("0x0123456789abcdef")).toBe("0x123456789abcdef");
    expect(parseAccountId("ffffffffffffffff")).toBe("0xffffffffffffffff");
  });

  it("rejects zero", () => {
    // Zero means "no account". Clearing a slot is a separate, explicit action,
    // so writing zero through the id field would be a confusing way to do it.
    expect(parseAccountId("0")).toBeNull();
    expect(parseAccountId("0x0")).toBeNull();
    expect(parseAccountId("0000000000000000")).toBeNull();
  });

  it("rejects anything that is not 1-16 hex digits", () => {
    expect(parseAccountId("")).toBeNull();
    expect(parseAccountId("   ")).toBeNull();
    expect(parseAccountId("xyz")).toBeNull();
    expect(parseAccountId("0x")).toBeNull();
    // 17 digits would silently truncate to a different id.
    expect(parseAccountId("0x00123456789abcdef")).toBeNull();
    expect(parseAccountId("12 34")).toBeNull();
    expect(parseAccountId("-1")).toBeNull();
  });
});

describe("formatAccountId", () => {
  it("renders a decimal id from the API as 0x hex", () => {
    // The API sends the id as a decimal string; the console and every other
    // tool talk about it in hex, so displaying decimal would be unreadable.
    expect(formatAccountId("6789")).toBe("0x1a85");
    expect(formatAccountId("81985529216486895")).toBe("0x123456789abcdef");
  });

  it("shows a dash for no account", () => {
    // A slot with no id is a normal state, not an error.
    expect(formatAccountId("0")).toBe("—");
    expect(formatAccountId("")).toBe("—");
    expect(formatAccountId(null)).toBe("—");
    expect(formatAccountId(undefined)).toBe("—");
  });

  it("never throws on a value it cannot read", () => {
    expect(formatAccountId("not-a-number")).toBe("—");
  });
});

describe("64-bit exactness", () => {
  // The whole reason these are string-to-string: a full-width id must survive
  // a round trip with every digit intact. Number() would round it and
  // activate a different account; BigInt would break the bundle on old
  // WebViews. Both failures are silent, so pin the behaviour.
  it("round-trips a full-width id through parse and format", () => {
    const decimal = "18446744073709551615"; // 0xffffffffffffffff
    expect(formatAccountId(decimal)).toBe("0xffffffffffffffff");
    expect(parseAccountId("0xffffffffffffffff")).toBe("0xffffffffffffffff");
  });

  it("keeps digits a JS number would have rounded away", () => {
    // 2^53 + 1 — the first integer a double cannot represent.
    expect(formatAccountId("9007199254740993")).toBe("0x20000000000001");
    expect(parseAccountId("0x20000000000001")).toBe("0x20000000000001");
  });
});

describe("the wire format the payload actually sends", () => {
  // runtime.c emits "\"id\":\"0x%016llx\"", and these tests previously only
  // ever fed formatAccountId decimal — so a fixture disagreeing with the
  // wire let the screen render "—" for every console with an account.
  const WIRE = "0x7a356e99a9e2205c";

  it("formats the 0x-hex the payload sends", () => {
    expect(formatAccountId(WIRE)).toBe("0x7a356e99a9e2205c");
  });

  it("still formats decimal, which older builds sent", () => {
    expect(formatAccountId("8806066252652093532")).toBe("0x7a356e99a9e2205c");
  });

  it("strips leading zeros from a zero-padded wire value", () => {
    expect(formatAccountId("0x000000000000beef")).toBe("0xbeef");
  });

  it("rejects a value that is neither hex nor decimal", () => {
    expect(formatAccountId("0xnothex")).toBe("—");
    expect(formatAccountId("0x")).toBe("—");
    expect(formatAccountId("0x00000000000000000")).toBe("—"); // 17 digits
  });
});

describe("accountIdToB64", () => {
  // Ground truth: both consoles report account_id_b64 "XCDiqZluNXo=" for
  // the account whose id reads 0x7a356e99a9e2205c. Bytes are emitted low
  // byte first because offact reads them straight into a uint64_t on a
  // little-endian CPU.
  it("matches what the console reports for the same account", () => {
    expect(accountIdToB64("0x7a356e99a9e2205c")).toBe("XCDiqZluNXo=");
    expect(accountIdToB64("8806066252652093532")).toBe("XCDiqZluNXo=");
  });

  it("always decodes to exactly 8 bytes, which pairing requires", () => {
    for (const id of [
      "0x1",
      "0xbeef",
      "0xffffffffffffffff",
      "0x7a356e99a9e2205c",
    ]) {
      expect(atob(accountIdToB64(id))).toHaveLength(8);
    }
  });

  it("zero-pads a short id rather than shifting the bytes", () => {
    expect(accountIdToB64("0x1")).toBe(
      btoa("\x01\x00\x00\x00\x00\x00\x00\x00"),
    );
  });

  it("returns empty for no id, zero, or junk", () => {
    expect(accountIdToB64(null)).toBe("");
    expect(accountIdToB64("")).toBe("");
    expect(accountIdToB64("0x0")).toBe("");
    expect(accountIdToB64("nope")).toBe("");
  });

  it("does not round a 64-bit id the way a Number would", () => {
    // 8806066252652093532 is above 2^53; via Number it becomes ...094000,
    // which is 8 valid bytes of a DIFFERENT account.
    const viaNumber = accountIdToB64(String(Number("8806066252652093532")));
    expect(viaNumber).not.toBe("XCDiqZluNXo=");
    expect(accountIdToB64("8806066252652093532")).toBe("XCDiqZluNXo=");
  });
});
