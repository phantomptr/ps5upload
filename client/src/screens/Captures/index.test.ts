import { describe, expect, it } from "vitest";

import { capturesTabOf } from "./index";

describe("capturesTabOf", () => {
  it("opens on screenshots, and on video clips when the address asks", () => {
    expect(capturesTabOf(new URLSearchParams(""))).toBe("screenshots");
    expect(capturesTabOf(new URLSearchParams("tab=videos"))).toBe("videos");
    expect(capturesTabOf(new URLSearchParams("tab=nonsense"))).toBe(
      "screenshots",
    );
  });
});
