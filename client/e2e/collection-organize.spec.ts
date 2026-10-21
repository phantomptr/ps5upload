import { expect, test } from "@playwright/test";

for (const hasMoves of [true, false]) {
  test(`collection organizer explains mixed sources and skipped items (${hasMoves ? "moves" : "no moves"})`, async ({ page }) => {
    const move = (from: string, to: string) => ({
      container: "/games", from, to, platform: "PS5", title_id: "PPSA01234", title: "Example",
      kind: "base", confident: true, reason: "", version: "1.00", region: "US", size: 10, content_id: "",
    });
    await page.route((url) => url.pathname.startsWith("/api/"), async (route) => {
      const path = new URL(route.request().url()).pathname;
      let body: unknown = {};
      if (path === "/api/version") body = { version: "7.1.10", caps: {} };
      if (path === "/api/collection/settings") body = { roots: ["/games"], refresh_secs: 0, sweep_sidecars: false };
      if (path === "/api/collection/library") body = { games: {}, summary: { total_games: 0, total_locations: 0, total_size_bytes: 0, duplicates_count: 0, reclaimable_bytes: 0 } };
      if (path === "/api/collection/scan") body = { running: false, done: 0, found: 0 };
      if (path === "/api/collection/organize/runs") body = [];
      if (path === "/api/collection/organize/plan") body = {
        token: "preview", containers: ["/games"], moves: hasMoves ? [
          move("game.ffpfsc", "PS5/PPSA01234 - Example/Example.ffpfsc"),
          move("dump", "PS5/PPSA01234 - Example/Example"),
        ] : [],
        skipped: [{ container: "/games", path: "broken.ffpfs", reason: "unreadable", detail: "invalid image" }],
        unsure_count: 0, total_bytes: hasMoves ? 20 : 0,
      };
      await route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
    });
    await page.goto("/collection", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "More actions", exact: true }).click();
    await page.getByRole("menuitem", { name: /Organize/ }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByRole("heading", { name: "Organize collection", exact: true })).toBeVisible();
    await expect(dialog.getByText(/extracted game folders/i)).toBeVisible();
    if (hasMoves) {
      await expect(dialog.getByText("game.ffpfsc", { exact: true })).toBeVisible();
      await expect(dialog.getByText("dump", { exact: true })).toBeVisible();
      await expect(dialog.getByRole("button", { name: "Organize 2 items", exact: true })).toBeVisible();
    } else {
      await expect(dialog.getByText("No moves planned.", { exact: true })).toBeVisible();
      await expect(dialog.getByText(/Every .*already/)).toHaveCount(0);
    }
    await dialog.getByText("Left alone (1)", { exact: true }).click();
    await expect(dialog.getByText("broken.ffpfs", { exact: true })).toBeVisible();
  });
}
