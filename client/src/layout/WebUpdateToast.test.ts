import { describe, expect, it } from "vitest";
import { shouldShowWebUpdate } from "./WebUpdateToast";

describe("shouldShowWebUpdate", () => {
  const u = { current: "10.1.0", latest: "10.2.0", available: true, docker: true };

  it("shows a newer release until that version is dismissed", () => {
    expect(shouldShowWebUpdate(u, null)).toBe(true);
    expect(shouldShowWebUpdate(u, "10.2.0")).toBe(false);
    expect(shouldShowWebUpdate(u, "10.1.5")).toBe(true);
  });

  it("shows nothing without an update", () => {
    expect(shouldShowWebUpdate(null, null)).toBe(false);
    expect(shouldShowWebUpdate({ ...u, available: false }, null)).toBe(false);
  });
});
