import { describe, expect, it } from "vitest";

import { nextTheme, normalizeTheme } from "./theme";

describe("nextTheme (two modes)", () => {
  it("flips between light and dark", () => {
    expect(nextTheme("dark")).toBe("light");
    expect(nextTheme("light")).toBe("dark");
  });
});

describe("normalizeTheme (stored values from older versions)", () => {
  it("keeps the two current modes", () => {
    expect(normalizeTheme("dark")).toBe("dark");
    expect(normalizeTheme("light")).toBe("light");
  });

  it("maps OLED to dark and Rose to light", () => {
    expect(normalizeTheme("oled")).toBe("dark");
    expect(normalizeTheme("rose")).toBe("light");
  });

  it("returns null for anything else", () => {
    expect(normalizeTheme(null)).toBeNull();
    expect(normalizeTheme("garbage")).toBeNull();
    expect(normalizeTheme(3)).toBeNull();
  });
});
