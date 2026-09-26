import { describe, expect, it } from "vitest";

import { nextTheme, type Theme } from "./theme";

/**
 * The toggle button steps a 4-theme cycle (PS5 Dark → PS5 Light → OLED →
 * Rose → back). Rose was added later and the wrap-around is easy to break, so
 * lock the full order + the wrap here.
 */
describe("nextTheme (theme cycle)", () => {
  it("cycles dark → light → oled → rose → dark", () => {
    const seen: Theme[] = [];
    let t: Theme = "dark";
    for (let i = 0; i < 5; i++) {
      seen.push(t);
      t = nextTheme(t);
    }
    expect(seen).toEqual(["dark", "light", "oled", "rose", "dark"]);
  });

  it("wraps from the last theme back to the first", () => {
    expect(nextTheme("rose")).toBe("dark");
  });

  it("restarts the cycle for an unknown current theme", () => {
    // indexOf → -1, (-1 + 1) % 4 === 0 → "dark"
    expect(nextTheme("garbage" as Theme)).toBe("dark");
  });
});
