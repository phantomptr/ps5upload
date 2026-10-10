import { expect, test, type Page } from "@playwright/test";

// One game page: opened from the Collection, it shows every saved console (with how old that
// knowledge is) and every copy on the drives; Back returns to the Collection; an install on a
// console other than the connected one shows on its row and Open queue goes to its Activity.

const ID = "PPSA01234";
const loc = (name: string, kind: string, version: string) => ({
  root: "/games",
  container: "/games",
  name,
  type: "pkg",
  path: name,
  absolute_path: `/games/${name}`,
  size_bytes: 1024 * 1024 * 1024,
  added_ts: 1,
  pkg: { kind, version, content_id: `UP0000-${ID}_00-ASTRO0000000000`, complete: true, title: "Astro" },
});
const offer = (name: string, category: string, version: string) => ({
  path: `/games/${name}`,
  name,
  version,
  content_id: `UP0000-${ID}_00-ASTRO0000000000`,
  title: "Astro",
  size_bytes: 1024,
  category,
});
const game = {
  game_id: ID,
  title: "Astro",
  platform: "PS5",
  sources: [],
  locations: [loc("astro.pkg", "base", "01.000"), loc("astro-patch.pkg", "patch", "01.004")],
  total_size_bytes: 2 * 1024 * 1024 * 1024,
  copies: 1,
  is_duplicate: false,
  added_ts: 1,
};

async function fakeEngine(page: Page) {
  const now = Math.floor(Date.now() / 1000);
  const view = {
    title_id: ID,
    title: "Astro",
    platform: "PS5",
    cover: null,
    copies: game.locations,
    consoles: [
      {
        host: "192.168.1.99",
        read_at: now - 2 * 3600,
        installed: true,
        version: "01.002",
        update: offer("astro-patch.pkg", "gp", "01.004"),
        dlc_missing: [],
      },
    ],
  };
  await page.addInitScript(() => {
    window.localStorage.setItem("ps5upload.host", "192.168.1.100");
    window.localStorage.setItem(
      "ps5upload.roster.v1",
      JSON.stringify({
        profiles: [
          { id: "pro", name: "Pro", host: "192.168.1.100" },
          { id: "phat", name: "Phat", host: "192.168.1.99" },
        ],
        active_id: "pro",
      }),
    );
  });
  await page.route(
    (u) => u.pathname.startsWith("/api/"),
    (route) => {
      const path = new URL(route.request().url()).pathname;
      const json = (body: unknown, status = 200) =>
        route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) });
      if (path === "/api/collection/library")
        return json({ summary: { total_games: 1, total_locations: 2, total_size_bytes: 1, duplicates_count: 0, reclaimable_bytes: 0 }, games: { [ID]: game } });
      if (path === "/api/collection/settings") return json({ roots: ["/games"], refresh_secs: null, sweep_sidecars: false });
      if (path === "/api/collection/scan") return json({ running: false, phase: "idle", done: 0, total: 0 });
      if (path === `/api/games/${ID}`) return json(view);
      return json({ error: "down" }, 503);
    },
  );
}

test("a game opened from the Collection shows every console and every copy, and Back returns", async ({ page }) => {
  await fakeEngine(page);
  await page.goto("/collection", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: /Astro/ }).first().click({ timeout: 30_000 });
  await expect(page).toHaveURL(new RegExp(`/games/${ID}$`));

  const consoles = page.getByTestId("game-consoles");
  await expect(consoles).toBeVisible({ timeout: 30_000 });
  const phat = page.getByTestId("game-console-192.168.1.99");
  await expect(phat).toContainText("Installed · v01.002");
  await expect(phat).toContainText("as of 2 h ago");
  await expect(phat.getByRole("button", { name: "Install update 01.004" })).toBeVisible();
  // The Pro has never been read for this game.
  await expect(page.getByTestId("game-console-192.168.1.100")).toContainText("Not checked yet");
  await expect(page.getByTestId("game-copies").locator("li")).toHaveCount(2);
  await expect(page.getByTestId("game-summary")).toContainText("Installed on Phat (01.002)");

  await page.getByTestId("game-back").click();
  await expect(page).toHaveURL(/\/collection$/);
});

test("installing on a console that is not the connected one shows on its row and opens its activity", async ({ page }) => {
  await fakeEngine(page);
  await page.goto(`/games/${ID}`, { waitUntil: "domcontentloaded" });
  const phat = page.getByTestId("game-console-192.168.1.99");
  await phat.getByRole("button", { name: "Install update 01.004" }).click({ timeout: 30_000 });
  // The engine is down here, so the item may fail; either way the row says what happened to it
  // and links to that console's queue.
  const open = phat.getByRole("button", { name: "Open", exact: true });
  await expect(open).toBeVisible({ timeout: 30_000 });
  await open.click();
  await expect(page).toHaveURL(/\/tasks\?console=192\.168\.1\.99$/);
  await expect(page.getByTestId("activity-console")).toContainText("Showing Phat");
});

test("Collection keeps the Games tab lit on a phone", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await fakeEngine(page);
  await page.goto("/collection", { waitUntil: "domcontentloaded" });
  const bottom = page.getByRole("navigation", { name: "Primary" }).last();
  await expect(bottom.getByRole("link", { name: /^Games/ })).toHaveAttribute("aria-current", "page", {
    timeout: 30_000,
  });
});
