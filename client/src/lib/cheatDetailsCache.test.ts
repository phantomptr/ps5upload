import { describe, expect, it } from "vitest";

import type { AppInfoDetails } from "../api/ps5";
import { cacheDetails, cachedDetails } from "./cheatDetailsCache";

const details = { version: "01.000" } as unknown as AppInfoDetails;

describe("a cheat page's game details", () => {
  it("are kept for a few minutes", () => {
    cacheDetails("h/A", details, 1000);
    expect(cachedDetails("h/A", 2000)).toBe(details);
    expect(cachedDetails("h/A", 1000 + 6 * 60 * 1000)).toBeNull();
  });

  it("forget a failed read, so the next look asks again", () => {
    cacheDetails("h/B", details, 1000);
    cacheDetails("h/B", null, 2000);
    expect(cachedDetails("h/B", 3000)).toBeNull();
  });
});
