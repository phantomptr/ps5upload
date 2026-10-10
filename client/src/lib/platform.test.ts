import { afterEach, describe, expect, it, vi } from "vitest";

import {
  isAndroid,
  isIOS,
  isMacOrLinuxDesktop,
  isMobile,
  isWindows,
} from "./platform";

function setUA(userAgent: string, extra: Record<string, unknown> = {}) {
  vi.stubGlobal("navigator", { userAgent, ...extra });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("isAndroid", () => {
  it("detects an Android WebView user agent", () => {
    setUA(
      "Mozilla/5.0 (Linux; Android 16; Pixel 9 Pro XL; wv) AppleWebKit/537.36",
    );
    expect(isAndroid()).toBe(true);
  });

  it("is false on desktop", () => {
    setUA("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36");
    expect(isAndroid()).toBe(false);
  });

  it("is false when navigator is unavailable (node/SSR)", () => {
    vi.stubGlobal("navigator", undefined);
    expect(isAndroid()).toBe(false);
  });
});

describe("isMacOrLinuxDesktop", () => {
  it("is true on macOS and Linux desktops", () => {
    setUA("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36", {
      maxTouchPoints: 0,
    });
    expect(isMacOrLinuxDesktop()).toBe(true);
    setUA("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36");
    expect(isMacOrLinuxDesktop()).toBe(true);
  });

  it("is false on Windows and Android", () => {
    setUA("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/120.0");
    expect(isWindows()).toBe(true);
    expect(isMacOrLinuxDesktop()).toBe(false);
    setUA("Mozilla/5.0 (Linux; Android 16; Pixel 9 Pro XL; wv) AppleWebKit/537.36");
    expect(isMacOrLinuxDesktop()).toBe(false);
  });
});

describe("isIOS", () => {
  it("detects an iPhone", () => {
    setUA("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)");
    expect(isIOS()).toBe(true);
  });

  it("is false on plain desktop Safari (no touch points)", () => {
    setUA("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", {
      maxTouchPoints: 0,
    });
    expect(isIOS()).toBe(false);
  });
});

describe("isMobile", () => {
  it("is true for Android and false for desktop", () => {
    setUA("Mozilla/5.0 (Linux; Android 16) AppleWebKit/537.36");
    expect(isMobile()).toBe(true);
    setUA("Mozilla/5.0 (Windows NT 10.0; Win64; x64)");
    expect(isMobile()).toBe(false);
  });
});
