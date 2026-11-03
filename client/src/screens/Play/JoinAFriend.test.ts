import { describe, expect, it } from "vitest";
import { cameraRefused } from "./JoinAFriend";

describe("cameraRefused", () => {
  it("is a refused permission for getUserMedia's permission errors", () => {
    expect(cameraRefused(new DOMException("denied", "NotAllowedError"))).toBe(true);
    expect(cameraRefused(new DOMException("insecure", "SecurityError"))).toBe(true);
    expect(cameraRefused({ name: "PermissionDeniedError" })).toBe(true);
  });

  it("is not for a missing or busy camera, or anything else", () => {
    expect(cameraRefused(new DOMException("none", "NotFoundError"))).toBe(false);
    expect(cameraRefused(new DOMException("busy", "NotReadableError"))).toBe(false);
    expect(cameraRefused(new Error("boom"))).toBe(false);
    expect(cameraRefused(null)).toBe(false);
    expect(cameraRefused(undefined)).toBe(false);
  });
});
