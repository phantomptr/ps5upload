import { describe, expect, it } from "vitest";

import { runGate } from "./runGate";

describe("runGate", () => {
  it("lets one run in and asks for one more when another is wanted meanwhile", () => {
    const g = runGate();
    expect(g.enter("a")).toBe(true);
    // A Stop's read while the poll's read is in flight: not dropped.
    expect(g.enter("a")).toBe(false);
    expect(g.enter("a")).toBe(false);
    expect(g.leave("a")).toBe(true);
    // The one run after it, and nothing more owed.
    expect(g.enter("a")).toBe(true);
    expect(g.leave("a")).toBe(false);
  });

  it("keeps consoles apart", () => {
    const g = runGate();
    expect(g.enter("ps5-a")).toBe(true);
    expect(g.enter("ps5-b")).toBe(true);
    expect(g.leave("ps5-b")).toBe(false);
    expect(g.leave("ps5-a")).toBe(false);
  });
});
