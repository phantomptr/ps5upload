import { describe, expect, it } from "vitest";
import { parseAccountId, formatAccountId } from "./index";

describe("parseAccountId", () => {
  it("accepts hex with or without the 0x prefix, any case", () => {
    expect(parseAccountId("0x1a2b")).toBe(0x1a2bn);
    expect(parseAccountId("1A2B")).toBe(0x1a2bn);
    expect(parseAccountId("  0XffFF  ")).toBe(0xffffn);
  });

  it("accepts a full 64-bit id", () => {
    expect(parseAccountId("0x0123456789abcdef")).toBe(0x0123456789abcdefn);
    expect(parseAccountId("ffffffffffffffff")).toBe(0xffffffffffffffffn);
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
