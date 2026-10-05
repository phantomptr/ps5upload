import { describe, it, expect } from "vitest";
import { parsePS5Firmware, firmwareMajor, ps5FakeGameUnplayableFirmware } from "./ps5Firmware";

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

describe("firmwareMajor (Stream Install FW gate)", () => {
  it("returns the integer major below the FW-11 cutoff (stream BLOCKED)", () => {
    expect(
      firmwareMajor("FreeBSD 11.0 r218215/releases/09.60 Jul 18 2023")
    ).toBe(9);
    expect(firmwareMajor("r/releases/05.00")).toBe(5);
    expect(firmwareMajor("r/releases/10.40")).toBe(10);
    // Major extraction is descriptive only; Stream support is not gated by it.
    expect(firmwareMajor("r/releases/09.60")! < 11).toBe(true);
    expect(firmwareMajor("r/releases/10.40")! < 11).toBe(true);
  });

  it("returns >= 11 at and above the cutoff (stream ALLOWED with advisory)", () => {
    expect(firmwareMajor("r/releases/11.00")).toBe(11);
    expect(firmwareMajor("r/releases/12.40")).toBe(12);
    // Newer major versions parse the same way.
    expect(firmwareMajor("r/releases/12.40")! >= 11).toBe(true);
    expect(firmwareMajor("r/releases/09.60")! >= 11).toBe(false);
  });

  it("returns null when the firmware can't be parsed (gate does NOT block)", () => {
    expect(firmwareMajor(null)).toBeNull();
    expect(firmwareMajor("")).toBeNull();
    expect(firmwareMajor("unknown build")).toBeNull();
  });
});

describe("ps5FakeGameUnplayableFirmware", () => {
  const K = (fw: string) => `FreeBSD 11.0-RELEASE-p0 #1 r229358/releases/${fw} Jul 17 2026`;
  const PS5_GAME = "UP4433-PPSA17221_00-MINECRAFTPS50000";
  const PS4_GAME = "UP9000-CUSA00207_00-BLOODBORNE000000";
  const HOMEBREW = "IV0002-ITEM00001_00-ITEMZFLOWIV00000";

  it("warns for a PS5 game above 11.60 (11.61, 12.xx, 13.60)", () => {
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), PS5_GAME)).toBe("13.60");
    expect(ps5FakeGameUnplayableFirmware(K("11.61"), PS5_GAME)).toBe("11.61");
    expect(ps5FakeGameUnplayableFirmware(K("12.00"), PS5_GAME)).toBe("12.00");
  });
  it("is silent at 11.60 and below", () => {
    expect(ps5FakeGameUnplayableFirmware(K("11.60"), PS5_GAME)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("09.60"), PS5_GAME)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("05.10"), PS5_GAME)).toBeNull();
  });
  it("never warns for a PS4 package, even on 13.60", () => {
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), PS4_GAME)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), "CUSA00207")).toBeNull();
  });
  it("never warns for PS5 homebrew (Itemzflow), even on 13.60", () => {
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), HOMEBREW)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), "ITEM00001")).toBeNull();
  });
  it("accepts a bare PPSA title id", () => {
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), "PPSA17221")).toBe("13.60");
  });
  it("never warns when the firmware or the id is unknown", () => {
    expect(ps5FakeGameUnplayableFirmware(null, PS5_GAME)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware("unknown build", PS5_GAME)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), null)).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), "")).toBeNull();
    expect(ps5FakeGameUnplayableFirmware(K("13.60"), "something-else")).toBeNull();
  });
});
