import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { withoutNoCheatsError } from "./ps5";

describe("a title with no cheats", () => {
  it("is an empty list, not an error", () => {
    // The helper says "no cheat files found" when the title has none: the game page showed it
    // as a red error.
    expect(withoutNoCheatsError({ mods: [], error: "no cheat files found" })).toEqual({ mods: [] });
    expect(withoutNoCheatsError({ mods: [], error: "no cheat files found for PPSA23226" })).toEqual({ mods: [] });
  });
  it("keeps a real error", () => {
    expect(withoutNoCheatsError({ mods: [], error: "permission denied" })).toEqual({
      mods: [],
      error: "permission denied",
    });
  });
});
