import { describe, expect, it, vi } from "vitest";

vi.mock("../state/lang", () => ({ useTr: () => (k: string) => k }));

import { isRowActivationKey } from "./Table";

const cell = { getAttribute: (n: string) => (n === "role" ? "gridcell" : null) };
const control = { getAttribute: () => null };

describe("isRowActivationKey", () => {
  it("opens the row on Enter or Space on a cell", () => {
    expect(isRowActivationKey("Enter", cell, 1, 0)).toBe(true);
    expect(isRowActivationKey(" ", cell, 0, 0)).toBe(true);
  });
  it("leaves other keys alone", () => {
    expect(isRowActivationKey("ArrowDown", cell, 1, 0)).toBe(false);
    expect(isRowActivationKey("a", cell, 1, 0)).toBe(false);
  });
  it("leaves a control inside a cell alone", () => {
    expect(isRowActivationKey("Enter", control, 1, 0)).toBe(false);
  });
  it("leaves the selection column alone", () => {
    expect(isRowActivationKey(" ", cell, 0, 1)).toBe(false);
    expect(isRowActivationKey(" ", cell, 1, 1)).toBe(true);
  });
});
