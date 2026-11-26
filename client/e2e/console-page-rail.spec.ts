import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "@playwright/test";

// The helper's own page at the sizes it is opened at, the helper's API stood in for. Needs
// `npm run build:console` first. The type follows the width, so a wide screen is short in rem: in
// the PS5 browser (1540x700) and on a 16:9 screen the rail ran past the bottom, Settings and Help
// out of sight until it was scrolled.

const PS5_AGENT =
  "Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.4 Safari/605.1.15";
const ORIGIN = "http://console.test";

test.use({ userAgent: PS5_AGENT });

for (const size of [
  { width: 1540, height: 700 },
  { width: 1920, height: 1080 },
  { width: 1280, height: 800 },
  { width: 1280, height: 1024 },
]) {
  test(`every section and Help are on screen at ${size.width}x${size.height}`, async ({ page }) => {
    const errors: string[] = [];
    await page.setViewportSize(size);
    await page.addInitScript(() => localStorage.setItem("ps5upload.console.tourSeen", "1"));
    page.on("pageerror", (e) => errors.push(e.message));
    const html = readFileSync(resolve(process.cwd(), "dist-console/index.html"), "utf8");
    await page.route(`${ORIGIN}/**`, async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === "/" || url.pathname.endsWith(".html")) return route.fulfill({ contentType: "text/html", body: html });
      if (!url.pathname.startsWith("/api/")) return route.fulfill({ status: 404, body: "" });
      return route.fulfill({ contentType: "application/json", body: JSON.stringify(url.pathname === "/api/token" ? { token: "t" } : {}) });
    });
    await page.goto(`${ORIGIN}/`);
    const rail = page.locator(".cw-rail");
    await expect(rail).toBeVisible({ timeout: 20_000 });
    const fit = await rail.evaluate((el) => ({ scroll: el.scrollHeight, client: el.clientHeight }));
    expect(fit.scroll).toBeLessThanOrEqual(fit.client);
    await expect(page.locator('[data-action="help"]')).toBeInViewport({ ratio: 1 });
    await expect(page.locator('[data-rail-id="settings"]')).toBeInViewport({ ratio: 1 });
    // The focused section's name is said in the hint bar (the rail may show icons only).
    await page.locator('[data-rail-id="settings"]').focus();
    await expect(page.locator(".cw-hint-tip")).toHaveText("Settings");
    expect(errors).toEqual([]);
  });
}
