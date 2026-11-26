import { describe, expect, it, vi } from "vitest";

import { findInPlace, localParent, placeParent, placeTransferPath, serverParent, type PlacePage } from "./placeList";

describe("place paths", () => {
  it("a server's or a console's parent", () => {
    expect(serverParent("/games/ps5")).toBe("/games");
    expect(serverParent("/games")).toBe("/");
    expect(serverParent("/")).toBeNull();
  });

  it("this computer's parent, with / or \\", () => {
    expect(localParent("/Users/me/Games")).toBe("/Users/me");
    expect(localParent("/Users")).toBe("/");
    expect(localParent("/")).toBeNull();
    expect(localParent("C:\\Users\\me")).toBe("C:\\Users");
    expect(localParent("C:\\Users")).toBe("C:\\");
    expect(localParent("C:\\")).toBeNull();
  });

  it("picks the rule by place, and a server's folder travels as remote://", () => {
    expect(placeParent({ kind: "computer" }, "C:\\Games")).toBe("C:\\");
    expect(placeParent({ kind: "console", host: "h" }, "/data/x")).toBe("/data");
    expect(placeTransferPath({ kind: "server", id: "nas" }, "/games")).toBe("remote://nas/games");
    expect(placeTransferPath({ kind: "computer" }, "/Users/me")).toBe("/Users/me");
  });
});

describe("findInPlace", () => {
  it("reads page after page until the name turns up", async () => {
    const pages: Record<string, PlacePage> = {
      start: { entries: [{ name: "a", path: "/d/a", is_dir: false, size: 1 }], more: "p2" },
      p2: { entries: [{ name: "b", path: "/d/b", is_dir: true, size: 0 }], more: null },
    };
    const list = vi.fn(async (_p: unknown, _d: string, more?: string | null) => pages[more ?? "start"]);
    const hit = await findInPlace({ kind: "server", id: "nas" }, "/d", "b", list as never);
    expect(hit?.is_dir).toBe(true);
    expect(list).toHaveBeenCalledTimes(2);
    expect(await findInPlace({ kind: "server", id: "nas" }, "/d", "zzz", list as never)).toBeNull();
  });
});
