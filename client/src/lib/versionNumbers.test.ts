import { describe, expect, it } from "vitest";
import { APPINFO_VERSION_KEYS, compareVersions, installedAppVersion, sameVersion } from "./versionNumbers";

const from = (rows: Record<string, string>) => (k: string) => rows[k];

describe("installedAppVersion", () => {
  it("asks for every key it reads", () => {
    expect(APPINFO_VERSION_KEYS.split(",")).toEqual(["CONTENT_VERSION", "VERSION", "APP_VER"]);
  });

  it("reads an updated PS4 game's version from VERSION (#934)", () => {
    // The reporter's replies: RDR2 and ELDEN RING keep APP_VER at the base game's 01.00.
    expect(installedAppVersion(from({ APP_VER: "01.00", VERSION: "01.32" }))).toBe("01.32");
    expect(installedAppVersion(from({ APP_VER: "01.00", VERSION: "01.26" }))).toBe("01.26");
  });

  it("prefers CONTENT_VERSION, then the higher of VERSION and APP_VER", () => {
    expect(installedAppVersion(from({ CONTENT_VERSION: "01.500.000" }))).toBe("01.500.000");
    expect(installedAppVersion(from({ CONTENT_VERSION: "01.000.016", APP_VER: "01.00", VERSION: "01.32" }))).toBe(
      "01.000.016",
    );
    expect(installedAppVersion(from({ VERSION: "01.00", APP_VER: "01.09" }))).toBe("01.09");
    expect(installedAppVersion(from({ APP_VER: "01.05" }))).toBe("01.05");
    expect(installedAppVersion(from({ VERSION: "01.26" }))).toBe("01.26");
    expect(installedAppVersion(from({ CONTENT_VERSION: "", VERSION: "", APP_VER: "01.00" }))).toBe("01.00");
    expect(installedAppVersion(from({}))).toBeNull();
  });
});

describe("compareVersions / sameVersion", () => {
  it("compare by numbers", () => {
    expect(compareVersions("01.32", "01.05")).toBeGreaterThan(0);
    expect(sameVersion("01.000.016", "1.0.16")).toBe(true);
    expect(sameVersion("01.32", null)).toBe(false);
  });
});
