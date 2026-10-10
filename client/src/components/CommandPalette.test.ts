import { afterEach, describe, expect, it, vi } from "vitest";

import { OPEN_COMMAND_PALETTE_EVENT, openCommandPalette } from "./CommandPalette";

describe("openCommandPalette", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("sends the event the palette listens for", () => {
    const target = new EventTarget();
    vi.stubGlobal("window", target);
    const seen: string[] = [];
    target.addEventListener(OPEN_COMMAND_PALETTE_EVENT, (e) => seen.push(e.type));
    openCommandPalette();
    expect(seen).toEqual([OPEN_COMMAND_PALETTE_EVENT]);
  });
});
