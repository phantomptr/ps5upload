import { describe, expect, it } from "vitest";

import { speedVerdict } from "./speedVerdict";

const MB = 1024 * 1024;

describe("speedVerdict", () => {
  it("names the kind of link the slower direction looks like", () => {
    expect(speedVerdict(105 * MB, 98 * MB)).toBe("gigabit");
    expect(speedVerdict(40 * MB, 60 * MB)).toBe("fast_wifi_or_busy");
    expect(speedVerdict(11 * MB, 11.5 * MB)).toBe("hundred_mbit");
    expect(speedVerdict(3 * MB, 80 * MB)).toBe("slow");
  });

  it("has nothing to say without both figures", () => {
    expect(speedVerdict(null, 5 * MB)).toBeNull();
    expect(speedVerdict(0, 0)).toBeNull();
  });
});
