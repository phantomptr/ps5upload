import { describe, it, expect } from "vitest";
import { parsePS5Firmware, ps5FakeGameNeedsNewKstuff } from "./ps5Firmware";

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

describe("ps5FakeGameNeedsNewKstuff", () => {
  const K = (fw: string) => `FreeBSD 11.0-RELEASE-p0 #1 r229358/releases/${fw} Jul 17 2026`;
  const PS5_GAME = "UP4433-PPSA17221_00-MINECRAFTPS50000";
  it("names 11.61 to 13.60, where a fake PS5 game needs kstuff 1.15 or newer", () => {
    expect(ps5FakeGameNeedsNewKstuff(K("11.61"), PS5_GAME)).toBe("11.61");
    expect(ps5FakeGameNeedsNewKstuff(K("12.70"), PS5_GAME)).toBe("12.70");
    expect(ps5FakeGameNeedsNewKstuff(K("13.60"), "PPSA17221")).toBe("13.60");
  });
  it("is null at 11.60 and below, above 13.60, for PS4 or homebrew, and for an unknown firmware", () => {
    expect(ps5FakeGameNeedsNewKstuff(K("11.60"), PS5_GAME)).toBeNull();
    expect(ps5FakeGameNeedsNewKstuff(K("14.00"), PS5_GAME)).toBeNull();
    expect(ps5FakeGameNeedsNewKstuff(K("13.60"), "CUSA00207")).toBeNull();
    expect(ps5FakeGameNeedsNewKstuff(K("13.60"), "IV0002-ITEM00001_00-ITEMZFLOWIV00000")).toBeNull();
    expect(ps5FakeGameNeedsNewKstuff(null, PS5_GAME)).toBeNull();
  });
});
