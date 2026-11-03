import { describe, expect, it } from "vitest";

import type { CollectionGame, CollectionLocation } from "../api/collection";
import {
  addOnCount,
  consoleCounts,
  consoleFilterOf,
  extraCopies,
  filterCounts,
  formatCollectionBytes,
  locationKind,
  viewGames,
  type CollectionQuery,
} from "./collectionView";

function loc(type: string, kind?: string, path = "x"): CollectionLocation {
  return {
    root: "/r",
    container: "",
    name: path,
    type,
    path,
    absolute_path: `/r/${path}`,
    size_bytes: 1,
    added_ts: 0,
    pkg: kind
      ? { kind, content_id: "UP9000-CUSA00900_00-BLOODBORNE000000" }
      : undefined,
  };
}

function game(
  id: string,
  title: string,
  over: Partial<CollectionGame> = {},
): CollectionGame {
  return {
    game_id: id,
    title,
    platform: id.startsWith("PP") ? "PS5" : "PS4",
    sources: [],
    locations: [loc("pkg", "base")],
    total_size_bytes: 10,
    copies: 1,
    is_duplicate: false,
    added_ts: 0,
    ...over,
  };
}

const q = (over: Partial<CollectionQuery> = {}): CollectionQuery => ({
  search: "",
  filter: "all",
  platform: "all",
  sort: "title-asc",
  ...over,
});

const games = [
  game("CUSA00900", "Bloodborne", {
    added_ts: 5,
    total_size_bytes: 35,
    locations: [loc("pkg", "base"), loc("pkg", "patch"), loc("pkg", "dlc")],
  }),
  game("PPSA01234", "Astro", {
    added_ts: 9,
    total_size_bytes: 5,
    is_duplicate: true,
    locations: [loc("folder"), loc("mount.exfat")],
  }),
  game("PPSA05555", "zeta", {
    added_ts: 1,
    total_size_bytes: 50,
    locations: [loc("zip", undefined, "games/zeta.zip")],
  }),
];

describe("viewing the collection", () => {
  it("sorts every way PS Game Library does", () => {
    expect(viewGames(games, q()).map((g) => g.title)).toEqual([
      "Astro",
      "Bloodborne",
      "zeta",
    ]);
    expect(viewGames(games, q({ sort: "added-desc" }))[0].title).toBe("Astro");
    expect(viewGames(games, q({ sort: "size-desc" }))[0].title).toBe("zeta");
    expect(viewGames(games, q({ sort: "locations-desc" }))[0].title).toBe(
      "Bloodborne",
    );
    expect(viewGames(games, q({ sort: "id-desc" }))[0].game_id).toBe(
      "PPSA05555",
    );
  });

  it("filters by what a copy is, by duplicates and by platform", () => {
    expect(
      viewGames(games, q({ filter: "mount" })).map((g) => g.game_id),
    ).toEqual(["PPSA01234"]);
    expect(viewGames(games, q({ filter: "duplicates" }))).toHaveLength(1);
    expect(
      viewGames(games, q({ platform: "PS4" })).map((g) => g.game_id),
    ).toEqual(["CUSA00900"]);
    expect(filterCounts(games)).toMatchObject({
      all: 3,
      pkg: 1,
      mount: 1,
      folder: 1,
      zip: 1,
      duplicates: 1,
    });
  });

  it("searches titles, IDs, content IDs and paths", () => {
    expect(viewGames(games, q({ search: "blood" }))).toHaveLength(1);
    expect(viewGames(games, q({ search: "ppsa012" }))).toHaveLength(1);
    expect(viewGames(games, q({ search: "BLOODBORNE000000" }))).toHaveLength(1);
    expect(viewGames(games, q({ search: "games/zeta" }))).toHaveLength(1);
  });

  it("counts add-ons and names what a location is", () => {
    expect(addOnCount(games[0])).toBe(2);
    expect(locationKind("mount.ffpkg")).toBe("Game image (.ffpkg)");
    expect(locationKind("rar")).toBe("RAR archive");
    expect(formatCollectionBytes(1536)).toBe("1.5 KB");
    expect(formatCollectionBytes(0)).toBe("0 B");
  });
});

describe("the console overlay", () => {
  const st = (over: Partial<import("../api/collection").GameConsoleState>) => ({
    game_id: "x",
    installed: false,
    dlc_missing: [],
    non_package_copy: false,
    ...over,
  });
  const offer = {
    path: "/p",
    name: "p",
    version: "1.09",
    content_id: "c",
    title: "",
    size_bytes: 1,
    category: "gp",
  };
  const states = new Map([
    [
      "CUSA00900",
      st({
        game_id: "CUSA00900",
        installed: true,
        installed_version: "1.03",
        update: offer,
      }),
    ],
    [
      "PPSA01234",
      st({
        game_id: "PPSA01234",
        installed: true,
        dlc_missing: [{ ...offer, category: "ac" }],
      }),
    ],
    ["PPSA05555", st({ game_id: "PPSA05555" })],
  ]);

  it("filters games the console lacks, or has an update or DLC here to install", () => {
    const ids = (c: "missing" | "addons") =>
      viewGames(games, q({ console: c, consoleStates: states })).map(
        (g) => g.game_id,
      );
    expect(ids("missing")).toEqual(["PPSA05555"]);
    expect(ids("addons").sort()).toEqual(["CUSA00900", "PPSA01234"]);
    expect(consoleCounts(games, states)).toEqual({
      installed: 2,
      missing: 1,
      addons: 2,
      update: 1,
      dlc: 1,
      firmware: 0,
      drives: 0,
      unplayed: 2,
    });
  });

  it("reads old filter names as the merged one", () => {
    expect(consoleFilterOf("update")).toBe("addons");
    expect(consoleFilterOf("dlc")).toBe("addons");
    expect(consoleFilterOf("installed")).toBe("installed");
    expect(consoleFilterOf("bogus")).toBeNull();
    expect(consoleFilterOf(null)).toBeNull();
  });

  it("does not filter at all without a console", () => {
    expect(viewGames(games, q({ console: "missing" }))).toHaveLength(3);
  });

  it("finds games with files on the PS5's drives, and installed games never seen played", () => {
    const driveIds = new Set(["PPSA05555"]);
    const playSeconds = (id: string) => (id === "CUSA00900" ? 600 : undefined);
    const ids = (c: "drives" | "unplayed") =>
      viewGames(
        games,
        q({ console: c, consoleStates: states, driveIds, playSeconds }),
      ).map((g) => g.game_id);
    expect(ids("drives")).toEqual(["PPSA05555"]);
    expect(ids("unplayed")).toEqual(["PPSA01234"]);
    const n = consoleCounts(games, states, null, { driveIds, playSeconds });
    expect(n.drives).toBe(1);
    expect(n.unplayed).toBe(1);
  });
});

describe("freeing a duplicate's space", () => {
  it("offers every full copy but the largest, and never an add-on", () => {
    const big = { ...loc("pkg", "base", "big.pkg"), size_bytes: 30 };
    const small = {
      ...loc("mount.exfat", undefined, "small.exfat"),
      size_bytes: 20,
    };
    const patch = { ...loc("pkg", "patch", "patch.pkg"), size_bytes: 99 };
    const g = game("CUSA00900", "Bloodborne", {
      locations: [small, patch, big],
    });
    expect(extraCopies(g).map((l) => l.path)).toEqual(["small.exfat"]);
    expect(extraCopies(game("CUSA00001", "One"))).toEqual([]);
  });

  it("never offers a package of another release under the same Game ID (#638)", () => {
    const withCid = (l: CollectionLocation, cid: string, size: number) => ({
      ...l,
      size_bytes: size,
      pkg: { ...l.pkg, content_id: cid },
    });
    const gameA = withCid(
      loc("pkg", "base", "game.pkg"),
      "EP0700-CUSA12345_00-GAME000000000000",
      50,
    );
    const odd = withCid(
      loc("pkg", "base", "unlock.pkg"),
      "EP0700-CUSA12345_00-UNLOCKKEY0000003",
      1,
    );
    const dlc = withCid(
      loc("pkg", "dlc", "dlc.pkg"),
      "EP0700-CUSA12345_00-SEASONPASS000001",
      2,
    );
    const g = game("CUSA12345", "Game", { locations: [gameA, odd, dlc] });
    expect(extraCopies(g)).toEqual([]);
    const again = withCid(
      loc("pkg", "base", "again.pkg"),
      "ep0700-cusa12345_00-game000000000000",
      40,
    );
    const rar = { ...loc("rar", undefined, "g.rar"), size_bytes: 30 };
    const g2 = game("CUSA12345", "Game", {
      locations: [gameA, odd, dlc, again, rar],
    });
    expect(extraCopies(g2).map((l) => l.path)).toEqual(["again.pkg", "g.rar"]);
  });
});
