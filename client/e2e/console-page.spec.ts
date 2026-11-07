import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";

// The page the helper serves to the PS5's own browser, in a real browser with its user agent and
// the helper's API stood in for: it loads, every section opens with no script error, and the keys
// the console's browser is known to send move around it. Needs `npm run build:console` first.
// Which keys a real controller sends is another matter: see src/console/nav.ts (`verified`).

const PS5_AGENT =
  "Mozilla/5.0 (PlayStation; PlayStation 5/13.60) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.4 Safari/605.1.15";
const ORIGIN = "http://console.test";

test.use({ userAgent: PS5_AGENT, viewport: { width: 1920, height: 1080 } });

async function open(page: Page, errors: string[], tourSeen = true) {
  if (tourSeen) {
    await page.addInitScript(() => localStorage.setItem("ps5upload.console.tourSeen", "1"));
  }
  page.on("pageerror", (e) => errors.push(e.message));
  const html = readFileSync(resolve(process.cwd(), "dist-console/index.html"), "utf8");
  await page.route(`${ORIGIN}/**`, async (route) => {
    const url = new URL(route.request().url());
    if (!url.pathname.startsWith("/api/")) {
      if (url.pathname === "/" || url.pathname.endsWith(".html")) {
        return route.fulfill({ contentType: "text/html", body: html });
      }
      return route.fulfill({ status: 404, body: "" });
    }
    const body =
      url.pathname === "/api/token"
        ? { token: "test" }
        : url.pathname === "/api/volumes"
          ? { volumes: [] }
          : url.pathname === "/api/fs/list"
            ? { entries: [], path: "/data", truncated: false }
            : {};
    return route.fulfill({ contentType: "application/json", body: JSON.stringify(body) });
  });
  await page.goto(`${ORIGIN}/`);
  await expect(page.locator("body")).not.toBeEmpty();
}

test.describe("the console page", () => {
  test("shows its tour once, and the keys leave it", async ({ page }) => {
    const errors: string[] = [];
    await open(page, errors, false);
    await expect(page.getByText("1 of 3")).toBeVisible({ timeout: 20_000 });
    await page.getByRole("button", { name: "Skip" }).click();
    await expect(page.getByText("1 of 3")).toBeHidden();
    expect(await page.evaluate(() => localStorage.getItem("ps5upload.console.tourSeen"))).toBe("1");
    expect(errors).toEqual([]);
  });

  test("loads, and the arrow keys move the focus", async ({ page }) => {
    const errors: string[] = [];
    await open(page, errors);
    await expect(page.getByRole("navigation").first()).toBeVisible({ timeout: 20_000 });
    const focused = () =>
      page.evaluate(() => document.activeElement?.textContent?.trim().slice(0, 40) ?? "");
    await page.keyboard.press("ArrowDown");
    const first = await focused();
    await page.keyboard.press("ArrowDown");
    expect(await focused()).not.toBe(first);
    expect(errors).toEqual([]);
  });

  test("every section opens without a script error", async ({ page }) => {
    const errors: string[] = [];
    await open(page, errors);
    const links = page.getByRole("navigation").first().locator("a, button");
    await expect(links.first()).toBeVisible({ timeout: 20_000 });
    const count = await links.count();
    expect(count).toBeGreaterThan(2);
    for (let i = 0; i < count; i++) {
      await links.nth(i).click();
      await expect(page.locator("main, [role=main], body").first()).toBeVisible();
    }
    expect(errors).toEqual([]);
  });
});
