import { describe, expect, it } from "vitest";
import { normalizeTitleId } from "./titleId";

describe("normalizeTitleId", () => {
  it("expands the bare id that installed-game listings produce", () => {
    // The regression: clicking a game in the picker sent 9 characters
    // and the console rejected it as malformed.
    expect(normalizeTitleId("CUSA12345")).toBe("CUSA12345_00");
    expect(normalizeTitleId("PPSA01234")).toBe("PPSA01234_00");
  });

  it("upper-cases and trims what the user typed", () => {
    expect(normalizeTitleId("  cusa12345  ")).toBe("CUSA12345_00");
  });

  it("leaves an already-full title id alone", () => {
    expect(normalizeTitleId("CUSA12345_00")).toBe("CUSA12345_00");
    // A non-default suffix must survive — _01 is a different release.
    expect(normalizeTitleId("CUSA12345_01")).toBe("CUSA12345_01");
  });

  it("leaves a content id alone", () => {
    expect(normalizeTitleId("UP9000-CUSA12345_00-LABEL00123456789")).toBe(
      "UP9000-CUSA12345_00-LABEL00123456789",
    );
  });

  it("does not invent a suffix for input it does not recognise", () => {
    // Guessing here would turn a typo into a lookup for a real but
    // different game, so unrecognised input goes to the console as-is
    // and gets a proper error.
    for (const bad of ["", "CUSA1234", "CUSA123456", "CUS112345", "CUSA1234A"]) {
      expect(normalizeTitleId(bad)).toBe(bad.trim().toUpperCase());
    }
  });
});
