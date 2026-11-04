import { expect, test } from "@playwright/test";
import { routeEngine } from "./fixtures/engine";

// An installed game's card on the Games grid has Play, always shown (a phone has no hover):
// pressing it launches the game without opening its page, and once the game runs the same
// control closes it. A game the PS5 lacks has none.

const status = {
  command_count: 1,
  instance_id: 1,
  prior_instance: "clean",
  ps5_kernel: "r229358/releases/13.60",
  ucred_elevated: true,
  version: "9.1.0",
};

for (const width of [1280, 390]) {
  test(`Play on a game's card starts it in place (${width}px)`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript(() =>
      window.localStorage.setItem("ps5upload.host", "192.168.1.100"),
    );
    const launched: string[] = [];
    await routeEngine(page, (path, req) => {
      let body: unknown = {};
      if (path === "/api/version") body = { version: "9.1.0", caps: {} };
      if (path === "/api/ps5/status") body = status;
      if (path === "/api/ps5/apps/installed")
        body = {
          titles: [
            { title_id: "PPSA01234", title_name: "Astro Bot", origin: "pkg" },
          ],
        };
      if (path === "/api/ps5/process/list")
        body = {
          truncated: false,
          processes: launched.length
            ? [
                {
                  pid: 77,
                  name: "eboot",
                  comm: "eboot.bin",
                  title_id: "PPSA01234",
                  app_id: 5,
                  memory_mib: 1,
                  threads: 1,
                  kind: "app",
                },
              ]
            : [],
        };
      if (path === "/api/ps5/app/launch") {
        launched.push(
          (req.postDataJSON() as { title_id: string }).title_id,
        );
        body = { ok: true };
      }
      return body;
    });
    await page.goto("/games", { waitUntil: "domcontentloaded" });
    const card = page
      .getByTestId("collection-card")
      .filter({ hasText: "Astro Bot" });
    const play = card.getByTestId("card-play");
    await expect(play).toBeVisible({ timeout: 30_000 });
    await expect(play).toHaveAttribute("data-state", "play");
    await expect(play).toHaveAccessibleName("Play: Astro Bot");

    // Inside the card, and the page has no sideways scroll.
    const [cardBox, playBox] = [
      await card.boundingBox(),
      await play.boundingBox(),
    ];
    expect(playBox!.x + playBox!.width).toBeLessThanOrEqual(
      cardBox!.x + cardBox!.width,
    );
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);

    await play.click();
    await expect.poll(() => launched).toEqual(["PPSA01234"]);
    await expect(page).toHaveURL(/\/games$/);
    await expect(play).toHaveAttribute("data-state", "running", {
      timeout: 15_000,
    });
    await expect(play).toHaveAccessibleName("Close game: Astro Bot");
  });
}
