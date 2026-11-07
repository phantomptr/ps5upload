import { describe, expect, it } from "vitest";
import { railSide } from "./TabNav";

describe("railSide", () => {
  it("takes a column down one side", () => {
    expect(railSide(0, 93)).toBe("right");
    expect(railSide(93, 0)).toBe("left");
  });
  it("leaves a notch and an ordinary phone alone", () => {
    expect(railSide(0, 0)).toBeNull();
    expect(railSide(59, 59)).toBeNull();
    expect(railSide(0, 48)).toBeNull();
  });
});
