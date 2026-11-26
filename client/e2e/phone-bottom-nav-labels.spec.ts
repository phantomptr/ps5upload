import { expect, test } from "@playwright/test";

import { routeEngine, seedConsole } from "./fixtures/engine";

// On a 360 px phone the six bottom tabs share 324 px: "Console" was cut to "Cons…", on every
// screen. Each English label fits whole, also in the bolder letters of the tab that is open.

test.use({ viewport: { width: 360, height: 800 }, hasTouch: true, isMobile: true });

for (const route of ["/home", "/console"]) {
  test(`every bottom tab shows its whole name on ${route}`, async ({ page }) => {
    await seedConsole(page);
    await routeEngine(page, () => undefined);
    await page.goto(route, { waitUntil: "domcontentloaded" });
    const nav = page.locator("[data-bottom-nav]");
    await expect(nav).toBeVisible({ timeout: 60_000 });
    const labels = nav.locator("a > span.truncate");
    await expect(labels).toHaveCount(6);
    const cut = () =>
      labels.evaluateAll((els) => els.filter((el) => el.scrollWidth > el.clientWidth).map((el) => el.textContent));
    expect(await cut()).toEqual([]);
    // A wide font, as on Linux and some Android phones.
    await page.addStyleTag({ content: "*{font-family:Verdana,'DejaVu Sans',sans-serif!important}" });
    expect(await cut(), "wide font").toEqual([]);
    // The bar itself stays inside the screen.
    const box = (await nav.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(360);
  });
}
