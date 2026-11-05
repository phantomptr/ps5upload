import { afterEach, describe, expect, it } from "vitest";

import { pauseRpInput, rpInput } from "./remotePlay";

describe("holding the game's input back", () => {
  afterEach(() => {
    for (const o of ["settings", "passcode", "keyboard"] as const) pauseRpInput(o, false);
  });

  // With Settings open, the console's keyboard opening and closing handed the pad back to the game.
  it("stays held until every owner has let go", () => {
    expect(rpInput.paused).toBe(false);
    pauseRpInput("settings", true);
    pauseRpInput("keyboard", true);
    pauseRpInput("keyboard", false);
    expect(rpInput.paused).toBe(true);
    pauseRpInput("keyboard", false);
    expect(rpInput.paused).toBe(true);
    pauseRpInput("settings", false);
    expect(rpInput.paused).toBe(false);
  });
});
