import { afterEach, describe, expect, it, vi } from "vitest";

import { dropIsOwnedElsewhere, OWN_DROP_ATTR, physicalPointInRect } from "./dropZone";

const rect = { left: 100, right: 200, top: 50, bottom: 80 };

describe("physicalPointInRect", () => {
  it("scales Tauri's physical pixels to CSS pixels", () => {
    expect(physicalPointInRect({ x: 300, y: 120 }, rect, 2)).toBe(true);
    expect(physicalPointInRect({ x: 300, y: 120 }, rect, 1)).toBe(false);
    expect(physicalPointInRect({ x: 150, y: 60 }, rect, 0)).toBe(true);
  });
});

describe("dropIsOwnedElsewhere", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("is true only over an element that handles its own drops", () => {
    const zone = { getBoundingClientRect: () => rect };
    const querySelectorAll = vi.fn(() => [zone]);
    vi.stubGlobal("document", { querySelectorAll });
    vi.stubGlobal("window", { devicePixelRatio: 1 });
    expect(dropIsOwnedElsewhere({ x: 150, y: 60 })).toBe(true);
    expect(dropIsOwnedElsewhere({ x: 10, y: 10 })).toBe(false);
    expect(querySelectorAll).toHaveBeenCalledWith(`[${OWN_DROP_ATTR}]`);
  });
});
