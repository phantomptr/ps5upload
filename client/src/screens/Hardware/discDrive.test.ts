import { describe, expect, it } from "vitest";

import { discDriveOf } from "./discDrive";

describe("discDriveOf", () => {
  it("reads the edition letter", () => {
    expect(discDriveOf("CFI-1115A")).toBe("yes");
    expect(discDriveOf("CFI-2016A")).toBe("yes");
    expect(discDriveOf("CFI-1116B")).toBe("no");
  });

  it("treats the slim Digital Edition and the Pro as able to take the add-on drive", () => {
    expect(discDriveOf("CFI-2016B")).toBe("attachable");
    expect(discDriveOf("CFI-7019")).toBe("attachable");
  });

  it("does not guess when the model is missing or unfamiliar", () => {
    expect(discDriveOf(undefined)).toBe("unknown");
    expect(discDriveOf("")).toBe("unknown");
    expect(discDriveOf("PlayStation 5")).toBe("unknown");
  });
});
