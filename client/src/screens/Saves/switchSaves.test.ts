import { describe, expect, it } from "vitest";

import { profileHex } from "./SwitchSaves";

describe("profileHex", () => {
  it("shows a profile as the 7 hex digits the engine keeps of it", () => {
    expect(profileHex(0x1f1e1d1)).toBe("1F1E1D1");
    expect(profileHex(0xabc)).toBe("0000ABC");
    expect(profileHex(0)).toBe("0000000");
  });
});
