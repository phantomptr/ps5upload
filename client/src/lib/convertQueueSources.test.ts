import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { sourcesForPath, withoutBuilding } from "./convertQueueSources";

describe("what a path dropped during a build queues (#621 review)", () => {
  it("scans a dropped folder as Add games from a folder… does: one item per game", async () => {
    const scan = vi.fn(async () => ({
      sources: [
        { path: "/games/A-app", isDir: true },
        { path: "/games/B.pkg", isDir: false },
      ],
      skipped: 1,
    }));
    expect(await sourcesForPath("/games", scan)).toEqual({
      sources: [
        { path: "/games/A-app", isDir: true },
        { path: "/games/B.pkg", isDir: false },
      ],
      skipped: 1,
    });
    expect(scan).toHaveBeenCalledWith("/games");
  });

  it("takes an image, package or archive as itself, and a game on the PS5 as it is", async () => {
    const scan = vi.fn();
    expect((await sourcesForPath("/g/G.exfat", scan)).sources).toEqual([{ path: "/g/G.exfat", isDir: false }]);
    expect((await sourcesForPath("/g/G.part1.rar", scan)).sources).toEqual([{ path: "/g/G.part1.rar", isDir: false }]);
    expect((await sourcesForPath("ps5://10.0.0.2/data/G-app", scan)).sources).toEqual([
      { path: "ps5://10.0.0.2/data/G-app", isDir: true },
    ]);
    expect(scan).not.toHaveBeenCalled();
  });

  it("leaves out the game being made right now", () => {
    const sources = [
      { path: "/games/A-app/", isDir: true },
      { path: "/games/B-app", isDir: true },
    ];
    expect(withoutBuilding(sources, "/games/A-app")).toEqual({ take: [sources[1]], building: 1 });
    expect(withoutBuilding(sources, null)).toEqual({ take: sources, building: 0 });
  });
});
