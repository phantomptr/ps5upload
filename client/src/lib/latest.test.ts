import { describe, expect, it } from "vitest";

import { createLatest } from "./latest";

describe("createLatest", () => {
  it("only the newest request is current, so an older answer arriving late is dropped", () => {
    const latest = createLatest();
    const a = latest.begin();
    const b = latest.begin();
    expect(latest.isCurrent(a)).toBe(false);
    expect(latest.isCurrent(b)).toBe(true);
  });

  it("can be invalidated outright", () => {
    const latest = createLatest();
    const a = latest.begin();
    latest.invalidate();
    expect(latest.isCurrent(a)).toBe(false);
  });
});
