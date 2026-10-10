import { describe, expect, it } from "vitest";

import { coverVersion } from "./coverVersion";

const loc = (absolute_path: string, size_bytes = 10, modified_at = "2026-10-01") => ({
  absolute_path,
  size_bytes,
  modified_at,
});

describe("coverVersion", () => {
  it("stays the same across rescans of an unchanged game", () => {
    const g = { local_cover: "PPSA01341.png", locations: [loc("/g/a.pkg"), loc("/g/b.pkg")] };
    const again = { local_cover: "PPSA01341.png", locations: [loc("/g/b.pkg"), loc("/g/a.pkg")] };
    expect(coverVersion(g)).toBe(coverVersion(again));
  });

  it("changes when the files the cover comes from change", () => {
    const g = { local_cover: "PPSA01341.png", locations: [loc("/g/a.pkg")] };
    expect(coverVersion(g)).not.toBe(coverVersion({ ...g, locations: [loc("/g/a.pkg", 11)] }));
    expect(coverVersion(g)).not.toBe(coverVersion({ ...g, locations: [loc("/g/a.pkg", 10, "2026-10-02")] }));
    expect(coverVersion(g)).not.toBe(coverVersion({ ...g, locations: [loc("/g/c.pkg")] }));
  });

  it("has no version without a local cover", () => {
    expect(coverVersion({ locations: [loc("/g/a.pkg")] })).toBeUndefined();
  });
});
