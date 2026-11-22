import { describe, expect, it, vi } from "vitest";
import { streamPackages } from "./streamPackages";

describe("streamPackages", () => {
  const pkgs = [
    { path: "/p/dlc.fpkg", category: "ac" },
    { path: "/p/update.fpkg", category: "gp", size: 10 },
    { path: "/p/game.fpkg", category: "gd", contentId: "UP0000-CUSA00000_00-X", size: 100 },
  ];

  it("Add to queue queues every package as a stream install without starting, game first", () => {
    const install = vi.fn(async (_s: string, _h: string, _o: object) => ({ ok: true }));
    streamPackages(install, "10.0.0.2", pkgs, false);
    expect(install.mock.calls.map((c) => c[0])).toEqual([
      "/p/game.fpkg",
      "/p/update.fpkg",
      "/p/dlc.fpkg",
    ]);
    expect(install.mock.calls[0]).toEqual([
      "/p/game.fpkg",
      "10.0.0.2",
      { category: "gd", contentId: "UP0000-CUSA00000_00-X", sizeBytes: 100, start: false },
    ]);
    for (const c of install.mock.calls) expect(c[2]).toMatchObject({ start: false });
  });

  it("Stream & install starts as any add does (no start flag)", () => {
    const install = vi.fn(async (_s: string, _h: string, _o: object) => ({ ok: true }));
    streamPackages(install, "10.0.0.2", pkgs, true);
    expect(install).toHaveBeenCalledTimes(3);
    for (const c of install.mock.calls) expect(c[2]).not.toHaveProperty("start");
  });
});
