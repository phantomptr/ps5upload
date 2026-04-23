import { describe, it, expect } from "vitest";
import { parsePS5Firmware } from "./ps5Firmware";

describe("parsePS5Firmware", () => {
  it("extracts from 'releases/09.60' kernel string", () => {
    expect(
      parsePS5Firmware(
        "FreeBSD 11.0-RELEASE-p0 #1 r218215/releases/09.60 Jul 18 2023"
      )
    ).toBe("9.60");
  });

  it("extracts from 'releases/10.00'", () => {
    expect(
      parsePS5Firmware("FreeBSD 11.0 r222222/releases/10.00-DEBUG")
    ).toBe("10.00");
  });

  it("strips leading zero from major", () => {
    expect(parsePS5Firmware("r/releases/05.00 foo")).toBe("5.00");
  });

  it("returns null for null/empty/unknown strings", () => {
    expect(parsePS5Firmware(null)).toBeNull();
    expect(parsePS5Firmware("")).toBeNull();
    expect(parsePS5Firmware("unknown")).toBeNull();
  });

  it("falls back to a bare NN.NN substring", () => {
    expect(parsePS5Firmware("kernel tag 9.00")).toBe("9.00");
  });

  it("prefers the releases/ match over a trailing date number", () => {
    // "releases/09.60" should win over the ".11" in "11.0"
    expect(
      parsePS5Firmware("FreeBSD 11.0 r218215/releases/09.60: Jul 18 2023")
    ).toBe("9.60");
  });
});
