import { describe, expect, it } from "vitest";

import { toLogInputs } from "./engineLogBridge";

describe("toLogInputs", () => {
  it("maps levels, strips the sidecar prefix and defaults unknown levels to info", () => {
    const out = toLogInputs([
      { seq: 1, level: "error", msg: "[engine:error] disk full" },
      { seq: 2, level: "warn", msg: "slow" },
      { seq: 3, level: "weird", msg: "[engine:weird] hi" },
    ] as never);
    expect(out).toEqual([
      { level: "error", source: "engine", message: "disk full" },
      { level: "warn", source: "engine", message: "slow" },
      { level: "info", source: "engine", message: "hi" },
    ]);
  });
});
