import { describe, expect, it } from "vitest";

import { deviceWord } from "./deviceName";

describe("deviceWord", () => {
  it("names the device by a word the computer translates", () => {
    expect(deviceWord("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)")).toBe("iphone");
    expect(deviceWord("Mozilla/5.0 (Linux; Android 15; Pixel 9)")).toBe("android");
    expect(deviceWord("Mozilla/5.0 (X11; Linux x86_64)")).toBe("phone");
  });

  it("tells an iPad, which says it is a Mac, from a Mac", () => {
    expect(deviceWord("Mozilla/5.0 (iPad; CPU OS 12_0 like Mac OS X)")).toBe("ipad");
    expect(deviceWord("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5)).toBe("ipad");
    expect(deviceWord("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0)).toBe("phone");
  });
});
