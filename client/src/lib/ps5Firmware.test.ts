import { describe, it, expect } from "vitest";
import {
  fakeGameLaunchHint,
  fakePackageNeedsLatestKstuff,
  parsePS5Firmware,
  ps5FakeGameNeedsNewKstuff,
} from "./ps5Firmware";

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
  it("fakePackageNeedsLatestKstuff reads the firmware alone", () => {
    expect(fakePackageNeedsLatestKstuff(K("13.40"))).toBe("13.40");
    expect(fakePackageNeedsLatestKstuff(K("11.60"))).toBeNull();
    expect(fakePackageNeedsLatestKstuff(K("13.61"))).toBeNull();
    expect(fakePackageNeedsLatestKstuff(null)).toBeNull();
  });
  it("fakeGameLaunchHint says the fake-package rule, the CE code and both ways", () => {
    const hint = fakeGameLaunchHint(K("13.40"), PS5_GAME);
    expect(hint).toContain("on firmware 11.61 to 13.60 (this PS5 has 13.40) a fake package needs the latest kstuff");
    expect(hint).toContain("with an older kstuff it installs but won't start (the PS5 shows CE-100022-5)");
    expect(hint).toContain("Kraken Mount");
    expect(hint).not.toMatch(/instead|better|recommended|alternative|fallback|1\.1\d/i);
    expect(fakeGameLaunchHint(K("11.50"), PS5_GAME)).toBeNull();
    expect(fakeGameLaunchHint(K("13.40"), "CUSA00207")).toBeNull();
  });
});
