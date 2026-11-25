import { expect, test } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// Games on a 360 px phone, two cards a row: the line under a game's title ("1 copy · 25.61 GB")
// shared its width with the card's ⋯ button and read "1… · 25.61 GB". The ⋯ now sits over the end
// of the title's line, and the line under it has the card's whole width.

test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

const GAMES: Array<[string, string, number]> = [
  ["PPSA90001", "Frostline", 27.5e9],
  ["CUSA90007", "Ember Knights", 19.3e9],
];

test("a card's line under the title is not cut, and the ⋯ still opens its menu", async ({ page }) => {
  const games: Record<string, unknown> = {};
  for (const [id, title, size] of GAMES) {
    const file = `${title.replace(/ /g, "")}.pkg`;
    games[id] = {
      game_id: id, title, platform: id.startsWith("PPSA") ? "PS5" : "PS4", sources: ["/games"],
      locations: [{
        root: "/games", container: "", name: file, type: "pkg", path: file, absolute_path: `/games/${file}`, size_bytes: size, added_ts: 1,
        pkg: { platform: "PS5", title, title_id: id, version: "01.000", kind: "base" },
      }],
      total_size_bytes: size, copies: 1, is_duplicate: false, added_ts: 1,
    };
  }
  await seedConsole(page);
  await routeEngine(page, (path) => {
    if (path === "/api/version") return { version: "12.6.31", caps: {} };
    if (path === "/api/collection/library")
      return {
        roots: ["/games"], missing_roots: [], generated_at: "2026-10-30T10:00:00Z",
        summary: { total_games: 2, total_locations: 2, total_size_bytes: 46.8e9, duplicates_count: 0, reclaimable_bytes: 0 },
        games,
      };
    if (path === "/api/collection/settings") return { roots: ["/games"], refresh_secs: null, sweep_sidecars: false, trash_available: true };
    if (path === "/api/collection/scan") return { running: false, deep: false, found: 2, done: 2, started_ms: 0, finished_ms: 0, error: null, games: 2, locations: 2 };
    return undefined;
  });
  await page.goto("/games", { waitUntil: "domcontentloaded" });
  const captions = page.locator("[data-scroll-root]:visible").getByTestId("collection-card-caption");
  await expect(captions).toHaveCount(2, { timeout: 60_000 });
  for (const caption of await captions.all()) {
    const copies = caption.getByText("1 copy", { exact: true });
    await expect(copies).toBeVisible();
    expect(await copies.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    // The ⋯ is inside the card, at the end of the title's line.
    const more = caption.getByRole("button", { name: /^More actions/ });
    const box = (await more.boundingBox())!;
    const card = (await caption.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(card.x + card.width + 1);
    expect(box.y).toBeGreaterThanOrEqual(card.y - 1);
  }
  await captions.first().getByRole("button", { name: /^More actions/ }).click();
  await expect(page.getByRole("menu")).toBeVisible();
});
