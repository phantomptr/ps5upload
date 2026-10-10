import { describe, expect, it } from "vitest";

import { greetingPart } from "./greeting";

describe("greetingPart", () => {
  it("says morning from 5 to noon", () => {
    expect(greetingPart(5)).toBe("morning");
    expect(greetingPart(11)).toBe("morning");
  });

  it("says afternoon from noon to six", () => {
    expect(greetingPart(12)).toBe("afternoon");
    expect(greetingPart(17)).toBe("afternoon");
  });

  it("says evening after six and through the night", () => {
    expect(greetingPart(18)).toBe("evening");
    expect(greetingPart(23)).toBe("evening");
    expect(greetingPart(0)).toBe("evening");
    expect(greetingPart(4)).toBe("evening");
  });
});
