import { describe, it, expect } from "vitest";
import {
  pngNameForJxr,
  joinDir,
  isJxrScreenshot,
} from "./screenshotConvert";

describe("isJxrScreenshot", () => {
  it("true only for .jxr (incl. case + doubled suffix)", () => {
    expect(isJxrScreenshot("shot.jxr")).toBe(true);
    expect(isJxrScreenshot("shot.JXR")).toBe(true);
    expect(isJxrScreenshot("shot.jxr.jxr")).toBe(true);
  });
  it("false for SDR formats the WebView renders directly", () => {
    // Regression: these were jxr-decoded and failed with
    // "not a JPEG XR file" on Preview.
    expect(isJxrScreenshot("20260610_220512_00839286.jpg")).toBe(false);
    expect(isJxrScreenshot("shot.jpeg")).toBe(false);
    expect(isJxrScreenshot("shot.png")).toBe(false);
    expect(isJxrScreenshot("noext")).toBe(false);
  });
});

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
