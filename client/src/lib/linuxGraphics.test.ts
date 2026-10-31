import { afterEach, describe, expect, it, vi } from "vitest";

import { linuxCompositingOff } from "./linuxGraphics";

/** The page's window with the flag the Linux shell's init script sets (or not). */
const withFlag = (flag?: unknown) =>
  vi.stubGlobal(
    "window",
    flag === undefined ? {} : { __PS5UPLOAD_LINUX_GRAPHICS__: flag },
  );

describe("linuxCompositingOff", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("is off without the shell's flag (other platforms, the browser)", () => {
    withFlag();
    expect(linuxCompositingOff()).toBe(false);
  });

  it("follows the flag the Linux shell sets at launch", () => {
    withFlag({ compositingOff: true });
    expect(linuxCompositingOff()).toBe(true);
    withFlag({ compositingOff: false });
    expect(linuxCompositingOff()).toBe(false);
  });

  it("ignores a malformed flag", () => {
    withFlag({ compositingOff: "yes" });
    expect(linuxCompositingOff()).toBe(false);
  });
});
