import { describe, expect, it } from "vitest";

import { confirmInitialFocus } from "./ConfirmDialog";

describe("where a confirm dialog opens", () => {
  it("on the affirmative button, so Enter confirms, unless the action is destructive", () => {
    expect(confirmInitialFocus(undefined)).toBe("confirm");
    expect(confirmInitialFocus(false)).toBe("confirm");
  });
  it("on Cancel for a destructive action: a held Enter must not delete", () => {
    expect(confirmInitialFocus(true)).toBe("cancel");
  });
});
