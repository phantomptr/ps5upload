import { describe, expect, it } from "vitest";
import { userDeleteRefusal } from "./userDelete";

describe("userDeleteRefusal", () => {
  it("finds the helper's word inside the engine's message", () => {
    expect(userDeleteRefusal(new Error("user delete failed: user_signed_in"))).toBe("signed_in");
    expect(userDeleteRefusal("Delete user: user delete failed: user_state_unknown")).toBe(
      "state_unknown",
    );
  });

  it("leaves any other failure as it is", () => {
    expect(userDeleteRefusal(new Error("user delete failed: sceUserServiceDestroyUser failed"))).toBe(
      null,
    );
    expect(userDeleteRefusal(new Error("connection refused"))).toBe(null);
  });
});
