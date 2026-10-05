import { describe, expect, it } from "vitest";

import { activityForHost } from "./activityScope";

describe("activityForHost", () => {
  const entries = [
    { id: "a", addr: "192.168.86.99:9120" },
    { id: "b", addr: "192.168.86.100" },
    { id: "c", addr: undefined },
  ];

  it("shows the selected console's work and local-only work", () => {
    expect(activityForHost(entries, "192.168.86.100").map((e) => e.id)).toEqual(["b", "c"]);
    expect(activityForHost(entries, "192.168.86.99").map((e) => e.id)).toEqual(["a", "c"]);
  });

  it("shows everything when no console is selected", () => {
    expect(activityForHost(entries, "").map((e) => e.id)).toEqual(["a", "b", "c"]);
  });
});
