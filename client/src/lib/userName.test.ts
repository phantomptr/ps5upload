import { describe, expect, it } from "vitest";

import { USER_NAME_MAX_BYTES, userNameBytes, userNameTooLong } from "./userName";

describe("a PS5 user name's length (#940)", () => {
  it("is 16 bytes, the registry field less its end mark", () => {
    expect(USER_NAME_MAX_BYTES).toBe(16);
    expect(userNameTooLong("david")).toBe(false);
    expect(userNameTooLong("0123456789abcdef")).toBe(false);
    expect(userNameTooLong("0123456789abcdefg")).toBe(true);
  });

  it("counts bytes, not letters", () => {
    expect(userNameBytes("é")).toBe(2);
    expect(userNameBytes("日")).toBe(3);
    expect(userNameBytes("😀")).toBe(4);
    // 8 letters, 16 bytes: fits. 6 letters, 18 bytes: does not, though an input of 16 would take it.
    expect(userNameTooLong("éééééééé")).toBe(false);
    expect(userNameTooLong("日本語日本語")).toBe(true);
    expect("日本語日本語".length).toBeLessThanOrEqual(16);
  });
});
