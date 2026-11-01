import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CollectionGame } from "../../api/collection";

// A server render reads a zustand store's initial state, so the Collection is a fixture.
const fixture = vi.hoisted(() => ({ library: null as unknown, consoleByHost: {} }));
vi.mock("../../state/collection", async (orig) => {
  const actual = await orig<typeof import("../../state/collection")>();
  const useCollectionStore = Object.assign(
    (sel: (s: typeof fixture) => unknown) => sel(fixture),
    { getState: () => fixture },
  );
  return { ...actual, useCollectionStore, loadCollection: vi.fn(), loadConsoleStates: vi.fn() };
});

vi.mock("../../state/lang", () => ({
  useTr:
    () =>
    (key: string, vars?: Record<string, string | number> | string, fallback?: string) =>
      typeof vars === "string" ? vars : (fallback ?? key),
}));

import { RecentGames } from "./RecentGames";

const game = (i: number) =>
  ({
    game_id: `PPSA9${String(i).padStart(4, "0")}`,
    title: `Game ${i}`,
    platform: "PS5",
    added_ts: i,
    total_size_bytes: 1e9,
    locations: [],
  }) as unknown as CollectionGame;

const withGames = (n: number) => {
  fixture.library = {
    games: Object.fromEntries(Array.from({ length: n }, (_, i) => [game(i).game_id, game(i)])),
  };
};

const html = () =>
  renderToStaticMarkup(
    <MemoryRouter>
      <RecentGames />
    </MemoryRouter>,
  );
const cards = (out: string) => out.match(/<li /g)?.length ?? 0;

afterEach(() => {
  fixture.library = null;
});

describe("RecentGames", () => {
  it("renders nothing for an empty Collection", () => {
    expect(html()).toBe("");
  });

  it("shows every game when there are only a few", () => {
    withGames(3);
    expect(cards(html())).toBe(3);
  });

  it("shows up to twelve, newest first, and links to the rest", () => {
    withGames(20);
    const out = html();
    expect(cards(out)).toBe(12);
    expect(out.indexOf("Game 19")).toBeLessThan(out.indexOf("Game 18"));
    expect(out).not.toContain("Game 7<");
    expect(out).toContain('href="/games?tab=all"');
  });

  it("sizes cards at a fixed width instead of a third of the row", () => {
    withGames(10);
    const out = html();
    expect(out).toContain("w-[7.75rem]");
    expect(out).toContain("sm:w-[8.5rem]");
    expect(out).not.toContain("(100%-2rem)/3");
  });

  it("has no arrows until the row is measured as overflowing", () => {
    // The arrows depend on a layout measurement; a first render (and a row that fits)
    // must not show controls that do nothing.
    withGames(10);
    expect(html()).not.toContain("home-recent-games-arrows");
  });
});
