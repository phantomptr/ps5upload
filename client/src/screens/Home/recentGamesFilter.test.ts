import { describe, expect, it } from "vitest";

import type { CollectionGame, GameConsoleState } from "../../api/collection";
import { recentGames } from "./recentGamesFilter";

const game = (game_id: string, added_ts: number): CollectionGame =>
  ({ game_id, title: game_id, platform: "PS5", added_ts, locations: [] }) as unknown as CollectionGame;
const has = (...ids: string[]): Record<string, GameConsoleState> =>
  Object.fromEntries(ids.map((id) => [id, { game_id: id, installed: true, dlc_missing: [], non_package_copy: false }]));

const games = [game("A", 1), game("B", 3), game("C", 2)];

describe("recentGames", () => {
  it("lists the newest first, up to the limit", () => {
    expect(recentGames(games, "all", {}).map((g) => g.game_id)).toEqual(["B", "C", "A"]);
    expect(recentGames(games, "all", {}, 2).map((g) => g.game_id)).toEqual(["B", "C"]);
  });

  it("narrows to what one console has", () => {
    const states = { "10.0.0.1": has("A"), "10.0.0.2": has("B", "C") };
    expect(recentGames(games, { host: "10.0.0.1" }, states).map((g) => g.game_id)).toEqual(["A"]);
    expect(recentGames(games, { host: "10.0.0.2" }, states).map((g) => g.game_id)).toEqual(["B", "C"]);
  });

  it("not installed means on none of the consoles that were read", () => {
    const states = { "10.0.0.1": has("A"), "10.0.0.2": has("C"), "10.0.0.3": undefined };
    expect(recentGames(games, "missing", states).map((g) => g.game_id)).toEqual(["B"]);
  });

  it("claims nothing is missing when no console was read", () => {
    expect(recentGames(games, "missing", {})).toEqual([]);
  });
});
