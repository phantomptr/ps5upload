import { describe, it, expect } from "vitest";
import { pngNameForJxr, joinDir } from "./screenshotConvert";

describe("pngNameForJxr", () => {
  it("strips a single .jxr", () => {
    expect(pngNameForJxr("20260531_232448_00673017.jxr")).toBe(
      "20260531_232448_00673017.png",
    );
  });

  it("collapses the doubled .jxr.jxr thumbnail suffix", () => {
    expect(pngNameForJxr("20260531_232448_00673017.jxr.jxr")).toBe(
      "20260531_232448_00673017.png",
    );
  });

  it("handles .jpg/.jpeg too", () => {
    expect(pngNameForJxr("shot.jpg")).toBe("shot.png");
    expect(pngNameForJxr("shot.jpeg")).toBe("shot.png");
  });

  it("is case-insensitive on the extension", () => {
    expect(pngNameForJxr("SHOT.JXR")).toBe("SHOT.png");
  });

  it("preserves dots inside the name that aren't image extensions", () => {
    expect(pngNameForJxr("my.cool.shot.jxr")).toBe("my.cool.shot.png");
  });

  it("appends .png when there's no known image extension", () => {
    expect(pngNameForJxr("weird")).toBe("weird.png");
  });
});

describe("joinDir", () => {
  it("joins with a forward slash", () => {
    expect(joinDir("/Users/me/shots", "a.png")).toBe("/Users/me/shots/a.png");
  });

  it("trims a trailing separator (posix or windows)", () => {
    expect(joinDir("/Users/me/shots/", "a.png")).toBe("/Users/me/shots/a.png");
    expect(joinDir("C:\\Users\\me\\shots\\", "a.png")).toBe(
      "C:\\Users\\me\\shots/a.png",
    );
  });
});
